//! The chunker over the committed fixture vault.
//!
//! The unit tests pin the rules on hand-written bodies; this file checks that
//! the rules survive real files — a BOM, CRLF bodies, a document with no
//! frontmatter, unicode filenames and the generated 2 MB document — and that
//! chunking is deterministic across runs and line-ending styles.

mod common;

use janus_mem::chunk::{Chunk, chunk_body};
use janus_mem::config::{ChunkingConfig, Config};
use janus_mem::document::Document;
use janus_mem::vault::resolve::resolve_all;
use janus_mem::vault::{WalkOptions, walk};

fn config(root: &std::path::Path) -> Config {
    Config::load(root).expect("the fixture config must parse")
}

fn documents(root: &std::path::Path) -> Vec<Document> {
    let config = config(root);
    let walked = walk(root, &config, WalkOptions::hermetic()).expect("the fixture must walk");
    let (documents, _) = resolve_all(&walked.files, &config);
    documents
}

fn chunking(config: &Config) -> ChunkingConfig {
    config.chunking
}

/// Asserts the invariants every fixture document's chunks must satisfy.
fn assert_well_formed(document: &Document, chunks: &[Chunk], oversized: bool, limit: usize) {
    let body = &document.body;
    let context = &document.rel_path;
    let mut previous_start = 0;

    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.ordinal, index as u16, "{context}: ordinals");
        assert_eq!(chunk.doc_id, document.id, "{context}: doc id");

        let (start, end) = (chunk.span.0 as usize, chunk.span.1 as usize);
        assert!(start < end, "{context}: empty span");
        assert!(end <= body.len(), "{context}: span past the body");
        assert!(
            body.is_char_boundary(start) && body.is_char_boundary(end),
            "{context}: span splits a character"
        );
        assert_eq!(chunk.text(body), &body[start..end], "{context}: span/text");

        let text = chunk.text(body);
        assert!(!text.trim().is_empty(), "{context}: whitespace-only chunk");
        let chars = text.chars().filter(|character| *character != '\r').count();
        assert!(
            chars <= limit || oversized,
            "{context}: chunk of {chars} characters exceeds {limit} with no note"
        );

        if index > 0 {
            assert!(start > previous_start, "{context}: chunks must advance");
        }
        previous_start = start;
    }
}

#[test]
fn every_fixture_document_chunks_deterministically() {
    let vault = common::fixture_vault();
    let config = config(vault.path());
    let chunking = chunking(&config);
    let limit = chunking.max_chars.max(1);
    let documents = documents(vault.path());
    assert_eq!(documents.len(), 13, "fixture document count changed");

    for document in &documents {
        let (chunks, notes) = chunk_body(document.id, &document.body, &chunking);
        assert!(
            !chunks.is_empty(),
            "{}: a document with content must chunk",
            document.rel_path
        );
        let oversized = notes.iter().any(|note| note.code() == "oversized_block");
        assert_well_formed(document, &chunks, oversized, limit);

        let (again, _) = chunk_body(document.id, &document.body, &chunking);
        assert_eq!(
            chunks, again,
            "{}: chunking is not deterministic",
            document.rel_path
        );
    }
}

#[test]
fn the_large_document_splits_into_many_chunks() {
    let vault = common::fixture_vault();
    let config = config(vault.path());
    let chunking = chunking(&config);
    let document = documents(vault.path())
        .into_iter()
        .find(|document| document.rel_path == common::LARGE_REL)
        .expect("the harness generates the large document");

    let (chunks, notes) = chunk_body(document.id, &document.body, &chunking);
    assert!(chunks.len() > 100, "got {} chunks", chunks.len());
    assert!(notes.is_empty(), "short paragraphs are never oversized");

    // Every chunk is within the limit and they are in source order.
    for chunk in &chunks {
        let text = chunk.text(&document.body);
        assert!(text.chars().count() <= chunking.max_chars);
        assert!(!text.trim().is_empty());
    }
    assert!(
        chunks
            .windows(2)
            .all(|pair| pair[0].span.0 < pair[1].span.0)
    );

    // The fixture config keeps the default 15% overlap, so consecutive chunks
    // share their tail; the union of spans must still cover the body.
    assert!(
        chunks
            .windows(2)
            .any(|pair| pair[0].span.1 > pair[1].span.0),
        "overlap should repeat part of the previous chunk"
    );
    let mut covered = 0;
    let mut cursor = 0;
    for chunk in &chunks {
        let start = (chunk.span.0 as usize).max(cursor);
        let end = chunk.span.1 as usize;
        if end > start {
            covered += end - start;
            cursor = end;
        }
    }
    assert!(
        covered > document.body.len() * 9 / 10,
        "chunks cover {covered} of {} body bytes",
        document.body.len()
    );
}

#[test]
fn line_endings_do_not_change_chunk_identities() {
    let vault = common::fixture_vault();
    let config = config(vault.path());
    let chunking = chunking(&config);
    let document = documents(vault.path())
        .into_iter()
        .find(|document| document.rel_path == "specs/crlf-spec.md")
        .expect("the CRLF fixture exists");

    assert!(
        document.body.contains("\r\n"),
        "the fixture is supposed to carry CRLF"
    );
    let (crlf, _) = chunk_body(document.id, &document.body, &chunking);
    let lf_body = document.body.replace("\r\n", "\n");
    let (lf, _) = chunk_body(document.id, &lf_body, &chunking);

    assert_eq!(
        crlf.iter().map(|chunk| chunk.id).collect::<Vec<_>>(),
        lf.iter().map(|chunk| chunk.id).collect::<Vec<_>>(),
        "a checkout with autocrlf must not re-key chunks (CH-9)"
    );
}

#[test]
fn a_document_without_frontmatter_chunks_from_its_body() {
    let vault = common::fixture_vault();
    let config = config(vault.path());
    let document = documents(vault.path())
        .into_iter()
        .find(|document| document.rel_path == "notes/2025-03-07-no-frontmatter.md")
        .expect("the no-frontmatter fixture exists");

    let (chunks, _) = chunk_body(document.id, &document.body, &chunking(&config));
    assert!(!chunks.is_empty());
    for chunk in &chunks {
        assert!(
            !document.body[chunk.span.0 as usize..chunk.span.1 as usize]
                .trim()
                .is_empty(),
            "every chunk has content"
        );
    }
}
