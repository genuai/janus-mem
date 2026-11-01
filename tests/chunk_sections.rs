//! The section scan must be well-formed over the committed fixture vault, not
//! just over hand-written strings.
//!
//! This is M2 phase 1's acceptance test (`docs/m2-breakdown.md`): the event
//! scanner is only correct if every byte span it reports can be sliced out of
//! the body it was derived from — including the CRLF, BOM, no-frontmatter and
//! generated 2 MB documents.

mod common;

use janus_mem::chunk::{BlockKind, Section, sections};
use janus_mem::config::Config;
use janus_mem::document::Document;
use janus_mem::vault::resolve::resolve_all;
use janus_mem::vault::{WalkOptions, walk};

/// Every fixture document, resolved through the real pipeline.
fn documents(root: &std::path::Path) -> Vec<Document> {
    let config = Config::load(root).expect("the fixture config must parse");
    let walked = walk(root, &config, WalkOptions::hermetic()).expect("the fixture must walk");
    let (documents, skips) = resolve_all(&walked.files, &config);
    assert!(
        skips.is_empty()
            || skips
                .iter()
                .all(|skip| skip.note.code() == "non_utf8_content"),
        "unexpected read skips: {skips:?}"
    );
    documents
}

/// Asserts the invariants every `Section` must satisfy against its body.
fn assert_well_formed(document: &Document, sections: &[Section]) {
    let body = &document.body;
    let context = &document.rel_path;

    let mut previous_end = 0;
    for section in sections {
        assert!(!section.blocks.is_empty(), "{context}: empty section");
        assert!(
            section.span.start < section.span.end && section.span.end <= body.len(),
            "{context}: section span {:?} outside 0..{}",
            section.span,
            body.len()
        );
        assert!(
            body.is_char_boundary(section.span.start) && body.is_char_boundary(section.span.end),
            "{context}: section span {:?} splits a character",
            section.span
        );
        assert!(
            section.span.start >= previous_end,
            "{context}: sections overlap at {:?} (previous end {previous_end})",
            section.span
        );
        previous_end = section.span.end;

        let mut block_end = section.span.start;
        for block in &section.blocks {
            assert!(
                block.span.start >= section.span.start && block.span.end <= section.span.end,
                "{context}: block {:?} escapes section {:?}",
                block.span,
                section.span
            );
            assert!(
                block.span.start >= block_end,
                "{context}: blocks overlap at {:?}",
                block.span
            );
            block_end = block.span.end;

            let text = &body[block.span.clone()];
            assert!(!text.trim().is_empty(), "{context}: empty block {text:?}");
            assert!(
                !text.starts_with('\r') && !text.starts_with('\n'),
                "{context}: block starts with a bare newline"
            );
            // A span must never end between the bytes of a CRLF pair: that would
            // split a line ending in two on a body read as bytes (§8.7).
            assert!(
                !(block.span.end < body.len()
                    && body.as_bytes().get(block.span.end.saturating_sub(1)) == Some(&b'\r')
                    && body.as_bytes().get(block.span.end) == Some(&b'\n')),
                "{context}: block {:?} splits a CRLF pair",
                block.span
            );

            if block.kind == BlockKind::List {
                assert!(!block.items.is_empty(), "{context}: list without items");
                for item in &block.items {
                    assert!(
                        item.start >= block.span.start && item.end <= block.span.end,
                        "{context}: item {item:?} escapes list {:?}",
                        block.span
                    );
                    assert!(
                        !body[item.clone()].trim().is_empty(),
                        "{context}: empty list item"
                    );
                }
            } else {
                assert!(
                    block.items.is_empty(),
                    "{context}: items on a {:?}",
                    block.kind
                );
            }
        }
    }
}

#[test]
fn every_fixture_document_scans_into_well_formed_sections() {
    let vault = common::fixture_vault();
    let documents = documents(vault.path());
    assert_eq!(documents.len(), 13, "fixture document count changed");

    for document in &documents {
        let sections = sections(&document.body);
        assert_well_formed(document, &sections);
    }
}

#[test]
fn the_generated_large_document_is_scanned_completely() {
    let vault = common::fixture_vault();
    let documents = documents(vault.path());
    let large = documents
        .iter()
        .find(|document| document.rel_path == common::LARGE_REL)
        .expect("the large fixture document is generated by the harness");

    assert!(large.body.len() > 1_000_000, "the fixture really is large");
    let sections = sections(&large.body);
    assert!(!sections.is_empty());
    let last = sections.last().expect("sections are non-empty");
    assert!(
        large.body[last.span.end..].trim().is_empty(),
        "the scan must consume the body up to its trailing whitespace"
    );
    assert_well_formed(large, &sections);
}

#[test]
fn a_document_with_no_frontmatter_still_scans() {
    let vault = common::fixture_vault();
    let documents = documents(vault.path());
    let document = documents
        .iter()
        .find(|document| document.rel_path == "notes/2025-03-07-no-frontmatter.md")
        .expect("the no-frontmatter fixture exists");

    assert!(document.raw_frontmatter.is_none());
    let sections = sections(&document.body);
    assert!(!sections.is_empty(), "body-only documents must chunk");
    assert_well_formed(document, &sections);
}
