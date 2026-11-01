//! Property tests for the chunker: arbitrary bodies must not panic, and no
//! generated document may split a fence, a table or a list item (§6.5, §10).
//!
//! These complement the table-driven unit tests in `src/chunk/split.rs`: the
//! table says which boundary was *chosen*, the property says that no boundary
//! can ever cut an atomic block, whatever the input looks like.

use janus_mem::chunk::{BlockKind, chunk_body, sections};
use janus_mem::config::ChunkingConfig;
use janus_mem::id::DocId;
use proptest::prelude::*;

fn document() -> DocId {
    DocId::from_match_key(b"chunk-property-test")
}

/// Every invariant a chunk must satisfy against the body it came from.
fn check(body: &str, max_chars: usize, overlap: f64) -> Result<(), TestCaseError> {
    let config = ChunkingConfig { max_chars, overlap };
    let (chunks, notes) = chunk_body(document(), body, &config);
    let limit = max_chars.max(1);
    let oversized = notes.iter().any(|note| note.code() == "oversized_block");

    let mut previous_start = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        prop_assert_eq!(chunk.ordinal, index as u16, "ordinals are contiguous");
        let (start, end) = (chunk.span.0 as usize, chunk.span.1 as usize);
        prop_assert!(start < end, "empty span {start}..{end}");
        prop_assert!(end <= body.len(), "span {start}..{end} outside the body");
        prop_assert!(body.is_char_boundary(start) && body.is_char_boundary(end));

        let text = chunk.text(body);
        prop_assert!(!text.trim().is_empty(), "whitespace-only chunk");
        prop_assert_eq!(text, &body[start..end], "text is the span's slice");

        if index > 0 {
            prop_assert!(start > previous_start, "chunks must make progress");
        }
        previous_start = start;

        // The chunker counts characters without the CR of a CRLF pair (a
        // carriage return is line-ending noise, not content), so the limit is
        // checked the same way here.
        let chars = text.chars().filter(|character| *character != '\r').count();
        prop_assert!(
            chars <= limit || oversized,
            "chunk of {chars} chars exceeds {limit} with no oversized_block note"
        );
    }

    // A chunk either contains an atomic block (or list item) whole, or does not
    // touch it. This is the "no fence is ever split" rule in general form.
    for section in sections(body) {
        for block in &section.blocks {
            let atoms: Vec<_> = if block.kind == BlockKind::List {
                block.items.clone()
            } else if block.kind == BlockKind::Paragraph || block.kind == BlockKind::Rule {
                Vec::new()
            } else {
                vec![block.span.clone()]
            };
            for atom in atoms {
                // Compare the atom's *content*: a chunk is trimmed of edge
                // whitespace, so its span may sit strictly inside an atom that
                // begins or ends with a newline without cutting any text.
                let atom = trim(body, atom);
                if atom.start >= atom.end {
                    continue;
                }
                for chunk in &chunks {
                    let start = chunk.span.0 as usize;
                    let end = chunk.span.1 as usize;
                    let overlaps = start < atom.end && atom.start < end;
                    if overlaps {
                        prop_assert!(
                            start <= atom.start && end >= atom.end,
                            "chunk {start}..{end} cuts atomic block {:?} for {:?}",
                            atom,
                            block.kind
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

/// Trims a span to its non-whitespace content, mirroring the chunker.
fn trim(body: &str, span: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let text = &body[span.clone()];
    let start = span.start + (text.len() - text.trim_start().len());
    let end = span.start + text.trim_end().len();
    start..end
}

/// Bodies of arbitrary Unicode, including control characters, lone CRs and NUL.
fn arbitrary_body() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..300)
        .prop_map(|characters| characters.into_iter().collect())
}

/// Document-shaped text: the fragments that exercise each block kind.
fn markdown_body() -> impl Strategy<Value = String> {
    let fragment = prop_oneof![
        Just("# Heading\n\nA sentence with a few words.".to_owned()),
        Just("## Sub\n\n- item one\n- item two\n  - nested item\n- item three".to_owned()),
        Just("```rust\nfn main() { println!(\"hi\"); }\n```".to_owned()),
        Just("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |".to_owned()),
        Just("> a quoted paragraph\n> over two lines".to_owned()),
        Just("Plain paragraph. Two sentences! And a question?".to_owned()),
        Just("---".to_owned()),
        Just("    indented code\n    second line".to_owned()),
        Just("~~~\ntilde fence\n~~~".to_owned()),
        Just("\n\n".to_owned()),
        Just("Setext\n======".to_owned()),
    ];
    prop::collection::vec(fragment, 0..8).prop_map(|fragments| fragments.join("\n\n"))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_unicode_bodies_chunk_safely(
        body in arbitrary_body(),
        max_chars in 1..120usize,
        overlap in 0.0..0.5f64,
    ) {
        check(&body, max_chars, overlap)?;
    }

    #[test]
    fn generated_markdown_never_splits_an_atomic_block(
        body in markdown_body(),
        max_chars in 1..200usize,
        overlap in 0.0..0.5f64,
    ) {
        check(&body, max_chars, overlap)?;
    }

    #[test]
    fn crlf_bodies_produce_the_same_chunk_ids(body in markdown_body()) {
        let lf = chunk_body(document(), &body, &ChunkingConfig { max_chars: 80, overlap: 0.15 }).0;
        let crlf_body = body.replace('\n', "\r\n");
        let crlf = chunk_body(document(), &crlf_body, &ChunkingConfig { max_chars: 80, overlap: 0.15 }).0;
        let ids: Vec<_> = lf.iter().map(|chunk| chunk.id).collect();
        let crlf_ids: Vec<_> = crlf.iter().map(|chunk| chunk.id).collect();
        prop_assert_eq!(ids, crlf_ids);
    }

    #[test]
    fn chunking_is_deterministic(
        body in markdown_body(),
        max_chars in 1..200usize,
        overlap in 0.0..0.5f64,
    ) {
        let config = ChunkingConfig { max_chars, overlap };
        let first = chunk_body(document(), &body, &config).0;
        let second = chunk_body(document(), &body, &config).0;
        prop_assert_eq!(first, second);
    }
}
