//! Read-side preservation matrix over the fixture vault — the M7 golden corpus
//! (SPEC §7.3, §7.4 invariants I1–I5).
//!
//! M2 builds no patcher, so what it can prove is the *contract* the patcher will
//! rely on: the reader isolates the frontmatter as a byte range and hands back
//! the raw text, the prefix (a BOM, or nothing) and the body **verbatim**. Every
//! assertion below is byte equality, not equality after re-serialization —
//! re-serializing is exactly what §7.3 forbids, and a YAML round-trip that
//! "looks the same" is what loses the comment, the quoting or the anchor.
//!
//! The fixtures are chosen for the shapes §7.3 must splice or refuse: block
//! sequences, flow mappings, anchors and aliases, multi-line scalars, quoted
//! scalars with escapes, comments on keys, CRLF, and a body that opens with
//! `---`. Each one is also an ordinary document, so it has to scan and chunk.
//!
//! The property tests at the bottom are the general form of the same claim: for
//! **any** text, prefix + raw + body reconstructs the input exactly.

use std::fs;
use std::path::{Path, PathBuf};

use janus_mem::config::Config;
use janus_mem::document::{Frontmatter, FrontmatterSpan};
use janus_mem::vault::frontmatter;
use proptest::prelude::*;

/// The committed fixture vault, relative to the crate root.
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault");

fn config() -> Config {
    Config::load(Path::new(FIXTURE)).expect("the fixture config must parse")
}

/// Every committed Markdown fixture, recursively, sorted by `rel_path`.
///
/// Walks the filesystem rather than the corpus walker on purpose: this matrix
/// covers files the walk excludes too (`archive/private/secret.md`, `target/`,
/// `node_modules/`), because a file being excluded from the corpus says nothing
/// about whether its bytes would survive a patch once it is admitted.
fn fixture_files() -> Vec<(String, PathBuf)> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
        let entries = fs::read_dir(dir).expect("the fixture tree must be readable");
        for entry in entries {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                visit(root, &path, out);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, path));
            }
        }
    }

    let mut out = Vec::new();
    visit(Path::new(FIXTURE), Path::new(FIXTURE), &mut out);
    out.sort();
    assert!(out.len() >= 15, "the fixture vault must stay populated");
    out
}

/// The three-way split §7.3 promises, as one string.
///
/// `prefix + raw + body` must equal the input, which is invariant I1 stated in a
/// way a test can check in one line.
fn rebuild(text: &str, read: &frontmatter::FrontmatterRead<'_>) -> String {
    format!(
        "{}{}{}",
        &text[..read.span.start],
        read.raw.unwrap_or_default(),
        read.body
    )
}

/// A reader result with owned fields, so assertions can outlive the source text.
struct OwnedRead {
    text: String,
    span: FrontmatterSpan,
    raw: Option<String>,
    body: String,
    bom_len: usize,
    frontmatter: Frontmatter,
}

impl OwnedRead {
    fn of(rel: &str, config: &Config) -> Self {
        let path = Path::new(FIXTURE).join(rel);
        let text = fs::read_to_string(&path).unwrap_or_else(|err| panic!("{rel}: {err}"));
        let read = frontmatter::read(&text, config);
        Self {
            span: read.span,
            raw: read.raw.map(str::to_owned),
            body: read.body.to_owned(),
            bom_len: read.bom_len,
            frontmatter: read.frontmatter,
            text,
        }
    }

    fn rebuild(&self) -> String {
        format!(
            "{}{}{}",
            &self.text[..self.span.start],
            self.raw.as_deref().unwrap_or_default(),
            self.body
        )
    }
}

#[test]
fn every_fixture_file_round_trips_through_the_reader() {
    let config = config();

    for (rel, _) in fixture_files() {
        let read = OwnedRead::of(&rel, &config);
        let text = &read.text;

        assert!(
            text.is_char_boundary(read.span.start) && text.is_char_boundary(read.span.end),
            "{rel}: span must land on char boundaries"
        );
        assert!(read.span.start <= read.span.end, "{rel}: span inverted");
        assert!(read.span.end <= text.len(), "{rel}: span past the end");

        assert_eq!(read.rebuild(), *text, "{rel}: bytes were lost");
        assert_eq!(
            read.body,
            text[read.span.end..],
            "{rel}: body is not the suffix"
        );

        // The prefix is a BOM or nothing: nothing else may precede the fence.
        let prefix = &text[..read.span.start];
        assert!(
            prefix.is_empty() || prefix == "\u{feff}",
            "{rel}: unexpected prefix {prefix:?}"
        );
        assert_eq!(prefix.len(), read.bom_len, "{rel}: BOM accounting drifted");

        if let Some(raw) = &read.raw {
            assert_eq!(
                *raw,
                text[read.span.start..read.span.end],
                "{rel}: raw is not the region"
            );
            assert!(
                raw.starts_with("---"),
                "{rel}: frontmatter must open with a fence"
            );
            assert!(
                raw.ends_with("---\n") || raw.ends_with("---\r\n"),
                "{rel}: frontmatter must close with a fence and a newline"
            );
            assert_eq!(
                read.span.start, read.bom_len,
                "{rel}: span must start after the BOM"
            );
        } else {
            // No frontmatter: the span is an empty region after the BOM, so a
            // block spliced in later keeps the BOM where it belongs.
            assert_eq!(read.span.start, read.span.end, "{rel}: empty span expected");
            assert_eq!(read.body, text[read.bom_len..], "{rel}: whole file is body");
        }
    }
}

#[test]
fn the_typed_view_reports_what_each_splice_fixture_says() {
    let config = config();
    let read = |rel: &str| OwnedRead::of(rel, &config);

    // A block sequence reads as a list, and its bytes stay a block sequence.
    let block = read("notes/block-sequence.md");
    assert_eq!(block.frontmatter.tags, ["splice", "block"]);
    raw_contains(&block, "tags:\n  - splice\n  - block\n");

    // A flow mapping is an unknown key for the typed view, and survives anyway.
    let flow = read("notes/flow-mapping.md");
    assert_eq!(flow.frontmatter.title.as_deref(), Some("Flow mapping"));
    assert_eq!(flow.frontmatter.status.as_deref(), Some("active"));
    raw_contains(&flow, "owner: {name: me, team: infra}");

    // An anchor and its alias are two keys with one value.
    let anchor = read("notes/anchor-alias.md");
    assert_eq!(
        anchor.frontmatter.title.as_deref(),
        Some("Anchor and alias")
    );
    raw_contains(&anchor, "team: &platform infra\nreviewer: *platform");

    // Both block-scalar styles keep their bytes, indentation included.
    let multiline = read("notes/multiline-scalar.md");
    assert_eq!(
        multiline.frontmatter.title.as_deref(),
        Some("Multi-line scalar")
    );
    raw_contains(&multiline, "summary: |\n  First line of a literal block.\n");
    raw_contains(&multiline, "note: >-\n");

    // Quoted scalars keep their escapes and the comment on the key.
    let quoted = read("notes/quoted-scalars.md");
    assert_eq!(
        quoted.frontmatter.title.as_deref(),
        Some("Quoted: a scoped title")
    );
    raw_contains(&quoted, r#"motto: "say \"hello\" # not a comment""#);
    raw_contains(&quoted, "status: active  # this comment must survive");

    // CRLF file: aliased keys resolve, and CRLF is still CRLF in the raw block.
    let crlf = read("specs/crlf-spec.md");
    assert_eq!(crlf.frontmatter.title.as_deref(), Some("CRLF line endings"));
    assert_eq!(crlf.frontmatter.tags, ["line-endings", "fixtures"]);
    raw_contains(&crlf, "---\r\n");
    raw_contains(&crlf, "related:\r\n  - crlf\r\n");
}

/// Asserts a fragment is present in the raw block, showing the block when it is
/// not — a bare `assert!(raw.contains(..))` on escaped YAML is hard to debug.
fn raw_contains(read: &OwnedRead, needle: &str) {
    let raw = read.raw.as_deref().expect("this fixture has frontmatter");
    assert!(raw.contains(needle), "expected {needle:?} in:\n{raw}");
}

#[test]
fn a_body_that_opens_with_a_rule_is_not_swallowed_as_frontmatter() {
    let read = OwnedRead::of("specs/body-starts-with-rule.md", &config());

    let raw = read.raw.as_deref().expect("the file has frontmatter");
    assert!(raw.contains("title: Body starts with a rule"));
    // The F-2 trap: the second `---` belongs to the body, so it is *after* the
    // span and must not be mistaken for an opening fence or a setext underline.
    assert!(
        read.body.starts_with("---\n"),
        "body must keep its leading rule: {:?}",
        &read.body[..read.body.len().min(20)]
    );
    assert_eq!(read.rebuild(), read.text);
}

#[test]
fn a_bom_belongs_to_the_prefix_not_the_frontmatter() {
    let read = OwnedRead::of("notes/bom-note.md", &config());
    assert!(read.text.starts_with('\u{feff}'));

    assert_eq!(read.bom_len, 3);
    assert_eq!(read.span.start, 3, "the span starts after the BOM");
    assert_eq!(&read.text[..read.span.start], "\u{feff}");
    assert_eq!(read.frontmatter.title.as_deref(), Some("BOM note"));
    assert_eq!(read.rebuild(), read.text);
}

// ───────────────────────────── property tests ──────────────────────────────

proptest! {
    /// The general form of I1: whatever the input, the reader accounts for every
    /// byte, so a patch that replaces the raw region can never lose text.
    #[test]
    fn the_reader_accounts_for_every_byte(text in "\\PC*") {
        let read = frontmatter::read(&text, &config());

        prop_assert_eq!(rebuild(&text, &read), text.clone());
        prop_assert!(text.is_char_boundary(read.span.start));
        prop_assert!(text.is_char_boundary(read.span.end));
        prop_assert!(read.span.end <= text.len());
        prop_assert_eq!(read.body, &text[read.span.end..]);
    }

    /// A generated block between fences is isolated exactly, whatever the YAML
    /// inside it is — valid, garbage, or an attempt to look like a fence.
    #[test]
    fn a_generated_block_is_isolated_without_reformatting(
        yaml in "[-a-zA-Z0-9_ :#\\[\\]{}|>'&*.,\"\\\\]{0,40}",
        body in "\\PC{0,40}",
    ) {
        let text = format!("---\n{yaml}\n---\n{body}");
        let read = frontmatter::read(&text, &config());

        prop_assert_eq!(read.raw, Some(&text[..text.len() - body.len()]));
        prop_assert_eq!(read.body, body.as_str());
        prop_assert_eq!(rebuild(&text, &read), text.clone());
        // Whatever the typed view could not use, it reported instead of failing.
        prop_assert!(read.span.start <= read.span.end);
    }

    /// CRLF frontmatter isolates the same way, and the reader never normalizes
    /// line endings — the `\r` count of the finished document is the input's.
    #[test]
    fn crlf_is_never_normalized(yaml in "[-a-zA-Z0-9_ :.,]{0,30}", body in "\\PC{0,30}") {
        let text = format!("---\r\n{yaml}\r\n---\r\n{body}");
        let read = frontmatter::read(&text, &config());

        prop_assert_eq!(rebuild(&text, &read), text.clone());
        let rebuilt = rebuild(&text, &read);
        prop_assert_eq!(rebuilt.matches('\r').count(), text.matches('\r').count());
        prop_assert!(read.raw.is_some_and(|raw| raw.ends_with("---\r\n")), "closing fence lost CRLF");
    }
}
