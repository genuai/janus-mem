//! Cases 3.1–3.8 of the M6 plan: one document, and one chunk of it (M6 §3.3).
//!
//! Every expected value is written by hand from the file the test writes,
//! including the exact truncated text. `path_escape` and `not_found` run
//! through the binary, because the CLI is the surface that exits on them
//! (§9.1, M6-15). No test reads the wall clock or sleeps.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{engine, fixture_vault, run, write_document};
use janus_mem::api::{Call, Tool};
use janus_mem::engine::Engine;
use janus_mem::id::Blake3Hash;
use janus_mem::index::Snapshot;
use janus_mem::index::store::Lock;
use serde_json::{Value, json};

const POSTGRES: &str = "decisions/2024-11-02-postgres-cache.md";

/// The body of [`POSTGRES`]: the file bytes after the closing fence.
const POSTGRES_BODY: &str = "# Postgres cache\n\nFirst link in the supersession cycle. M1 \
     parses and echoes the link; validation\narrives with M5.\n";

/// One `read_document` call with the parameters a case needs.
fn read(params: Value) -> Call {
    Call::new(Tool::ReadDocument, params)
}

/// One read that must answer, returning its envelope.
fn answer(engine: &Engine, params: Value) -> Value {
    let response = engine.call(&read(params));
    assert!(response.ok, "the read failed: {}", response.json);
    response.json
}

/// The blake3 of one file's bytes, as the answer reports it.
fn hash_of(path: &Path) -> String {
    Blake3Hash::of_bytes(&fs::read(path).expect("the file bytes")).to_hex()
}

/// A corpus with one config line and no `[mcp]` section.
fn scratch(id: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        format!("index_dir = \".janus\"\ncorpus_id = \"{id}\"\n"),
    )
    .expect("write the config");
    dir
}

/// The published snapshot, read straight from `.janus/index.bin`.
fn generation(root: &Path) -> Snapshot {
    let bytes = fs::read(root.join(".janus/index.bin")).expect("the built generation");
    Snapshot::from_bytes(&bytes).expect("a valid snapshot")
}

/// The indexed hash of one document, from its row.
fn indexed_hash(root: &Path, rel_path: &str) -> String {
    let snapshot = generation(root);
    let row = snapshot
        .docs
        .iter()
        .find(|row| snapshot.strings.get(row.rel_path) == Some(rel_path))
        .expect("the document's row");
    row.content_hash.to_hex()
}

// ───────────────────────────── one document ────────────────────────────────

#[test]
fn case_3_1_one_read_returns_the_file_it_read() {
    let dir = fixture_vault();
    let root = dir.path();
    let report = answer(&engine(root), json!({ "path": POSTGRES }));

    // The typed frontmatter view, key by key: a second parser would disagree.
    assert_eq!(
        report["frontmatter"],
        json!({
            "id": null,
            "author": null,
            "title": "Postgres cache",
            "type": null,
            "status": "superseded",
            "date": null,
            "effective_from": null,
            "effective_to": null,
            "supersedes": ["decisions/2024-12-01-redis-cache.md"],
            "superseded_by": [],
            "doc_version": null,
            "schema_version": null,
            "tags": [],
            "timeless": null,
            "scope": null,
            "shared_with": [],
        })
    );
    assert_eq!(report["body"], json!(POSTGRES_BODY));
    assert_eq!(report["content_hash"], json!(hash_of(&root.join(POSTGRES))));
    assert_eq!(report["rel_path"], json!(POSTGRES));
    let path = report["path"].as_str().expect("the absolute path");
    assert!(path.ends_with(POSTGRES), "{path}");
    assert_eq!(report["truncated"], json!(false));
    assert!(report.get("span").is_none(), "a whole body has no span");
}

#[test]
fn case_3_1b_a_skipped_refresh_does_not_relabel_the_bytes() {
    let dir = fixture_vault();
    let root = dir.path();
    common::index(root);
    let file = root.join(POSTGRES);
    let before = hash_of(&file);
    assert_eq!(
        indexed_hash(root, POSTGRES),
        before,
        "the build saw this file"
    );

    // Edit the frontmatter only: the body, and its hash, stay the same.
    let edited = fs::read_to_string(&file)
        .expect("the document")
        .replace("title: Postgres cache", "title: Postgres cache (draft)");
    fs::write(&file, edited).expect("edit the frontmatter");
    let after = hash_of(&file);
    assert_ne!(after, before, "the file's bytes must have changed");

    let lock =
        Lock::acquire(&root.join(".janus"), Duration::from_secs(1)).expect("hold the writer lock");
    // A refresh would relabel the row, and the busy lock declines one (M6-19).
    let refreshed = engine(root).call(&Call::new(
        Tool::Search,
        json!({ "query": "postgres", "intent": "history" }),
    ));
    assert!(refreshed.ok, "{}", refreshed.json);
    assert_eq!(
        indexed_hash(root, POSTGRES),
        before,
        "the refresh was skipped"
    );

    let report = answer(&engine(root), json!({ "path": POSTGRES }));
    // The hash describes the bytes this call read, never the indexed row.
    assert_eq!(report["content_hash"], json!(after));
    assert_eq!(
        report["frontmatter"]["title"],
        json!("Postgres cache (draft)")
    );
    assert_eq!(report["body"], json!(POSTGRES_BODY));
    drop(lock);

    // A read never refreshes the index; the next call that loads one does, and
    // its row follows the file. The two facts are what make the rule necessary.
    assert!(engine(root).call(&read(json!({ "path": POSTGRES }))).ok);
    assert_eq!(indexed_hash(root, POSTGRES), before);
    assert!(
        engine(root)
            .call(&Call::new(Tool::Search, json!({ "query": "postgres" })))
            .ok
    );
    assert_eq!(indexed_hash(root, POSTGRES), after);
}

// ───────────────────────────── confinement ─────────────────────────────────

/// The failure object a `read --json` command printed.
fn failed(root: &Path, path: &str) -> (Option<i32>, Value) {
    let output = run(root, &["read", path, "--json"]);
    let failure: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (output.status.code(), failure)
}

#[test]
fn case_3_2_a_forbidden_directory_is_a_path_escape() {
    let dir = fixture_vault();
    let root = dir.path();
    // `.janus/` holds the cache, never vault content (§8.6 rule 1), and the
    // file exists, so the refusal is about the directory and not a typo.
    common::index(root);
    let (code, failure) = failed(root, ".janus/manifest.json");
    assert_eq!(code, Some(4));
    assert_eq!(failure["ok"], json!(false));
    assert_eq!(failure["code"], json!("path_escape"));
    assert_eq!(failure["api"], json!(1));
}

#[test]
fn case_3_3_a_path_outside_the_corpus_is_a_path_escape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vault");
    common::copy_tree(Path::new(common::FIXTURE), &root).expect("copy the fixture");
    // The file exists, one level above the corpus.
    fs::write(dir.path().join("outside.md"), "not corpus content\n")
        .expect("write the outside file");

    let (code, failure) = failed(&root, "../outside.md");
    assert_eq!(code, Some(4));
    assert_eq!(failure["code"], json!("path_escape"));
}

#[test]
fn case_3_4_a_missing_path_is_not_found_and_named() {
    let dir = fixture_vault();
    let (code, failure) = failed(dir.path(), "notes/absent.md");
    assert_eq!(code, Some(1));
    assert_eq!(failure["code"], json!("not_found"));
    let message = failure["message"].as_str().expect("the message");
    assert!(message.contains("notes/absent.md"), "{message}");
}

#[test]
fn case_3_3b_an_excluded_path_is_not_found_and_named() {
    // `archive/private/**` is excluded by the fixture config: a legal path the
    // walk refuses, which is "no document", never "no such directory".
    let dir = fixture_vault();
    let root = dir.path();
    write_document(
        root,
        "archive/private/hidden.md",
        "---\ntitle: Hidden\n---\n\nsecret\n",
    );
    let (code, failure) = failed(root, "archive/private/hidden.md");
    assert_eq!(code, Some(1));
    assert_eq!(failure["code"], json!("not_found"));
    let message = failure["message"].as_str().expect("the message");
    assert!(message.contains("archive/private/hidden.md"), "{message}");
}

// ──────────────────────────── the text cap ─────────────────────────────────

#[test]
fn case_3_5_the_cut_counts_characters_not_bytes() {
    let dir = scratch("accents");
    let root = dir.path();
    // Six characters, twelve bytes. A byte counter would answer "ééé" here.
    write_document(root, "notes/accents.md", "---\ntitle: Accents\n---\néééééé");
    let engine = engine(root);

    let five = answer(
        &engine,
        json!({ "path": "notes/accents.md", "max_chars": 5 }),
    );
    assert_eq!(five["body"], json!("ééééé"));
    assert_eq!(five["truncated"], json!(true));

    let six = answer(
        &engine,
        json!({ "path": "notes/accents.md", "max_chars": 6 }),
    );
    assert_eq!(six["body"], json!("éééééé"));
    assert_eq!(six["truncated"], json!(false));
}

#[test]
fn case_3_6_the_cap_is_configured_unless_the_request_names_one() {
    let dir = scratch("cap");
    let root = dir.path();
    // Ten digits, five hundred times: five thousand characters.
    let long = "0123456789".repeat(500);
    write_document(
        root,
        "notes/long.md",
        &format!("---\ntitle: Long\n---\n{long}"),
    );

    // No `[mcp]` section: the configured default is 4000 characters.
    let default = answer(&engine(root), json!({ "path": "notes/long.md" }));
    assert_eq!(default["body"], json!("0123456789".repeat(400)));
    assert_eq!(default["truncated"], json!(true));

    // A request overrides the default.
    let short = answer(
        &engine(root),
        json!({ "path": "notes/long.md", "max_chars": 3 }),
    );
    assert_eq!(short["body"], json!("012"));
    assert_eq!(short["truncated"], json!(true));

    // `0` is no limit, so nothing was cut.
    let whole = answer(
        &engine(root),
        json!({ "path": "notes/long.md", "max_chars": 0 }),
    );
    assert_eq!(whole["body"], json!(long));
    assert_eq!(whole["truncated"], json!(false));

    // The default comes from the config, not from a constant in the code.
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"cap\"\n\n[mcp]\nmax_chars = 7\n",
    )
    .expect("set the cap");
    let configured = answer(&engine(root), json!({ "path": "notes/long.md" }));
    assert_eq!(configured["body"], json!("0123456"));
    assert_eq!(configured["truncated"], json!(true));
}

// ───────────────────────────── one chunk ───────────────────────────────────

/// Two sections, so one chunk's span can move without its text changing.
const TWO_SECTIONS: &str =
    "---\ntitle: Span\ndate: 2024-01-01\n---\n\n## First\n\nalpha\n\n## Second\n\nbravo\n";

/// A corpus with [`TWO_SECTIONS`] indexed, and the second chunk's id.
fn two_sections() -> (tempfile::TempDir, String) {
    let dir = scratch("span");
    let root = dir.path();
    write_document(root, "notes/two.md", TWO_SECTIONS);
    common::index(root);
    let chunks = common::chunks_json(root, &["notes/two.md"]);
    let id = chunks["docs"][0]["chunks"][1]["id"]
        .as_str()
        .expect("the second chunk's id")
        .to_owned();
    (dir, id)
}

#[test]
fn case_3_7_a_chunk_id_survives_a_rebuild() {
    let (dir, id) = two_sections();
    let root = dir.path();
    // A chunk id excludes the document id, so a rebuild keeps it.
    common::run_ok(root, &["index", "--rebuild"]);

    let report = answer(
        &engine(root),
        json!({ "path": "notes/two.md", "chunk_id": id }),
    );
    assert_eq!(report["body"], json!("## Second\n\nbravo"));
    assert_eq!(report["span"], json!([18, 34]));
    assert_eq!(report["breadcrumb"], json!(["Second"]));
    assert_eq!(report["truncated"], json!(false));
    // The hash still describes the whole file, not the chunk.
    assert_eq!(
        report["content_hash"],
        json!(hash_of(&root.join("notes/two.md")))
    );
}

#[test]
fn case_3_7_a_body_edit_that_moves_the_span_is_not_found() {
    let (dir, id) = two_sections();
    let root = dir.path();
    // A new section above the others: the second chunk's text is unchanged, so
    // its id survives, and its old span now points at other bytes.
    write_document(
        root,
        "notes/two.md",
        "---\ntitle: Span\ndate: 2024-01-01\n---\n\n## Zero\n\nzero\n\n## First\n\nalpha\n\n## Second\n\nbravo\n",
    );
    // The lock makes the token real: the generation cannot catch up.
    let lock =
        Lock::acquire(&root.join(".janus"), Duration::from_secs(1)).expect("hold the writer lock");
    let response = engine(root).call(&read(json!({ "path": "notes/two.md", "chunk_id": id })));
    assert!(!response.ok, "{}", response.json);
    assert_eq!(response.code, Some("not_found"));
    let message = response.json["message"].as_str().expect("the message");
    assert!(message.contains("notes/two.md"), "{message}");
    assert!(message.contains("run janus index"), "{message}");
    drop(lock);
}

#[test]
fn case_3_8_text_has_one_path() {
    let dir = fixture_vault();
    let root = dir.path();
    // One read returns one document, and only when it was asked for.
    let report = answer(&engine(root), json!({ "path": POSTGRES }));
    let body = report["body"].as_str().expect("the body");
    assert_eq!(body, POSTGRES_BODY);

    // `search` answers with a span and a chunk id, never with text.
    let found = common::search_json(root, &["cache", "--no-embeddings"]);
    let rows = found["results"].as_array().expect("results");
    assert!(!rows.is_empty(), "the query must match something");
    for row in rows {
        assert!(row.get("text").is_none(), "a row carries no text: {row}");
        assert!(row.get("body").is_none(), "a row carries no body: {row}");
        assert!(row.get("span").is_some(), "a row names a span: {row}");
    }
}
