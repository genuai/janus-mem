//! The M7 patcher against the M7 golden corpus: cases 1.1 to 1.17 (SPEC §7.3).
//!
//! Two halves, in this order. The scanner half locates bytes: which key sits on
//! which line, where its value starts and ends, where a trailing comment begins,
//! and which shapes it refuses. The planner half asserts the splice and the
//! refusal answers. A case that asserts a shape is in the first half; the same
//! case is asserted again as a refusal where `plan` is the thing under test.
//!
//! The corpus is the fixture vault M2 built for this phase
//! (`tests/fixtures/vault`); no second corpus is created here. Every case is
//! named as the phase plan names it.

use std::fs;
use std::path::{Path, PathBuf};

use janus_mem::config::{Config, Field};
use janus_mem::document::{DocNote, FrontmatterSpan};
use janus_mem::id::Blake3Hash;
use janus_mem::vault::frontmatter;
use janus_mem::write::{self, Patch, PatchError, Plan, ValueShape};
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

fn patch(keys: &[(&str, Option<&str>)]) -> Patch {
    Patch {
        path: PathBuf::from("notes/test.md"),
        keys: keys
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.map(str::to_owned)))
            .collect(),
        expected_hash: None,
        create_frontmatter: false,
        dry_run: false,
    }
}

fn typed(text: &str) -> janus_mem::document::Frontmatter {
    frontmatter::read(text, &config()).frontmatter
}

/// Asserts a document re-reads as valid frontmatter plus a body (I4).
fn assert_reads_as_frontmatter(text: &str) {
    let read = frontmatter::read(text, &config());
    assert!(read.raw.is_some(), "no frontmatter block in:\n{text}");
    assert!(!read.span.is_empty(), "empty frontmatter span in:\n{text}");
    let unparseable: Vec<&DocNote> = read
        .notes
        .iter()
        .filter(|note| matches!(note, DocNote::YamlUnparseable { .. }))
        .collect();
    assert!(
        unparseable.is_empty(),
        "output does not parse as YAML: {unparseable:?}"
    );
}

/// Asserts every key not named in `patched` kept its value bytes and its order.
fn assert_other_keys_untouched(input: &str, output: &str, patched: &[&str]) {
    let before = write::scan(input).expect("the input scans");
    let after = write::scan(output).expect("the output scans");
    let before_keys: Vec<&str> = before.keys.iter().map(|k| k.key.as_str()).collect();
    let after_keys: Vec<&str> = after.keys.iter().map(|k| k.key.as_str()).collect();
    // A key that was absent is appended after the last key, so the keys that
    // were already there must keep their order.
    let kept: Vec<&str> = after_keys
        .iter()
        .copied()
        .filter(|key| before_keys.contains(key))
        .collect();
    assert_eq!(kept, before_keys, "key order changed");
    for key in &before.keys {
        if patched.contains(&key.key.as_str()) {
            continue;
        }
        let found = after
            .keys
            .iter()
            .find(|other| other.key == key.key)
            .unwrap_or_else(|| panic!("key {} disappeared", key.key));
        assert_eq!(
            &input[key.value.start..key.value.end],
            &output[found.value.start..found.value.end],
            "key {} changed",
            key.key
        );
    }
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

// ──────────────────────── the splice and the refusals ────────────────────────

#[test]
fn case_1_1_patch_replaces_one_span_and_nothing_else() {
    let input = fixture("notes/block-sequence.md");
    let plan = write::plan(&input, &config(), &patch(&[("status", Some("superseded"))]))
        .expect("in budget");

    assert_eq!(
        plan.text,
        "\
---
title: Block sequence
date: 2025-03-09
# A block sequence: M7 must refuse to patch it, not rewrite it as `[a, b]` (§7.3).
tags:
  - splice
  - block
status: superseded
---
# Block sequence

`tags` is a block sequence. A patch that replaced it would reflow three lines into
one, so M7 refuses with `UnsupportedPatch { key: \"tags\" }` and suggests
`write_document`.
"
    );

    assert_eq!(plan.changed_keys, ["status"]);
    assert!(plan.appended_keys.is_empty());
    assert_eq!(plan.original, input.as_bytes());
    assert_eq!(plan.bytes_before, input.len());
    assert_eq!(plan.bytes_after, plan.text.len());
    assert_eq!(
        plan.bytes_after - plan.bytes_before,
        "superseded".len() - "active".len()
    );
    // The hashes are the etag a read reports, so the receipt can be the next
    // `expected_hash` (case 2.6).
    assert_eq!(
        plan.old_hash,
        Blake3Hash::of_bytes(input.as_bytes()).to_hex()
    );
    assert_eq!(
        plan.new_hash,
        Blake3Hash::of_bytes(plan.text.as_bytes()).to_hex()
    );
    assert_other_keys_untouched(&input, &plan.text, &["status"]);
}

#[test]
fn case_1_2_crlf_is_kept_and_the_appended_line_is_crlf() {
    let input = fixture("specs/crlf-spec.md");
    assert!(input.contains("\r\n"), "the fixture must be CRLF");

    // One existing value: the byte delta is the value delta, CRLF count equal.
    let plan = write::plan(
        &input,
        &config(),
        &patch(&[("written_by", Some("rehearsal"))]),
    )
    .expect("scalar");
    assert_eq!(
        plan.bytes_after as i64 - plan.bytes_before as i64,
        "rehearsal".len() as i64 - "fixture-generator".len() as i64
    );
    assert_eq!(
        plan.text.matches("\r\n").count(),
        input.matches("\r\n").count()
    );
    assert!(plan.text.contains("written_by: rehearsal\r\n"));
    assert_eq!(
        plan.text.matches('\n').count(),
        plan.text.matches("\r\n").count(),
        "a bare LF appeared in a CRLF file"
    );

    // One appended key: exactly one more CRLF, and the line ends with CRLF.
    let plan = write::plan(
        &input,
        &config(),
        &patch(&[("superseded_by", Some("specs/2026-01-01-new.md"))]),
    )
    .expect("append");
    assert_eq!(
        plan.text.matches("\r\n").count(),
        input.matches("\r\n").count() + 1
    );
    assert_eq!(
        plan.bytes_after as i64 - plan.bytes_before as i64,
        "superseded_by: specs/2026-01-01-new.md\r\n".len() as i64
    );
    assert!(
        plan.text
            .contains("superseded_by: specs/2026-01-01-new.md\r\n---\r\n")
    );
    assert_eq!(plan.appended_keys, ["superseded_by"]);
    assert_reads_as_frontmatter(&plan.text);
    assert_eq!(typed(&plan.text).superseded_by, ["specs/2026-01-01-new.md"]);
}

#[test]
fn case_1_3_bom_stays_at_byte_zero() {
    let input = fixture("notes/bom-note.md");
    assert!(input.starts_with('\u{feff}'));

    let plan =
        write::plan(&input, &config(), &patch(&[("status", Some("superseded"))])).expect("append");
    assert_eq!(
        plan.text,
        "\u{feff}---
title: BOM note
date: 2025-03-02
tags: [bom]
status: superseded
---
# BOM note

This file starts with a UTF-8 BOM, which must not be part of the YAML block.
"
    );
    assert_eq!(&plan.text[..3], "\u{feff}");
    assert_eq!(write::scan(&plan.text).expect("scan").span.start, 3);
}

#[test]
fn case_1_4_trailing_comment_and_its_spaces_survive() {
    let input = fixture("notes/quoted-scalars.md");
    let plan =
        write::plan(&input, &config(), &patch(&[("status", Some("superseded"))])).expect("scalar");

    assert_eq!(
        plan.text,
        r#"---
title: "Quoted: a scoped title"
date: 2025-03-13
# A trailing comment on a key, and escapes inside quotes. `# not a comment` is
# literal text, so a naive line scanner would truncate the value there.
note: 'it''s fine'
motto: "say \"hello\" # not a comment"
status: superseded  # this comment must survive a patch to this line
---
# Quoted scalars

The quotes and escapes are part of the file, not decoration: a patcher that
re-emitted `motto` would have to re-escape it perfectly or change its value.
"#
    );
    assert_other_keys_untouched(&input, &plan.text, &["status"]);
}

#[test]
fn case_1_5_block_sequence_is_refused_and_the_input_is_unchanged() {
    let input = fixture("notes/block-sequence.md");
    let error =
        write::plan(&input, &config(), &patch(&[("tags", Some("splice"))])).expect_err("refused");

    assert_eq!(
        error,
        PatchError::UnsupportedShape {
            key: "tags".to_owned(),
            reason: write::reason::BLOCK_SEQUENCE,
        }
    );
    assert_eq!(fixture("notes/block-sequence.md"), input, "the file moved");
}

#[test]
fn case_1_6_multiline_flow_mapping_anchor_and_alias_are_refused_with_their_own_reason() {
    let cases = [
        (
            "notes/multiline-scalar.md",
            "summary",
            write::reason::MULTILINE_SCALAR,
        ),
        (
            "notes/multiline-scalar.md",
            "note",
            write::reason::MULTILINE_SCALAR,
        ),
        (
            "notes/flow-mapping.md",
            "owner",
            write::reason::FLOW_MAPPING,
        ),
        ("notes/anchor-alias.md", "team", write::reason::ANCHOR),
        ("notes/anchor-alias.md", "reviewer", write::reason::ALIAS),
        (
            "notes/yaml-comments.md",
            "tags",
            write::reason::BLOCK_SEQUENCE,
        ),
    ];

    for (rel, key, reason) in cases {
        let input = fixture(rel);
        match write::plan(&input, &config(), &patch(&[(key, Some("replacement"))])) {
            Ok(plan) => panic!(
                "{rel} {key} was accepted, expected {reason}:\n{}",
                plan.text
            ),
            Err(error) => assert_eq!(
                error,
                PatchError::UnsupportedShape {
                    key: key.to_owned(),
                    reason,
                },
                "{rel} {key}"
            ),
        }
    }
}

#[test]
fn case_1_7_duplicate_top_level_key_is_refused() {
    let duplicated = "---\nstatus: active\nstatus: superseded\n---\nbody\n";
    let error = write::plan(
        duplicated,
        &config(),
        &patch(&[("status", Some("retired"))]),
    )
    .expect_err("refused");
    assert_eq!(
        error,
        PatchError::UnsupportedShape {
            key: "status".to_owned(),
            reason: write::reason::DUPLICATE_KEY,
        }
    );

    // A duplicate elsewhere costs its own key, not the whole file.
    let elsewhere = "---\ntitle: Duplicate tag\ntags: [a]\ntags: [b]\nstatus: active\n---\nbody\n";
    let plan = write::plan(elsewhere, &config(), &patch(&[("status", Some("retired"))]))
        .expect("other keys");
    assert_eq!(
        plan.text,
        "---\ntitle: Duplicate tag\ntags: [a]\ntags: [b]\nstatus: retired\n---\nbody\n"
    );
}

#[test]
fn case_1_8_flow_sequence_of_scalars_is_replaceable() {
    // The flow mapping next to it keeps its bytes.
    let input = fixture("notes/flow-mapping.md");
    let plan =
        write::plan(&input, &config(), &patch(&[("status", Some("superseded"))])).expect("scalar");
    assert!(plan.text.contains("owner: {name: me, team: infra}\n"));

    // A flow sequence is a scalar list on one line, so one splice replaces it.
    let input = fixture("specs/crlf-spec.md");
    let plan =
        write::plan(&input, &config(), &patch(&[("labels", Some("crlf"))])).expect("flow sequence");
    assert!(plan.text.contains("labels: crlf\r\n"));
    assert!(!plan.text.contains("line-endings"));
    assert_eq!(typed(&plan.text).tags, ["crlf"]);
    assert_reads_as_frontmatter(&plan.text);
}

#[test]
fn case_1_9_absent_key_is_appended_before_the_fence() {
    let input = fixture("notes/undated.md");
    let plan =
        write::plan(&input, &config(), &patch(&[("status", Some("active"))])).expect("append");

    assert_eq!(
        plan.text,
        "\
---
id: fixture-undated
title: Undated note
status: active
---
# Undated note

No frontmatter date, no name date and no folder date, so this is the fixture's one
deliberate `date_source: absent` document. It never decays. The declared id keeps its
identity stable across checkouts.
"
    );
    assert_eq!(plan.appended_keys, ["status"]);
    assert_eq!(plan.changed_keys, ["status"]);

    // Two absent keys land in sorted order, not request order.
    let plan = write::plan(
        &input,
        &config(),
        &patch(&[("status", Some("active")), ("superseded_by", None)]),
    )
    .expect("append");
    assert_eq!(plan.appended_keys, ["status", "superseded_by"]);
    assert!(
        plan.text
            .contains("status: active\nsuperseded_by: null\n---\n")
    );
}

#[test]
fn case_1_10_no_frontmatter_is_refused_unless_creation_is_asked_for() {
    let input = fixture("notes/2025-03-07-no-frontmatter.md");
    let error =
        write::plan(&input, &config(), &patch(&[("status", Some("active"))])).expect_err("refused");
    assert_eq!(error, PatchError::NoFrontmatter);

    let mut creating = patch(&[("status", Some("active"))]);
    creating.create_frontmatter = true;
    let plan = write::plan(&input, &config(), &creating).expect("created");
    assert_eq!(
        plan.text,
        "\
---
status: active
---
# No frontmatter

This file has no YAML block at all. Its date comes from the filename prefix.
"
    );

    // A BOM stays before the new block, and the body is untouched.
    let with_bom = "\u{feff}# Title\n\nBody line.\n";
    let plan = write::plan(with_bom, &config(), &creating).expect("created");
    assert_eq!(
        plan.text,
        "\u{feff}---\nstatus: active\n---\n# Title\n\nBody line.\n"
    );
}

#[test]
fn case_1_10_an_unclosed_fence_is_body_and_a_created_block_goes_before_it() {
    // The reader reports an opening fence with no closing fence as body-only, so
    // a created block goes before it and the stray fence is now the body's first
    // line. No byte is lost, and the new block is the one that gets read.
    let input = "---\njust a body line\n";
    let mut creating = patch(&[("type", Some("note"))]);
    creating.create_frontmatter = true;

    let plan = write::plan(input, &config(), &creating).expect("created");
    assert_eq!(plan.text, "---\ntype: note\n---\n---\njust a body line\n");
    assert_reads_as_frontmatter(&plan.text);
    assert_eq!(typed(&plan.text).r#type.as_deref(), Some("note"));
}

#[test]
fn case_1_11_empty_value_is_a_null_and_is_replaceable() {
    let input = "---\ntitle: Empty value\nstatus:\n---\nbody\n";
    let plan =
        write::plan(input, &config(), &patch(&[("status", Some("active"))])).expect("null value");
    assert_eq!(
        plan.text,
        "---\ntitle: Empty value\nstatus: active\n---\nbody\n"
    );

    // With a trailing comment, the value goes before the spacing and the
    // comment keeps its place.
    let input = "---\nstatus:  # none yet\n---\nbody\n";
    let plan = write::plan(input, &config(), &patch(&[("status", Some("active"))])).expect("value");
    assert_eq!(plan.text, "---\nstatus: active  # none yet\n---\nbody\n");
    assert_reads_as_frontmatter(&plan.text);

    // An empty span already reads as null, so a request for null changes
    // nothing (case 2.10).
    let plan = write::plan(input, &config(), &patch(&[("status", None)])).expect("null value");
    assert_eq!(plan.text, input);
    assert!(plan.changed_keys.is_empty());
}

#[test]
fn case_1_12_written_values_read_back_through_the_typed_reader() {
    // The generator of the plan, one call per alphabet. Every value must be
    // accepted and must read back as the same string.
    let fixed = [
        "true",
        "null",
        "007",
        "1.0",
        "a: b",
        "#x",
        "[x]",
        "- x",
        "&a",
        "*a",
        "'q'",
        "\"q\"",
        "",
        " leading",
        "trailing ",
        "  ",
        "a\nb",
        "a\rb",
        "a\tb",
        "a\0b",
        "a\u{1b}b",
        "a\u{7f}b",
        "café",
        "中文",
        "😀",
        "a b",
        "---",
        "...",
        "0x10",
        "1e5",
        "~",
    ];
    for value in fixed {
        assert_value_round_trips(value);
    }

    // A bare write needs a YAML *string* back, not a coerced scalar. A boolean,
    // a number, a null, and a YAML 1.1 boolean spelling are all quoted, because
    // the file must not declare a type the caller did not ask for.
    for value in [
        "true", "false", "yes", "no", "on", "off", "null", "007", "1.0",
    ] {
        let rendered = write::render_value(Some(value));
        assert!(
            rendered.starts_with('"'),
            "{value:?} was written bare as {rendered}"
        );
        assert_value_round_trips(value);
    }
}

/// Plans one value into a document and reads it back through the typed reader.
fn assert_value_round_trips(value: &str) {
    let input = "---\ntitle: Round trip\nstatus: active\n---\nbody\n";
    let plan = write::plan(input, &config(), &patch(&[("status", Some(value))]))
        .unwrap_or_else(|err| panic!("value {value:?} was refused: {err}"));
    assert_eq!(
        typed(&plan.text).status.as_deref(),
        Some(value),
        "value {value:?} read back differently; written as:\n{}",
        plan.text
    );
    assert_reads_as_frontmatter(&plan.text);
}

#[test]
fn case_1_12a_escaped_newline_and_quote_round_trip() {
    // The parser spike the whole value writer rests on: a newline and a double
    // quote in one value, written double-quoted, read back by oneyaml and by the
    // config's typed reader.
    let value = "line one\nline two \"quoted\" and \\ back";
    let rendered = write::render_value(Some(value));
    assert_eq!(rendered, r#""line one\nline two \"quoted\" and \\ back""#);

    let yaml = format!("status: {rendered}\n");
    let parsed: oneyaml::Value = oneyaml::from_str(&yaml).expect("oneyaml reads the escape");
    assert_eq!(
        parsed
            .as_mapping()
            .and_then(|mapping| mapping.get_str("status"))
            .and_then(oneyaml::Value::as_str),
        Some(value)
    );

    let document = format!("---\n{yaml}---\nbody\n");
    assert_eq!(typed(&document).status.as_deref(), Some(value));
}

#[test]
fn case_1_12b_escape_set_and_every_refused_bare_form_survives_quoted() {
    // The escape set, spelled out.
    let escaped = [
        ("\\", r#""\\""#),
        ("\"", r#""\"""#),
        ("\n", r#""\n""#),
        ("\r", r#""\r""#),
        ("\t", r#""\t""#),
        ("\u{0}", r#""\u0000""#),
        ("\u{1}", r#""\u0001""#),
        ("\u{1f}", r#""\u001F""#),
        ("\u{7f}", r#""\u007F""#),
        ("a\u{0}b", r#""a\u0000b""#),
    ];
    for (value, expected) in escaped {
        assert_eq!(
            write::render_value(Some(value)),
            expected,
            "escape of {value:?}"
        );
    }

    // Bare writes that change meaning, and the quoted form that does not.
    let refused_bare = [
        "a: b", "- x", "#x", "'q'", "\"q\"", "007", "null", "&a", "*a", "[x]", "", "a #b", "a\tb",
        "a\nb", " space", "space ", "~", "x:",
    ];
    for value in refused_bare {
        let rendered = write::render_value(Some(value));
        assert!(
            rendered.starts_with('"') && rendered.ends_with('"'),
            "{value:?} was written bare as {rendered}"
        );
        assert_value_round_trips(value);
    }
}

#[test]
fn case_1_12c_key_outside_the_pattern_is_refused() {
    let input = fixture("notes/undated.md");
    for key in [
        "a\nsuperseded_by: x",
        "a: b",
        "a#b",
        "\"a\"",
        "",
        "-a",
        "a b",
        "ü",
    ] {
        let error = write::plan(&input, &config(), &patch_with_key(key)).expect_err("refused");
        assert_eq!(
            error,
            PatchError::KeyShape {
                key: key.to_owned()
            }
        );
    }
    assert_eq!(fixture("notes/undated.md"), input, "the file moved");
}

/// A patch of one raw key, for the key-pattern case.
fn patch_with_key(key: &str) -> Patch {
    Patch {
        path: PathBuf::from("notes/test.md"),
        keys: vec![(key.to_owned(), Some("x".to_owned()))],
        expected_hash: None,
        create_frontmatter: false,
        dry_run: false,
    }
}

#[test]
fn case_1_13_every_accepted_patch_rereads_as_frontmatter_and_body() {
    let files = [
        "notes/block-sequence.md",
        "notes/flow-mapping.md",
        "notes/anchor-alias.md",
        "notes/multiline-scalar.md",
        "notes/quoted-scalars.md",
        "notes/yaml-comments.md",
        "notes/undated.md",
        "notes/bom-note.md",
        "specs/crlf-spec.md",
        "specs/body-starts-with-rule.md",
        "decisions/2024-11-02-postgres-cache.md",
        "archive/private/secret.md",
        ".scratch.md",
    ];
    for rel in files {
        let input = fixture(rel);
        let scan = write::scan(&input).unwrap_or_else(|err| panic!("{rel}: {err}"));
        let patchable = scan
            .keys
            .iter()
            .find(|key| matches!(key.shape, ValueShape::Scalar | ValueShape::FlowSequence))
            .unwrap_or_else(|| panic!("{rel} has no patchable key"));
        let plan = write::plan(
            &input,
            &config(),
            &patch(&[(&patchable.key, Some("value"))]),
        )
        .unwrap_or_else(|err| panic!("{rel} {}: {err}", patchable.key));
        assert_reads_as_frontmatter(&plan.text);
        assert!(
            plan.text.ends_with(&input[scan.span.end..]),
            "{rel}: body moved"
        );
        assert_eq!(
            &plan.text[..scan.span.start],
            &input[..scan.span.start],
            "{rel}: prefix moved"
        );
    }
}

#[test]
fn case_1_14_other_keys_comments_anchors_and_multiline_strings_are_untouched() {
    for rel in [
        "notes/anchor-alias.md",
        "notes/multiline-scalar.md",
        "notes/flow-mapping.md",
        "notes/yaml-comments.md",
        "notes/quoted-scalars.md",
    ] {
        let input = fixture(rel);
        let plan = write::plan(&input, &config(), &patch(&[("status", Some("superseded"))]))
            .unwrap_or_else(|err| panic!("{rel}: {err}"));
        assert_other_keys_untouched(&input, &plan.text, &["status"]);
        assert!(plan.text.contains("# "), "{rel}: a comment was lost");
    }

    // The block sequence, the anchor line and the folded scalar are still the
    // same bytes after a patch of a different key.
    let input = fixture("notes/anchor-alias.md");
    let plan =
        write::plan(&input, &config(), &patch(&[("title", Some("Renamed"))])).expect("title");
    assert!(
        plan.text
            .contains("team: &platform infra\nreviewer: *platform\n")
    );
}

#[test]
fn case_1_15_two_keys_touch_two_spans_and_nothing_between() {
    let input = fixture("notes/block-sequence.md");
    let plan = write::plan(
        &input,
        &config(),
        &patch(&[
            ("status", Some("superseded")),
            ("title", Some("Block list")),
        ]),
    )
    .expect("two keys");

    assert_eq!(plan.changed_keys, ["status", "title"]);
    assert_eq!(
        plan.bytes_after as i64 - plan.bytes_before as i64,
        ("superseded".len() + "Block list".len()) as i64
            - ("active".len() + "Block sequence".len()) as i64
    );
    assert!(plan.text.contains("title: Block list\n"));
    assert!(plan.text.contains("status: superseded\n"));
    // The block sequence between them, and its comment, are untouched.
    assert!(plan.text.contains("  - splice\n  - block\n"));
    assert_other_keys_untouched(&input, &plan.text, &["status", "title"]);
}

#[test]
fn case_1_16_duplicate_and_empty_requests_are_usage_errors() {
    let input = fixture("notes/undated.md");
    assert_eq!(
        write::plan(&input, &config(), &patch(&[])).expect_err("empty"),
        PatchError::EmptyRequest
    );

    let twice = patch(&[("status", Some("active")), ("status", Some("retired"))]);
    assert_eq!(
        write::plan(&input, &config(), &twice).expect_err("twice"),
        PatchError::DuplicateRequestKey {
            key: "status".to_owned()
        }
    );
}

#[test]
fn a_patch_that_writes_what_the_file_already_says_changes_no_byte() {
    // Decision M7-44: a key counts as changed when its bytes differ.
    let input = fixture("notes/undated.md");
    let plan = write::plan(
        &input,
        &config(),
        &patch(&[("title", Some("Undated note"))]),
    )
    .expect("same value");

    assert!(plan.changed_keys.is_empty(), "{:?}", plan.changed_keys);
    assert!(plan.appended_keys.is_empty());
    assert_eq!(plan.text, input);
    assert_eq!(plan.bytes_before, plan.bytes_after);
    assert_eq!(plan.old_hash, plan.new_hash);
}

// ───────────────────────────── value equality (P2) ───────────────────────────

/// Plans one patch against a document written inline, with the fixture config.
///
/// `plan` reads no file, so a case can state the whole document it compares
/// before and after.
fn planned(text: &str, keys: &[(&str, Option<&str>)]) -> Plan {
    write::plan(text, &config(), &patch(keys)).expect("the patch plans")
}

/// Case 2.1 — a quoted value, patched to the value it holds.
#[test]
fn case_2_1_a_quoted_value_patched_to_the_same_value_changes_no_byte() {
    let input = "---\nstatus: \"active\"\ntitle: Quoted\n---\n\nbody\n";
    let plan = planned(input, &[("status", Some("active"))]);

    assert_eq!(plan.text, input, "the file must keep every byte");
    assert!(plan.changed_keys.is_empty(), "{:?}", plan.changed_keys);
    assert!(plan.appended_keys.is_empty());
    assert_eq!(plan.old_hash, plan.new_hash);
}

/// Case 2.2 — the bare spelling, written again, and the quote characters as a
/// value.
#[test]
fn case_2_2_a_bare_value_written_again_changes_no_byte() {
    let input = "---\nstatus: active\ntitle: Bare\n---\n\nbody\n";
    let plan = planned(input, &[("status", Some("active"))]);
    assert_eq!(plan.text, input);
    assert!(plan.changed_keys.is_empty(), "{:?}", plan.changed_keys);

    // A request is a value, not a rendering: `"active"` with the quote
    // characters in it is another value, so it writes.
    let plan = planned(input, &[("status", Some("\"active\""))]);
    assert_eq!(plan.changed_keys, ["status"]);
    assert_eq!(typed(&plan.text).status.as_deref(), Some("\"active\""));
    assert_ne!(plan.text, input);
}

/// Case 2.3 — a trailing comment belongs to the line, not to the value.
#[test]
fn case_2_3_a_trailing_comment_survives_a_same_value_write() {
    let input = "---\nstatus: active  # current\n---\n\nbody\n";
    let plan = planned(input, &[("status", Some("active"))]);
    assert_eq!(plan.text, input);
    assert!(plan.changed_keys.is_empty(), "{:?}", plan.changed_keys);
}

/// Case 2.4 — the status reader canonicalises the case.
#[test]
fn case_2_4_a_status_in_another_case_changes_no_byte() {
    let input = "---\nstatus: Active\n---\n\nbody\n";
    let plan = planned(input, &[("status", Some("active"))]);
    assert_eq!(plan.text, input);
    assert!(plan.changed_keys.is_empty(), "{:?}", plan.changed_keys);
}

/// Case 2.5 — a free-text key has no reader, so the case is a value.
///
/// Case 2.4 and this one are the pair that proves the rule is per key.
#[test]
fn case_2_5_a_free_text_value_in_another_case_writes() {
    let input = "---\nowner: Active\n---\n\nbody\n";
    let plan = planned(input, &[("owner", Some("active"))]);

    assert_eq!(plan.text, "---\nowner: active\n---\n\nbody\n");
    assert_eq!(plan.changed_keys, ["owner"]);
    assert!(plan.appended_keys.is_empty());
    assert_eq!(
        plan.old_hash,
        Blake3Hash::of_bytes(input.as_bytes()).to_hex()
    );
    assert_ne!(plan.old_hash, plan.new_hash);
}

/// Case 2.6 — a value that differs is replaced, and nothing else moves.
#[test]
fn case_2_6_a_value_that_differs_writes_the_span() {
    let input = "---\ntitle: One\nstatus: active\n---\n\nbody\n";
    let plan = planned(input, &[("status", Some("superseded"))]);

    assert_eq!(
        plan.text,
        "---\ntitle: One\nstatus: superseded\n---\n\nbody\n"
    );
    assert_eq!(plan.changed_keys, ["status"]);
    assert_other_keys_untouched(input, &plan.text, &["status"]);
}

/// Case 2.9 — an alias and its canonical name are one type.
#[test]
fn case_2_9_an_alias_and_its_canonical_type_are_one_value() {
    let input = "---\ntype: adr\n---\n\nbody\n";
    let plan = planned(input, &[("type", Some("decision"))]);
    assert_eq!(plan.text, input);
    assert!(plan.changed_keys.is_empty(), "{:?}", plan.changed_keys);

    // A different type is a different value, so it writes.
    let plan = planned(input, &[("type", Some("spec"))]);
    assert_eq!(plan.changed_keys, ["type"]);
}

/// Case 2.10 — an empty span is a null, and a null is one value.
#[test]
fn case_2_10_an_empty_value_is_a_null() {
    let input = "---\nstatus:\n---\n\nbody\n";

    let plan = planned(input, &[("status", Some("active"))]);
    assert_eq!(plan.text, "---\nstatus: active\n---\n\nbody\n");
    assert_eq!(plan.changed_keys, ["status"]);

    let plan = planned(input, &[("status", None)]);
    assert_eq!(plan.text, input);
    assert!(plan.changed_keys.is_empty(), "{:?}", plan.changed_keys);
    assert_eq!(plan.old_hash, plan.new_hash);
}

/// A timestamp and its date part are one date (§2.2).
#[test]
fn a_timestamp_and_its_date_are_one_value() {
    let input = "---\ndate: 2026-10-29T10:00:00Z\neffective_to: null\n---\n\nbody\n";

    let plan = planned(input, &[("date", Some("2026-10-29"))]);
    assert_eq!(plan.text, input);
    assert!(plan.changed_keys.is_empty(), "{:?}", plan.changed_keys);

    // A different date writes, and so does a value the parser rejects in place
    // of one it accepts.
    let plan = planned(input, &[("date", Some("2026-10-30"))]);
    assert_eq!(plan.changed_keys, ["date"]);
}

/// A value the date parser rejects is not a date, so two are two values.
#[test]
fn two_values_the_date_parser_rejects_are_two_values() {
    let input = "---\ndate: 2026-10\n---\n\nbody\n";

    let plan = planned(input, &[("date", Some("2026-11"))]);
    assert_eq!(plan.changed_keys, ["date"], "a rejected date is its bytes");
    assert_eq!(plan.text, "---\ndate: 2026-11\n---\n\nbody\n");
}

/// A list and a null are two values: a reader that drops the list must not make
/// a request for null a no-op.
#[test]
fn a_flow_sequence_and_a_null_are_two_values() {
    let input = "---\ntags: [db, rust]\n---\n\nbody\n";
    let plan = planned(input, &[("tags", None)]);
    assert_eq!(plan.changed_keys, ["tags"]);
    assert_eq!(plan.text, "---\ntags: null\n---\n\nbody\n");
}

// ───────────────────────── the plan's invariants ──────────────────────────────

proptest! {
    /// I1, I2, I4 and I5 over a generated document, with a generated value.
    #[test]
    fn invariants_hold_for_a_generated_patch(
        value in any_value(),
        crlf in any::<bool>(),
        bom in any::<bool>(),
        with_anchor in any::<bool>(),
    ) {
        let newline = if crlf { "\r\n" } else { "\n" };
        let mut input = String::new();
        if bom {
            input.push('\u{feff}');
        }
        input.push_str("---");
        input.push_str(newline);
        input.push_str("# owner: platform");
        input.push_str(newline);
        input.push_str("title: \"A title\"");
        input.push_str(newline);
        input.push_str("status: active   # current");
        input.push_str(newline);
        input.push_str("tags:");
        input.push_str(newline);
        input.push_str("  - db");
        input.push_str(newline);
        if with_anchor {
            input.push_str("team: &platform infra");
            input.push_str(newline);
            input.push_str("reviewer: *platform");
            input.push_str(newline);
        }
        input.push_str("---");
        input.push_str(newline);
        input.push_str("Body line one.");
        input.push_str(newline);
        input.push_str("Body line two.");
        input.push_str(newline);

        let plan = write::plan(&input, &config(), &patch(&[("status", Some(value.as_str()))]))
            .map_err(|err| TestCaseError::fail(err.to_string()))?;

        // I2: newline style and BOM. The value is escaped, so no line is added.
        prop_assert_eq!(plan.text.matches('\n').count(), input.matches('\n').count());
        prop_assert_eq!(
            plan.text.matches("\r\n").count(),
            input.matches("\r\n").count()
        );
        prop_assert_eq!(plan.text.starts_with('\u{feff}'), bom);

        // I1 and I5: the prefix, the body, and every other key's bytes.
        let scan = write::scan(&input).expect("input scans");
        prop_assert_eq!(&plan.text[..scan.span.start], &input[..scan.span.start]);
        prop_assert!(plan.text.ends_with(&input[scan.span.end..]));
        let after = write::scan(&plan.text).expect("output scans");
        for key in &scan.keys {
            if key.key == "status" {
                continue;
            }
            let found = after.keys.iter().find(|other| other.key == key.key).expect("key kept");
            prop_assert_eq!(
                &input[key.value.start..key.value.end],
                &plan.text[found.value.start..found.value.end]
            );
        }

        // I4: the output reads back as frontmatter plus body, and the value is
        // the string that was asked for.
        prop_assert!(frontmatter::read(&plan.text, &config()).raw.is_some());
        let read = typed(&plan.text);
        prop_assert_eq!(read.status.as_deref(), Some(value.as_str()));
    }

    /// I3: a refused shape yields no bytes at all. The generator builds a
    /// document whose `tags` key is a block collection of random depth, so a
    /// patched `tags` must be refused and a patched scalar must still work.
    #[test]
    fn a_refused_patch_yields_no_bytes(
        lines in prop::collection::vec("[a-z]{0,6}", 1..4),
    ) {
        let mut input = String::from("---\nstatus: active\ntags:\n");
        for line in &lines {
            input.push_str("  - ");
            input.push_str(line);
            input.push('\n');
        }
        input.push_str("---\nbody\n");

        let refused = write::plan(&input, &config(), &patch(&[("tags", Some("x"))]));
        prop_assert_eq!(
            refused,
            Err(PatchError::UnsupportedShape {
                key: "tags".to_owned(),
                reason: write::reason::BLOCK_SEQUENCE,
            })
        );

        let accepted = write::plan(&input, &config(), &patch(&[("status", Some("retired"))]))
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        prop_assert!(accepted.text.contains("---\nbody\n"));
        prop_assert!(accepted.text.starts_with("---\nstatus: retired\ntags:\n"));
    }

    /// Case 1.12 as a property: any value at all, written by the writer, reads
    /// back through the typed reader as the same string.
    #[test]
    fn any_value_round_trips_through_the_typed_reader(value in any_value()) {
        let input = "---\nstatus: active\n---\nbody\n";
        let plan = write::plan(input, &config(), &patch(&[("status", Some(value.as_str()))]))
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        let read = frontmatter::read(&plan.text, &config());
        prop_assert!(read.raw.is_some());
        prop_assert_eq!(
            read.frontmatter.status.as_deref(),
            Some(value.as_str()),
            "written as {:?}",
            plan.text
        );
    }
}

/// The value alphabet of case 1.12.
fn any_value() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(vec![
            "true", "null", "007", "1.0", "a: b", "#x", "[x]", "- x", "&a", "*a", "'q'", "\"q\"",
        ])
        .prop_map(str::to_owned),
        Just(String::new()),
        Just(" leading".to_owned()),
        Just("trailing ".to_owned()),
        "[ -~]{0,24}",
        "\\PC{0,16}",
        "[\\n\\r\\t\\x00\\x1b\\x7f]{1,4}",
        "[^\\x00-\\x7f]{1,6}",
    ]
}
