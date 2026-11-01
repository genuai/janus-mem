//! Sections become chunks: the size limit, overlap and content-addressed ids
//! (§6.5 rules 2–5).
//!
//! The splitter never re-parses text. It walks the [`super::blocks::BlockSpan`]s
//! produced by [`super::blocks`], turns each into a list of *atoms* — the smallest units a
//! boundary may fall between — and greedily packs atoms into chunks. Atoms are:
//!
//! * a whole block for anything atomic (code, table, quote, HTML, rule);
//! * a list **item** for a list (a boundary between items keeps every marker);
//! * a sentence, then a word, then a character for a paragraph — the last two
//!   only when a sentence or word alone exceeds the limit.
//!
//! Two properties fall out of that shape and are what the tests pin down:
//!
//! * **Progress.** Every chunk contains at least one atom, and overlap never
//!   re-includes the chunk's own first atom, so the loop always advances.
//! * **Fidelity.** A chunk either contains an atomic block entirely or does not
//!   touch it. No fence, table or list item is ever cut in half.
//!
//! Chunk ids are content-addressed (§6.4, D24): the same heading path and the
//! same normalized text produce the same id in every document, which is what
//! makes the M4 embedding cache free across edits, moves and copies. The
//! document id and the document title deliberately do **not** participate
//! (CH-5): a title defaults to the filename stem, so including it would re-key
//! every chunk on a rename.

use std::borrow::Cow;
use std::ops::Range;

use crate::config::ChunkingConfig;
use crate::document::DocNote;
use crate::id::{ChunkId, DocId};

use super::blocks::{BlockKind, Section, sections};

/// Ceiling on [`Chunk::ordinal`], which §6.5 fixes at `u16`.
const ORDINAL_MAX: u16 = u16::MAX;

/// Separator between breadcrumb components and the chunk text in the id hash.
///
/// NUL cannot occur in a Markdown heading and is vanishingly rare in a body, so
/// it is the same unambiguous separator M1's identity ladder uses (S-3).
const ID_SEPARATOR: u8 = 0;

/// One retrievable unit of a document (§6.5).
///
/// Text is **not** stored: the chunk is a span into [`crate::document::Document::body`]
/// and the index persists spans only (§8.3 rule 1). Retrieve the source text with
/// [`Chunk::text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Content-addressed identity; see the module docs and §6.4.
    pub id: ChunkId,
    /// The document this chunk belongs to.
    pub doc_id: DocId,
    /// Ancestor headings, outermost first, excluding the document title.
    ///
    /// Empty for content before the first heading.
    pub heading_path: Vec<String>,
    /// Byte range within the document body.
    ///
    /// A tuple with `u32` bounds because §6.5 fixes the persisted shape; the
    /// conversion is checked once per document ([`DocNote::BodyTooLarge`]).
    pub span: (u32, u32),
    /// Zero-based position within the document, for display order only.
    ///
    /// Never identity: [`Chunk::id`] is the content hash, and this field must
    /// not be used to match chunks across edits.
    pub ordinal: u16,
}

impl Chunk {
    /// The chunk's source text, sliced out of the document body.
    ///
    /// Returns `""` rather than panicking when handed a body the chunk was not
    /// derived from — a stale span is a caller bug, not a reason to abort a
    /// query.
    #[must_use]
    pub fn text<'a>(&self, body: &'a str) -> &'a str {
        let start = self.span.0 as usize;
        let end = self.span.1 as usize;
        body.get(start..end).unwrap_or_default()
    }

    /// The chunk's length in characters, counted exactly as the splitter counts
    /// it: the CR of a CRLF pair is not content (§6.5, CH-9).
    ///
    /// This is what `janus chunks` reports as `chars`, so a chunk can be compared
    /// against `max_chars` without re-deriving the rule in the renderer.
    #[must_use]
    pub fn char_count(&self, body: &str) -> usize {
        count_chars(self.text(body))
    }

    /// The heading path as one string, e.g. `"Architecture › Caching"`.
    ///
    /// Display-friendly and title-free: the title is prepended by
    /// [`Chunk::embed_text`], because it belongs to the document, not the chunk.
    #[must_use]
    pub fn breadcrumb(&self) -> String {
        self.heading_path.join(" › ")
    }

    /// The text an embedder or a lexical indexer should see for this chunk.
    ///
    /// `title › heading › subheading`, a blank line, then the chunk itself. The
    /// breadcrumb is the cheap, deterministic form of contextual retrieval
    /// (§6.5 rule 4, D14): it costs nothing and materially improves recall on
    /// short sections. Any heading that merely repeats the title is dropped, so
    /// a document whose H1 is its title does not read `Undated › Undated`.
    ///
    /// This is **not** part of the chunk's identity: editing the title must not
    /// change any [`Chunk::id`] (CH-5).
    #[must_use]
    pub fn embed_text(&self, body: &str, title: &str) -> String {
        let title = title.trim();
        let mut out =
            String::with_capacity(title.len() + self.span.1.saturating_sub(self.span.0) as usize);
        out.push_str(title);
        for part in self.heading_path.iter().filter(|part| *part != title) {
            if !out.is_empty() {
                out.push_str(" › ");
            }
            out.push_str(part);
        }
        out.push_str("\n\n");
        out.push_str(self.text(body));
        out
    }
}

/// Splits `body` into chunks, returning them with any notes about the split.
///
/// The limit counts Unicode scalar values, **not** the CR of a CRLF pair, so a
/// vault checked out with `autocrlf` chunks identically to one with LF (CH-9).
///
/// Deterministic: the same `(doc_id, body, config)` always yields equal chunks.
/// `config` is trusted to be sensible but never assumed to be: a zero limit is
/// treated as 1, and an overlap outside `0.0..=0.5` (or NaN) is clamped, so a
/// hand-edited `janus.toml` cannot hang or panic a scan (I-8).
#[must_use]
pub fn chunk_body(
    doc_id: DocId,
    body: &str,
    config: &ChunkingConfig,
) -> (Vec<Chunk>, Vec<DocNote>) {
    let mut notes = Vec::new();
    if let Some(note) = body_size_note(body.len()) {
        notes.push(note);
        return (Vec::new(), notes);
    }

    let limits = Limits::new(config);
    let mut chunks = Vec::new();
    for section in sections(body) {
        let atoms = atoms(body, &section, limits.max_chars);
        pack(
            body,
            doc_id,
            &section.heading_path,
            &atoms,
            &limits,
            &mut chunks,
            &mut notes,
        );
    }

    if chunks.len() > usize::from(ORDINAL_MAX) {
        notes.push(DocNote::TooManyChunks);
    }
    for (index, chunk) in chunks.iter_mut().enumerate() {
        chunk.ordinal = u16::try_from(index).unwrap_or(ORDINAL_MAX);
    }

    (chunks, notes)
}

/// The note to emit when a body's length cannot be expressed in a `u32` span.
///
/// Split out from [`chunk_body`] so the branch is testable without allocating
/// four gigabytes.
fn body_size_note(len: usize) -> Option<DocNote> {
    (len > u32::MAX as usize).then_some(DocNote::BodyTooLarge)
}

/// Effective limits after clamping, so the packing loop has no special cases.
#[derive(Debug, Clone, Copy)]
struct Limits {
    max_chars: usize,
    overlap_chars: usize,
}

impl Limits {
    fn new(config: &ChunkingConfig) -> Self {
        let max_chars = config.max_chars.max(1);
        // `f64::clamp` propagates NaN, and a NaN target would make the overlap
        // loop run to the start of the chunk every time. A non-finite overlap is
        // a config accident, not an instruction.
        let overlap = if config.overlap.is_finite() {
            config.overlap.clamp(0.0, 0.5)
        } else {
            0.0
        };
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation
        )]
        let overlap_chars = (overlap * max_chars as f64).round() as usize;
        Self {
            max_chars,
            overlap_chars,
        }
    }
}

/// Splits one section into the atoms a boundary may fall between.
///
/// The section's own heading line leads, because that is what the chunk should
/// quote (§6.5 rule 4, CH-4): a chunk about a decision starts with
/// `## Decision`, not with the paragraph after it. The heading is atomic — an
/// enormous heading is an oversized chunk rather than a cut one.
fn atoms(body: &str, section: &Section, limit: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if let Some(first) = section.blocks.first()
        && section.span.start < first.span.start
    {
        out.push(section.span.start..first.span.start);
    }
    for block in &section.blocks {
        match block.kind {
            BlockKind::Paragraph => {
                for sentence in sentences(body, block.span.clone()) {
                    if chars_in(body, &sentence) <= limit {
                        out.push(sentence);
                    } else {
                        for word in words(body, sentence) {
                            if chars_in(body, &word) <= limit {
                                out.push(word);
                            } else {
                                out.extend(char_slices(body, word, limit));
                            }
                        }
                    }
                }
            }
            // A list is splittable *between* its items and nowhere else: an item
            // carries its own marker, so a boundary there keeps it a list.
            BlockKind::List => out.extend(block.items.iter().cloned()),
            _ => out.push(block.span.clone()),
        }
    }
    out
}

/// Splits a paragraph into sentence spans, trimmed to their content.
///
/// A sentence ends after `.`, `!`, `?` (and the CJK forms) when the next
/// character is whitespace or the paragraph ends. This is deliberately crude:
/// it only runs when a paragraph is already too large, so a mis-split costs one
/// slightly odd chunk boundary, never lost text.
fn sentences(body: &str, span: Range<usize>) -> Vec<Range<usize>> {
    let text = &body[span.clone()];
    let mut out = Vec::new();
    let offset = span.start;
    let mut start = 0;

    let mut iter = text.char_indices().peekable();
    while let Some((index, character)) = iter.next() {
        if !is_sentence_end(character) {
            continue;
        }
        let end = index + character.len_utf8();
        let boundary = match iter.peek() {
            None => true,
            Some(&(_, next)) => next.is_whitespace(),
        };
        if boundary {
            push_sentence(body, offset, start..end, &mut out);
            start = end;
        }
    }
    if start < text.len() {
        push_sentence(body, offset, start..text.len(), &mut out);
    }
    out
}

/// Pushes one sentence, trimmed, relative to the section being scanned.
fn push_sentence(body: &str, offset: usize, span: Range<usize>, out: &mut Vec<Range<usize>>) {
    if let Some(sentence) = trim_span(body, offset + span.start..offset + span.end) {
        out.push(sentence);
    }
}

fn is_sentence_end(character: char) -> bool {
    matches!(character, '.' | '!' | '?' | '。' | '！' | '？')
}

/// Splits a span at whitespace, returning trimmed word spans.
fn words(body: &str, span: Range<usize>) -> Vec<Range<usize>> {
    let text = &body[span.clone()];
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            if let Some(word_start) = start.take()
                && let Some(word) = trim_span(body, span.start + word_start..span.start + index)
            {
                out.push(word);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(word_start) = start
        && let Some(word) = trim_span(body, span.start + word_start..span.end)
    {
        out.push(word);
    }
    out
}

/// Hard-splits a single word into `limit`-sized character slices.
fn char_slices(body: &str, span: Range<usize>, limit: usize) -> Vec<Range<usize>> {
    let text = &body[span.clone()];
    let mut out = Vec::new();
    let mut start = 0;
    let mut count = 0;
    for (index, character) in text.char_indices() {
        if count == limit {
            out.push(span.start + start..span.start + index);
            start = index;
            count = 0;
        }
        if character != '\r' {
            count += 1;
        }
    }
    if start < text.len() {
        out.push(span.start + start..span.end);
    }
    out
}

/// Trims whitespace from both ends of a span, returning `None` when nothing is
/// left. Trimming is what keeps chunk spans free of the blank lines between
/// blocks, so a chunk's text starts at real content.
fn trim_span(body: &str, span: Range<usize>) -> Option<Range<usize>> {
    let text = &body[span.clone()];
    let start = span.start + (text.len() - text.trim_start().len());
    let end = span.start + text.trim_end().len();
    (start < end).then_some(start..end)
}

/// Characters in a span, **not counting the CR of a CRLF pair**.
///
/// A carriage return is line-ending noise, never content: ignoring it makes a
/// vault checked out with `autocrlf` chunk exactly like one with LF, which in
/// turn makes chunk ids and boundaries identical across the two (CH-9, §8.7).
fn chars_in(body: &str, span: &Range<usize>) -> usize {
    count_chars(&body[span.clone()])
}

/// Characters in `text`, not counting the CR of a CRLF pair.
///
/// The single definition of "chunk length", shared by the splitter and by
/// [`Chunk::char_count`]: if a report counted carriage returns the limit would
/// appear to be violated by a document that is merely checked out with
/// `autocrlf`.
fn count_chars(text: &str) -> usize {
    text.chars().filter(|character| *character != '\r').count()
}

/// Packs one section's atoms into chunks, appending to `chunks`.
fn pack(
    body: &str,
    doc_id: DocId,
    heading_path: &[String],
    atoms: &[Range<usize>],
    limits: &Limits,
    chunks: &mut Vec<Chunk>,
    notes: &mut Vec<DocNote>,
) {
    let mut first = 0;
    while first < atoms.len() {
        // Greedily take atoms while the running character count stays within the
        // limit. Counting bytes between the previous atom's end and this one's
        // end means the whitespace *between* atoms is counted exactly once.
        let mut last = first;
        let mut cursor = atoms[first].start;
        let mut chars = 0;
        while last < atoms.len() {
            let added = chars_in(body, &(cursor..atoms[last].end));
            if chars + added > limits.max_chars && last > first {
                break;
            }
            chars += added;
            cursor = atoms[last].end;
            last += 1;
        }

        let end = last - 1;
        if chars > limits.max_chars {
            // Only an indivisible atom can get here: paragraphs are split down to
            // single characters, so an oversized chunk is a fence, a table, a
            // quote or one very long list item.
            notes.push(DocNote::OversizedBlock { chars });
        }
        if let Some(span) = trim_span(body, atoms[first].start..atoms[end].end) {
            let text = &body[span.clone()];
            chunks.push(Chunk {
                id: chunk_id(heading_path, normalize(text).as_ref()),
                doc_id,
                heading_path: heading_path.to_vec(),
                span: (span.start as u32, span.end as u32),
                ordinal: 0, // assigned once the whole document is known
            });
        }

        if last == atoms.len() {
            break;
        }
        first = overlap_start(body, atoms, first, end, chars, limits.overlap_chars);
    }
}

/// Chooses the first atom of the next chunk: the tail of the emitted one.
///
/// Returns an index strictly greater than `first`, which is the loop's progress
/// guarantee. The tail is at least `overlap_chars` long (or the whole chunk if
/// it is shorter), but never more than half of the chunk, so overlap can never
/// turn one chunk into the next.
///
/// The `earliest` floor is what makes progress unconditional: a chunk that is a
/// single atom has nothing to re-include, so the next chunk starts after it even
/// when overlap is configured. Returning `first` here would make the packing
/// loop re-emit the same chunk forever.
fn overlap_start(
    body: &str,
    atoms: &[Range<usize>],
    first: usize,
    end: usize,
    chunk_chars: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return end + 1;
    }
    let earliest = first + 1;
    if end < earliest {
        return earliest;
    }
    let target = overlap_chars.min(chunk_chars / 2);
    let mut start = end;
    let mut chars = chars_in(body, &atoms[end].clone());
    while start > earliest && chars < target {
        let previous = start - 1;
        chars += chars_in(body, &(atoms[previous].start..atoms[start].start));
        start = previous;
    }
    start
}

/// The chunk id: `blake3(heading path ‖ 0x00 ‖ text)` (§6.4, D24).
///
/// Each heading component is NUL-terminated, so `["a", "b"]` and `["a\0b"]`
/// cannot collide. The text is length-prefixed for the same reason: a body may
/// legally contain a NUL byte, and without the prefix it could impersonate a
/// path separator.
fn chunk_id(heading_path: &[String], text: &str) -> ChunkId {
    let mut hasher = blake3::Hasher::new();
    for part in heading_path {
        hasher.update(part.as_bytes());
        hasher.update(&[ID_SEPARATOR]);
    }
    hasher.update(&(text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
    ChunkId::from_bytes(*hasher.finalize().as_bytes())
}

/// Normalizes chunk text for hashing only (CH-9).
///
/// Line endings become `\n`, trailing whitespace is stripped from each line, and
/// the whole is trimmed. Nothing else changes: Unicode normalization would
/// silently merge text the author wrote differently, and comment or markdown
/// reformatting is the writer's job, not the hasher's.
///
/// Returns a borrow when the text is already normalized, which is the common
/// case, so the hot path allocates nothing.
fn normalize(text: &str) -> Cow<'_, str> {
    let needs_work = text.contains('\r')
        || text.trim() != text
        || text
            .split('\n')
            .any(|line| line.ends_with(' ') || line.ends_with('\t'));
    if !needs_work {
        return Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len());
    for line in text.split('\n') {
        out.push_str(line.trim_end_matches(['\r', ' ', '\t']));
        out.push('\n');
    }
    Cow::Owned(out.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(max_chars: usize, overlap: f64) -> ChunkingConfig {
        ChunkingConfig { max_chars, overlap }
    }

    fn doc() -> DocId {
        DocId::from_match_key(b"test-document")
    }

    fn chunks(body: &str, max_chars: usize, overlap: f64) -> Vec<Chunk> {
        chunk_body(doc(), body, &config(max_chars, overlap)).0
    }

    fn texts<'a>(chunks: &'a [Chunk], body: &'a str) -> Vec<&'a str> {
        chunks.iter().map(|chunk| chunk.text(body)).collect()
    }

    /// Fence markers per chunk, used to prove no fence is ever split.
    fn fence_lines(text: &str) -> usize {
        text.lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("```") || trimmed.starts_with("~~~")
            })
            .count()
    }

    #[test]
    fn a_short_document_is_one_chunk_per_section() {
        let body = "# A\n\nfirst\n\n# B\n\nsecond\n";
        let chunks = chunks(body, 1800, 0.15);
        assert_eq!(texts(&chunks, body), vec!["# A\n\nfirst", "# B\n\nsecond"]);
        assert_eq!(chunks[0].heading_path, vec!["A".to_owned()]);
        assert_eq!(chunks[1].heading_path, vec!["B".to_owned()]);
    }

    #[test]
    fn ordinals_are_contiguous_and_spans_ascending() {
        let body = "# A\n\none two three four five six seven eight nine ten\n\n# B\n\nlast\n";
        let chunks = chunks(body, 12, 0.0);
        assert!(chunks.len() > 2);
        for (index, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.ordinal, index as u16);
            if index > 0 {
                assert!(chunk.span.0 > chunks[index - 1].span.0);
            }
            assert!(chunk.span.0 < chunk.span.1);
            assert!(chunk.span.1 as usize <= body.len());
        }
    }

    #[test]
    fn a_long_paragraph_splits_at_word_boundaries() {
        let body = "word ".repeat(200);
        let chunks = chunks(&body, 60, 0.0);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(
                chunk.text(&body).chars().count() <= 60,
                "chunk over the limit: {:?}",
                chunk.text(&body)
            );
            assert!(!chunk.text(&body).starts_with(' '));
        }
    }

    #[test]
    fn sentences_are_the_preferred_boundary() {
        let sentence = "This sentence is exactly long enough to matter. ";
        let body = sentence.repeat(10);
        let chunks = chunks(&body, 60, 0.0);
        for chunk in &chunks {
            let text = chunk.text(&body);
            assert!(
                text.ends_with('.') || text.len() < 40,
                "expected a sentence boundary, got {text:?}"
            );
        }
    }

    #[test]
    fn a_fence_is_never_split_and_is_reported() {
        let body = format!("```\n{}\n```\n", "x".repeat(3000));
        let (chunks, notes) = chunk_body(doc(), &body, &config(100, 0.15));
        assert_eq!(chunks.len(), 1, "one oversized fence block");
        assert_eq!(chunks[0].text(&body), body.trim_end());
        assert!(matches!(notes.as_slice(), [DocNote::OversizedBlock { chars }] if *chars > 3000));
        assert_eq!(fence_lines(chunks[0].text(&body)), 2);
    }

    #[test]
    fn a_table_is_atomic() {
        let mut body = String::from("| a | b |\n|---|---|\n");
        for row in 0..200 {
            body.push_str(&format!("| {row} | value |\n"));
        }
        let (chunks, notes) = chunk_body(doc(), &body, &config(100, 0.15));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text(&body), body.trim_end());
        assert!(notes.iter().any(|note| note.code() == "oversized_block"));
    }

    #[test]
    fn a_list_splits_between_items_only() {
        let mut body = String::new();
        for item in 0..40 {
            body.push_str(&format!("- item {item} with a little body text\n"));
        }
        let (chunks, notes) = chunk_body(doc(), &body, &config(120, 0.0));
        assert!(chunks.len() > 1, "the list must split");
        assert!(notes.is_empty(), "no item is oversized");
        for chunk in &chunks {
            for line in chunk.text(&body).lines() {
                assert!(
                    line.starts_with("- item "),
                    "split inside an item: {line:?}"
                );
            }
        }
    }

    #[test]
    fn overlap_repeats_the_tail_of_the_previous_chunk() {
        let body = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";
        let chunks = chunks(body, 30, 0.5);
        assert!(chunks.len() > 1);
        for pair in chunks.windows(2) {
            let previous = pair[0].text(body);
            let next = pair[1].text(body);
            let previous_words: Vec<&str> = previous.split_whitespace().collect();
            let first_next = next.split_whitespace().next().unwrap();
            assert!(
                previous_words.contains(&first_next),
                "{next:?} does not continue {previous:?}"
            );
        }
    }

    #[test]
    fn overlap_always_makes_progress() {
        // A single repeating word is the pathological case for a tail search.
        let body = "aa ".repeat(400);
        let chunks = chunks(&body, 20, 0.5);
        assert!(chunks.len() > 1);
        for pair in chunks.windows(2) {
            assert!(pair[0].span.0 < pair[1].span.0);
        }
    }

    #[test]
    fn an_oversized_chunk_with_overlap_still_advances() {
        // Regression: a chunk made of exactly one atom has no tail to
        // re-include, so the overlap search used to return its own first atom
        // and the packing loop re-emitted it forever.
        let body = format!("```\n{}\n```\n\nafter the fence\n", "x".repeat(500));
        let (chunks, notes) = chunk_body(doc(), &body, &config(50, 0.5));
        assert_eq!(chunks.len(), 2, "the fence and the paragraph after it");
        assert!(chunks[0].text(&body).starts_with("```"));
        assert_eq!(chunks[1].text(&body), "after the fence");
        assert!(notes.iter().any(|note| note.code() == "oversized_block"));
        assert!(chunks[0].span.0 < chunks[1].span.0);
    }

    #[test]
    fn zero_overlap_produces_disjoint_chunks() {
        let body = "one two three four five six seven eight nine ten eleven twelve";
        let chunks = chunks(body, 20, 0.0);
        assert!(chunks.len() > 1);
        for pair in chunks.windows(2) {
            assert!(pair[0].span.1 <= pair[1].span.0);
        }
    }

    #[test]
    fn a_chunk_text_starts_with_its_heading() {
        let body = "# Title\n\nbody\n\n## Decision\n\nUse sqlite.\n";
        let chunks = chunks(body, 1800, 0.0);
        let decision = chunks
            .iter()
            .find(|chunk| chunk.heading_path == vec!["Title".to_owned(), "Decision".to_owned()])
            .expect("the decision section chunks");
        assert!(decision.text(body).starts_with("## Decision"));
    }

    #[test]
    fn identities_are_content_addressed_without_the_document() {
        let body = "# A\n\nthe same text\n";
        let one = chunk_body(DocId::from_match_key(b"one"), body, &config(1800, 0.0)).0;
        let two = chunk_body(DocId::from_match_key(b"two"), body, &config(1800, 0.0)).0;
        assert_eq!(
            one.iter().map(|c| c.id).collect::<Vec<_>>(),
            two.iter().map(|c| c.id).collect::<Vec<_>>(),
            "the same text in another document shares ids (D24)"
        );
        assert_ne!(one[0].doc_id, two[0].doc_id);
    }

    #[test]
    fn identity_depends_on_the_heading_path() {
        let a = chunks("# A\n\ntext\n", 1800, 0.0);
        let b = chunks("# B\n\ntext\n", 1800, 0.0);
        assert_ne!(a[0].id, b[0].id);
    }

    #[test]
    fn identity_ignores_line_endings_and_edge_whitespace() {
        let lf = chunks("# A\n\ntext here\n", 1800, 0.0);
        let crlf = chunks("# A\r\n\r\ntext here\r\n", 1800, 0.0);
        assert_eq!(lf[0].id, crlf[0].id);
    }

    #[test]
    fn identity_is_unaffected_by_the_title() {
        let body = "# Heading\n\ntext\n";
        let chunks = chunks(body, 1800, 0.0);
        let renamed = chunks[0].embed_text(body, "A completely different title");
        assert!(renamed.starts_with("A completely different title"));
        // The id is a function of the heading path and the text, nothing else.
        assert_eq!(
            chunks[0].id,
            chunk_id(&["Heading".to_owned()], "# Heading\n\ntext")
        );
    }

    #[test]
    fn duplicate_ids_within_one_document_are_accepted() {
        let body = "# A\n\nsame\n\n# A\n\nsame\n";
        let chunks = chunks(body, 1800, 0.0);
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            chunks[0].id, chunks[1].id,
            "identical content, one id (D24)"
        );
        assert_ne!(chunks[0].ordinal, chunks[1].ordinal);
    }

    #[test]
    fn a_heading_path_cannot_impersonate_chunk_text() {
        let one = chunk_id(&["a".to_owned(), "b".to_owned()], "x");
        let two = chunk_id(&["a".to_owned()], "b\0x");
        assert_ne!(one, two);
    }

    #[test]
    fn empty_bodies_have_no_chunks() {
        for body in ["", "\n\n", "   \n\t\n"] {
            let (chunks, notes) = chunk_body(doc(), body, &config(1800, 0.15));
            assert!(chunks.is_empty(), "{body:?}");
            assert!(notes.is_empty(), "{body:?}");
        }
    }

    #[test]
    fn a_zero_limit_is_clamped_instead_of_panicking() {
        let (chunks, _) = chunk_body(doc(), "abcdef", &config(0, 0.15));
        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(chunk.text("abcdef").chars().count() <= 1);
        }
    }

    #[test]
    fn degenerate_overlap_is_clamped() {
        let body = "one two three four five six seven eight";
        for overlap in [f64::NAN, f64::INFINITY, -1.0, 5.0] {
            let (chunks, _) = chunk_body(doc(), body, &config(20, overlap));
            assert!(!chunks.is_empty(), "overlap {overlap}");
            for pair in chunks.windows(2) {
                assert!(pair[0].span.0 < pair[1].span.0, "overlap {overlap} stalled");
            }
        }
    }

    #[test]
    fn an_oversized_body_is_refused_by_length_not_by_allocation() {
        assert!(body_size_note(10).is_none());
        assert!(body_size_note(u32::MAX as usize).is_none());
        assert!(matches!(
            body_size_note(u32::MAX as usize + 1),
            Some(DocNote::BodyTooLarge)
        ));
    }

    #[test]
    fn embed_text_prepends_the_breadcrumb_without_repeating_the_title() {
        let body = "# Caching\n\n## Decision\n\nUse sqlite.\n";
        let chunks = chunks(body, 1800, 0.0);
        let decision = chunks
            .iter()
            .find(|chunk| chunk.heading_path.len() == 2)
            .expect("the nested section chunks");
        let text = decision.embed_text(body, "Caching");
        assert_eq!(text, "Caching › Decision\n\n## Decision\n\nUse sqlite.");

        let preamble = chunk_body(doc(), "intro\n", &config(1800, 0.0)).0;
        assert_eq!(preamble[0].embed_text("intro\n", "Title"), "Title\n\nintro");
    }

    #[test]
    fn normalize_borrows_when_there_is_nothing_to_do() {
        assert!(matches!(normalize("already clean"), Cow::Borrowed(_)));
        assert!(matches!(normalize("trailing  \n"), Cow::Owned(_)));
        assert!(matches!(normalize("crlf\r\n"), Cow::Owned(_)));
        assert_eq!(normalize("a\r\nb\r\n"), "a\nb");
        assert_eq!(normalize("  padded  "), "padded");
    }
}
