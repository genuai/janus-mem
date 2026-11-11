//! The frontmatter scanner against the M7 golden corpus (SPEC §7.3).
//!
//! This file locates bytes: which key sits on which line, where its value starts
//! and ends, where a trailing comment begins, and which shapes are refused. The
//! corpus is the fixture vault M2 built for this phase (`tests/fixtures/vault`);
//! no second corpus is created here.
//!
//! The cases of the phase plan that assert a *shape* are here. The cases that
//! assert a *refusal* or a splice arrive with the patch planner, because a
//! refusal is its answer (cases 1.5, 1.6, 1.8 and 1.12c).

use std::fs;
use std::path::Path;

use janus_mem::config::{Config, Field};
use janus_mem::document::FrontmatterSpan;
use janus_mem::vault::frontmatter;
use janus_mem::write::{self, PatchError, ValueShape};
use proptest::prelude::*;

/// The committed fixture vault, relative to the crate root.
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault");

/// The fixture vault's config.
fn config() -> Config {
    Config::load(Path::new(FIXTURE)).expect("the fixture config must parse")
}

/// One fixture, as the bytes on disk.
fn fixture(rel: &str) -> String {
    fs::read_to_string(Path::new(FIXTURE).join(rel)).unwrap_or_else(|err| panic!("{rel}: {err}"))
}

/// The shape the scanner reports for one key of one fixture.
fn shape_of(rel: &str, key: &str) -> ValueShape {
    let text = fixture(rel);
    let scan = write::scan(&text).unwrap_or_else(|err| panic!("{rel}: {err}"));
    scan.keys
        .iter()
        .find(|span| span.key == key)
        .unwrap_or_else(|| panic!("{rel} has no key {key}"))
        .shape
}

/// The shape the scanner reports for one key of one inline document.
fn shape_in(text: &str, key: &str) -> ValueShape {
    let scan = write::scan(text).expect("the document scans");
    scan.keys
        .iter()
        .find(|span| span.key == key)
        .unwrap_or_else(|| panic!("no key {key} in:\n{text}"))
        .shape
}

// ───────────────────────────── the shapes ─────────────────────────────────────

#[test]
fn every_golden_shape_is_reported() {
    // Case 1.5 and 1.6: each refused shape names its own reason.
    assert_eq!(
        shape_of("notes/block-sequence.md", "tags"),
        ValueShape::Unsupported(write::reason::BLOCK_SEQUENCE)
    );
    assert_eq!(
        shape_of("notes/yaml-comments.md", "tags"),
        ValueShape::Unsupported(write::reason::BLOCK_SEQUENCE)
    );
    assert_eq!(
        shape_of("notes/flow-mapping.md", "owner"),
        ValueShape::Unsupported(write::reason::FLOW_MAPPING)
    );
    assert_eq!(
        shape_of("notes/anchor-alias.md", "team"),
        ValueShape::Unsupported(write::reason::ANCHOR)
    );
    assert_eq!(
        shape_of("notes/anchor-alias.md", "reviewer"),
        ValueShape::Unsupported(write::reason::ALIAS)
    );
    for key in ["summary", "note"] {
        assert_eq!(
            shape_of("notes/multiline-scalar.md", key),
            ValueShape::Unsupported(write::reason::MULTILINE_SCALAR)
        );
    }

    // A block mapping has no fixture of its own, and is not guessed at.
    assert_eq!(
        shape_in("---\nowner:\n  name: me\n---\n", "owner"),
        ValueShape::Unsupported(write::reason::BLOCK_MAPPING)
    );

    // Case 1.8: a flow sequence of scalars is replaceable, and the block
    // sequence next to it is not.
    assert_eq!(
        shape_of("specs/crlf-spec.md", "labels"),
        ValueShape::FlowSequence
    );
    assert_eq!(
        shape_of("specs/crlf-spec.md", "related"),
        ValueShape::Unsupported(write::reason::BLOCK_SEQUENCE)
    );

    // The scalars around them stay scalars, with and without quotes.
    for (rel, key) in [
        ("notes/block-sequence.md", "status"),
        ("notes/quoted-scalars.md", "motto"),
        ("notes/quoted-scalars.md", "note"),
        ("notes/flow-mapping.md", "status"),
        ("specs/body-starts-with-rule.md", "status"),
    ] {
        assert_eq!(shape_of(rel, key), ValueShape::Scalar, "{rel} {key}");
    }
}

#[test]
fn case_1_7_a_duplicate_key_is_marked_unpatchable() {
    let text = "---\nstatus: active\nstatus: superseded\ntags: [a]\n---\nbody\n";
    let scan = write::scan(text).expect("the document scans");

    let status: Vec<&write::KeySpan> = scan.keys.iter().filter(|k| k.key == "status").collect();
    assert_eq!(status.len(), 2, "both definitions are reported");
    for span in status {
        assert_eq!(
            span.shape,
            ValueShape::Unsupported(write::reason::DUPLICATE_KEY)
        );
    }
    // A duplicate costs its own key, not the file.
    assert_eq!(shape_in(text, "tags"), ValueShape::FlowSequence);
}

#[test]
fn case_1_11_a_null_value_is_an_empty_span_after_the_colon() {
    let text = "---\ntitle: T\nstatus:\n---\nbody\n";
    let scan = write::scan(text).expect("the document scans");
    let status = scan
        .keys
        .iter()
        .find(|span| span.key == "status")
        .expect("status");
    assert_eq!(status.shape, ValueShape::Scalar);
    assert_eq!(status.value.start, status.value.end, "a null has no bytes");
    assert_eq!(status.value.comment, None);
    assert_eq!(&text[status.value.start - 1..status.value.start], ":");

    // A comment after an empty value belongs to the line, not the value.
    let text = "---\nstatus:  # none yet\n---\nbody\n";
    let scan = write::scan(text).expect("the document scans");
    let status = &scan.keys[0];
    assert_eq!(status.shape, ValueShape::Scalar);
    assert_eq!(status.value.start, status.value.end);
    assert_eq!(
        status.value.comment,
        Some(text.find('#').expect("a comment"))
    );

    // A value that is a comment only, with block content under it, is a
    // collection, not a null.
    let text = "---\ntags:  # a list\n  - a\n---\nbody\n";
    assert_eq!(
        shape_in(text, "tags"),
        ValueShape::Unsupported(write::reason::BLOCK_SEQUENCE)
    );
}

#[test]
fn the_comment_boundary_is_where_the_value_ends() {
    let text = fixture("notes/quoted-scalars.md");
    let scan = write::scan(&text).expect("the scan");
    let status = scan
        .keys
        .iter()
        .find(|span| span.key == "status")
        .expect("status");

    // Case 1.4's line: two spaces and a comment survive because they are
    // outside the value span.
    assert_eq!(&text[status.value.start..status.value.end], "active");
    let at = status
        .value
        .comment
        .expect("the line has a trailing comment");
    assert!(
        text[at..].starts_with("# this comment must survive"),
        "{at}"
    );

    // A `#` inside a quoted scalar is text, not a comment.
    let motto = scan
        .keys
        .iter()
        .find(|span| span.key == "motto")
        .expect("motto");
    assert_eq!(motto.value.comment, None);
    assert_eq!(
        &text[motto.value.start..motto.value.end],
        "\"say \\\"hello\\\" # not a comment\""
    );

    // A `#` with no space before it is part of a plain scalar.
    let text = "---\nurl: http://example.test/x#frag\n---\nbody\n";
    let scan = write::scan(text).expect("the scan");
    assert_eq!(
        &text[scan.keys[0].value.start..scan.keys[0].value.end],
        "http://example.test/x#frag"
    );
    assert_eq!(scan.keys[0].value.comment, None);
}

#[test]
fn a_value_that_cannot_be_delimited_is_refused() {
    for (text, key, reason) in [
        (
            "---\nstatus: \"unclosed\n---\nbody\n",
            "status",
            write::reason::NO_VALUE,
        ),
        (
            "---\nstatus: 'unclosed\n---\nbody\n",
            "status",
            write::reason::NO_VALUE,
        ),
        (
            "---\ntags: [a, b\n---\nbody\n",
            "tags",
            write::reason::NO_VALUE,
        ),
        (
            "---\ntags: [a, [b]]\n---\nbody\n",
            "tags",
            write::reason::NESTED,
        ),
        (
            "---\ntags: [a, {b: c}]\n---\nbody\n",
            "tags",
            write::reason::NESTED,
        ),
        (
            "---\ntitle: \"quoted\" junk\n---\nbody\n",
            "title",
            write::reason::NESTED,
        ),
        (
            "---\nstatus: !tagged x\n---\nbody\n",
            "status",
            write::reason::NESTED,
        ),
    ] {
        assert_eq!(
            shape_in(text, key),
            ValueShape::Unsupported(reason),
            "{text:?}"
        );
    }
}

#[test]
fn case_1_12c_the_key_pattern_accepts_every_config_key_and_nothing_else() {
    // Every key the config can read is a patch key, under every alias.
    let config = config();
    for field in Field::ALL {
        assert!(write::is_patch_key(field.as_str()), "{field:?} name");
        for alias in config.aliases(field) {
            assert!(write::is_patch_key(alias), "{field:?} alias {alias}");
        }
    }

    // A key is written as a line, so anything that could add or split a key is
    // not a key.
    for key in [
        "a\nsuperseded_by: x",
        "a: b",
        "a#b",
        "\"a\"",
        "'a'",
        "",
        "-a",
        "a b",
        "a\tb",
        "ü",
        " a",
        "a\n",
    ] {
        assert!(!write::is_patch_key(key), "{key:?} was accepted");
    }
}

#[test]
fn a_line_at_column_zero_that_is_not_a_key_is_refused() {
    // The block is not a mapping of keys, so no key in it can be spliced
    // safely: a top-level sequence or a bare scalar is a refusal.
    for text in [
        "---\n- a\n- b\n---\nbody\n",
        "---\nstandalone\n---\nbody\n",
        "---\nstatus: active\n- a\n---\nbody\n",
    ] {
        let error = write::scan(text).expect_err("refused");
        assert!(
            matches!(
                error,
                PatchError::UnsupportedShape {
                    reason: write::reason::NO_VALUE,
                    ..
                }
            ),
            "{text:?} gave {error:?}"
        );
    }
}

#[test]
fn a_key_that_is_not_a_plain_name_is_reported_unpatchable() {
    // A quoted key and a key with a space are real keys, and no patch can name
    // them. They are reported, left alone, and the other keys still scan.
    for text in [
        "---\n\"a\": 1\nstatus: active\n---\nbody\n",
        "---\nx y: 1\nstatus: active\n---\nbody\n",
    ] {
        let scan = write::scan(text).expect("the document scans");
        let status = scan
            .keys
            .iter()
            .find(|span| span.key == "status")
            .expect("status still scanned");
        assert_eq!(status.shape, ValueShape::Scalar);
        let odd = &scan.keys[0];
        assert_eq!(odd.shape, ValueShape::Unsupported(write::reason::NESTED));
        assert_eq!(odd.value.start, odd.value.end);
    }
}

#[test]
fn indented_lines_belong_to_the_key_above_them() {
    // The block sequence under `tags` is not three keys of its own, and the
    // comment between the keys is not a key either.
    let text = fixture("notes/yaml-comments.md");
    let scan = write::scan(&text).expect("the scan");
    let keys: Vec<&str> = scan.keys.iter().map(|key| key.key.as_str()).collect();
    assert_eq!(keys, ["title", "date", "tags", "status"]);
}

// ───────────────────────────── the spans ──────────────────────────────────────

#[test]
fn case_1_17_every_span_lies_inside_the_frontmatter_block() {
    for rel in every_fixture() {
        let input = fixture(&rel);
        let scan = write::scan(&input).unwrap_or_else(|err| panic!("{rel}: {err}"));
        for key in &scan.keys {
            assert!(
                scan.span.start <= key.value.start && key.value.start <= key.value.end,
                "{rel} {}: inverted or early span",
                key.key
            );
            assert!(
                key.value.end <= scan.span.end,
                "{rel} {}: span ends past the block",
                key.key
            );
            if let Some(comment) = key.value.comment {
                assert!(
                    key.value.end <= comment && comment < scan.span.end,
                    "{rel} {}: comment outside the block",
                    key.key
                );
            }
            assert!(
                input.is_char_boundary(key.value.start) && input.is_char_boundary(key.value.end),
                "{rel} {}: span splits a character",
                key.key
            );
        }
    }
}

#[test]
fn the_scanner_and_the_reader_agree_on_the_block() {
    // The scanner repeats the reader's block rules so the writer needs no
    // config. This keeps the two from drifting apart silently.
    let config = config();
    for rel in every_fixture() {
        let input = fixture(&rel);
        let read = frontmatter::read(&input, &config);
        let scan = write::scan(&input).unwrap_or_else(|err| panic!("{rel}: {err}"));
        assert_eq!(scan.span, read.span, "{rel}: block span differs");
        assert_eq!(
            scan.span.is_empty(),
            read.raw.is_none(),
            "{rel}: block presence"
        );
        if let Some(raw) = read.raw {
            // The YAML runs from after the opening fence to the closing fence.
            let open = raw.find('\n').map_or(0, |offset| offset + 1);
            let close = raw[..raw.len() - 1]
                .rfind('\n')
                .map_or(0, |offset| offset + 1);
            assert_eq!(
                &input[scan.yaml_start..scan.yaml_end],
                &raw[open..close],
                "{rel}: YAML extent differs"
            );
        } else {
            assert_eq!(
                scan.yaml_start, scan.yaml_end,
                "{rel}: an absent block has a region"
            );
        }
    }
}

#[test]
fn a_document_without_a_block_has_no_keys_and_no_span() {
    for text in [
        "# No frontmatter\n\nbody\n",
        "\u{feff}# No frontmatter\n\nbody\n",
        "---\nnever closed\n",
    ] {
        let scan = write::scan(text).expect("no block is not an error here");
        assert!(scan.keys.is_empty(), "{text:?}");
        assert!(scan.span.is_empty(), "{text:?}");
        assert_eq!(scan.yaml_start, scan.yaml_end, "{text:?}");
        // The empty span sits after any BOM, which is where a block would go.
        let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
        assert_eq!(
            scan.span,
            FrontmatterSpan {
                start: bom,
                end: bom
            },
            "{text:?}"
        );
    }
}

#[test]
fn the_scanner_module_reads_and_writes_no_file() {
    for (name, source) in [
        ("mod.rs", include_str!("../src/write/mod.rs")),
        ("scan.rs", include_str!("../src/write/scan.rs")),
    ] {
        for forbidden in ["std::fs", "std::io", "File::", "tempfile", "Lock"] {
            assert!(
                !source.contains(forbidden),
                "src/write/{name} mentions {forbidden}"
            );
        }
    }
}

/// Every Markdown fixture, recursively, sorted.
fn every_fixture() -> Vec<String> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("fixture tree").flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, out);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                out.push(
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut out = Vec::new();
    visit(Path::new(FIXTURE), Path::new(FIXTURE), &mut out);
    out.sort();
    assert!(out.len() >= 15);
    out
}

// ───────────────────────────── properties ─────────────────────────────────────

proptest! {
    /// Whatever the document, every span the scanner reports is inside the
    /// block and on a character boundary (case 1.17 as a property).
    #[test]
    fn every_reported_span_is_inside_the_block(
        pairs in prop::collection::vec(("[a-z]{1,6}", "[ -~]{0,20}"), 0..4),
        body in "\\PC{0,40}",
        crlf in any::<bool>(),
    ) {
        let newline = if crlf { "\r\n" } else { "\n" };
        let yaml: Vec<String> = pairs.iter().map(|(key, value)| format!("{key}: {value}")).collect();
        let yaml = yaml.join(newline);
        let text = format!("---{newline}{yaml}{newline}---{newline}{body}");
        let scan = write::scan(&text).map_err(|err| TestCaseError::fail(err.to_string()))?;

        prop_assert_eq!(scan.yaml_start <= scan.yaml_end, true);
        prop_assert_eq!(scan.span.start, 0);
        for key in &scan.keys {
            prop_assert!(scan.span.start <= key.value.start);
            prop_assert!(key.value.start <= key.value.end);
            prop_assert!(key.value.end <= scan.span.end);
            prop_assert!(text.is_char_boundary(key.value.start));
            prop_assert!(text.is_char_boundary(key.value.end));
            prop_assert!(key.line >= 2);
        }
    }

    /// A generated key line always scans as one key, whatever the value.
    #[test]
    fn a_generated_scalar_line_scans_as_one_key(value in "\\PC{0,20}") {
        let text = format!("---\nstatus: {value}\n---\nbody\n");
        let scan = write::scan(&text).map_err(|err| TestCaseError::fail(err.to_string()))?;
        prop_assert_eq!(scan.keys.len(), 1);
        prop_assert_eq!(&scan.keys[0].key, "status");
        prop_assert!(scan.keys[0].value.start <= scan.keys[0].value.end);
        prop_assert!(scan.keys[0].value.end <= text.find("\n---\n").ok_or_else(
            || TestCaseError::fail("closing fence not found".to_owned())
        )?);
    }
}
