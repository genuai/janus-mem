//! Cases 2.1–2.3, 2.7a, 2.7b, 2.8, 4.2 and 4.5 of the M7 plan: the first write
//! tool, its etag, and the corpus boundary, over `janus rpc` (§3.2, §3.4).
//!
//! Every case drives the binary over one pipe and reads the bytes on disk after
//! the call, so a write that happened is a changed file and not a claim. No case
//! sleeps, reads the wall clock or opens a port, and every corpus is a temporary
//! directory. A write needs no index (M7-17), so no case builds one.
//!
//! The exit code of a failure inside a server is `0`: a server answers, it does
//! not exit (§9.1). The exit code each code carries is asserted in
//! `src/api/error.rs` and, on the CLI, in `tests/cli_set.rs`.
//!
//! The MCP surface's own cases — the gate, `writes_enabled`, the instructions
//! string and the ignored config key — live in `tests/mcp_write.rs`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{janus, write_document};
use janus_mem::api::{Writes, write_refusal};
use serde_json::{Value, json};

/// The one document every write names, and its bytes, so a diff is a literal.
const ONE: &str = "---\ntitle: One\nstatus: active\n---\n\n# One\n\nfirst prose\n";

/// The four write names of §9.3.
const WRITE_NAMES: [&str; 4] = [
    "update_frontmatter",
    "write_document",
    "supersede_document",
    "promote_document",
];

// ──────────────────────────────── helpers ──────────────────────────────────

/// A corpus with one document and no index.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"write\"\n\n[embedding]\nenabled = false\n",
    )
    .expect("write the config");
    write_document(dir.path(), "notes/one.md", ONE);
    dir
}

/// A corpus whose config excludes one directory from the index.
fn scratch_excluding() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"write\"\n\n[embedding]\nenabled = false\n\n\
         [index]\nexclude = [\"drafts/**\"]\n",
    )
    .expect("write the config");
    write_document(dir.path(), "drafts/plan.md", ONE);
    dir
}

/// A workspace naming two corpora, either of which may be read-only.
///
/// The archived corpus declares `corpus_id = "fixtures"` while the workspace
/// calls it `context`, so a refusal must name the id the caller typed. The role
/// decides reads, the `read_only` key decides writes.
fn workspace(primary_read_only: bool, archive_read_only: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (corpus, id) in [("primary", "main"), ("archive", "fixtures")] {
        let root = dir.path().join(corpus);
        fs::create_dir_all(&root).expect("create the corpus");
        fs::write(
            root.join("janus.toml"),
            format!(
                "index_dir = \".janus\"\ncorpus_id = \"{id}\"\n\n[embedding]\nenabled = false\n"
            ),
        )
        .expect("write the corpus config");
        write_document(&root, "notes/one.md", ONE);
    }
    fs::write(
        dir.path().join("workspace.toml"),
        format!(
            "[[corpora]]\nid = \"main\"\nroot = \"primary\"\nrole = \"primary\"\n\
             read_only = {primary_read_only}\n\n\
             [[corpora]]\nid = \"context\"\nroot = \"archive\"\nrole = \"archived\"\n\
             read_only = {archive_read_only}\n"
        ),
    )
    .expect("write the workspace");
    dir
}

/// One file's text, so a file that did not change is one assertion.
fn text(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).expect("read the document")
}

/// The blake3 of one file, which is the etag a read reports.
fn hash(root: &Path, rel: &str) -> String {
    let bytes = fs::read(root.join(rel)).expect("read the document");
    blake3::hash(&bytes).to_hex().to_string()
}

/// Runs one `janus rpc` session over one corpus or one workspace.
///
/// The answers come back parsed, one per non-empty request line.
fn session(
    vault: Option<&Path>,
    found: Option<&Path>,
    allow_write: bool,
    requests: &[Value],
) -> Vec<Value> {
    let mut command = janus();
    command.env("RUST_LOG", "off").arg("rpc");
    if allow_write {
        command.arg("--allow-write");
    }
    match (vault, found) {
        (Some(root), None) => {
            command.arg("--vault").arg(root);
        }
        (None, Some(workspace)) => {
            command.arg("--workspace").arg(workspace);
        }
        _ => panic!("name one corpus root or one workspace"),
    }
    let lines: String = requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect();
    let output = command.write_stdin(lines).output().expect("run janus rpc");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("stdout is UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("an answer line"))
        .collect()
}

/// One request line for `tool`, with `params`.
fn request(id: u64, tool: &str, params: Value) -> Value {
    json!({ "id": id, "tool": tool, "params": params })
}

/// The answer to one `update_frontmatter` call over a corpus.
fn update(root: &Path, allow_write: bool, params: Value) -> Value {
    session(
        Some(root),
        None,
        allow_write,
        &[request(1, "update_frontmatter", params)],
    )
    .remove(0)
}

/// The answer to one `update_frontmatter` call over a workspace.
fn write_in(found: &Path, path: &str, etag: &str) -> Value {
    session(
        None,
        Some(found),
        true,
        &[request(
            1,
            "update_frontmatter",
            json!({
                "path": path,
                "patch": { "status": "superseded" },
                "expected_hash": etag,
            }),
        )],
    )
    .remove(0)
}

/// The answer to one call over a corpus, by wire name.
fn call(root: &Path, allow_write: bool, tool: &str, params: Value) -> Value {
    session(Some(root), None, allow_write, &[request(1, tool, params)]).remove(0)
}

/// The failure code of one answer, asserting that it failed.
fn code(answer: &Value) -> &str {
    assert_eq!(answer["ok"], json!(false), "{answer}");
    answer["code"].as_str().expect("a code")
}

// ──────────────────────────────── the etag ─────────────────────────────────

#[test]
fn case_2_1_a_write_with_no_etag_is_refused_with_a_one_line_preview() {
    let dir = scratch();
    let root = dir.path();
    let answer = update(
        root,
        true,
        json!({ "path": "notes/one.md", "patch": { "status": "superseded" } }),
    );
    assert_eq!(code(&answer), "precondition_required", "{answer}");
    // One line, naming the key that would change and the two ways forward
    // (M7-13).
    let hint = answer["hint"].as_str().expect("a hint");
    assert!(hint.contains("status"), "{hint}");
    assert!(hint.contains("expected_hash"), "{hint}");
    assert!(hint.contains("dry_run"), "{hint}");
    assert!(!hint.contains('\n'), "the hint is one line: {hint}");
    // Nothing was written.
    assert_eq!(text(root, "notes/one.md"), ONE);
}

#[test]
fn case_2_1_a_dry_run_answers_the_full_preview_and_writes_nothing() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let answer = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "dry_run": true,
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(answer["dry_run"], json!(true), "{answer}");
    assert_eq!(answer["changed_keys"], json!(["status"]), "{answer}");
    assert_eq!(answer["old_hash"], json!(before), "{answer}");
    assert_ne!(answer["new_hash"], json!(before), "{answer}");
    assert_eq!(text(root, "notes/one.md"), ONE);
}

#[test]
fn case_2_2_a_stale_hash_is_a_conflict_that_names_both_hashes() {
    let dir = scratch();
    let root = dir.path();
    let current = hash(root, "notes/one.md");
    let presented = "0".repeat(64);
    let answer = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": presented,
        }),
    );
    assert_eq!(code(&answer), "conflict", "{answer}");
    let message = answer["message"].as_str().expect("a message");
    assert!(message.contains(&presented), "{message}");
    assert!(message.contains(&current), "{message}");
    // A conflict writes nothing.
    assert_eq!(text(root, "notes/one.md"), ONE);
}

#[test]
fn case_2_3_a_correct_hash_answers_a_receipt_the_next_read_agrees_with() {
    let dir = scratch();
    let root = dir.path();
    let document = call(
        root,
        true,
        "read_document",
        json!({ "path": "notes/one.md" }),
    );
    let etag = document["content_hash"]
        .as_str()
        .expect("a content hash")
        .to_owned();
    let bytes_before = fs::metadata(root.join("notes/one.md"))
        .expect("the document")
        .len() as u64;

    let answer = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": etag,
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(answer["old_hash"], json!(etag), "{answer}");
    assert_eq!(answer["path"], json!("notes/one.md"), "{answer}");
    assert_eq!(answer["dry_run"], json!(false), "{answer}");
    assert_eq!(answer["appended_keys"], json!([]), "{answer}");
    // The new hash is the hash of the bytes on disk, and the next read reports
    // it as `content_hash`, so a receipt is a usable etag (M7-18).
    assert_eq!(answer["new_hash"], json!(hash(root, "notes/one.md")));
    let after = call(
        root,
        true,
        "read_document",
        json!({ "path": "notes/one.md" }),
    );
    assert_eq!(after["content_hash"], answer["new_hash"], "{after}");
    let bytes_after = fs::metadata(root.join("notes/one.md"))
        .expect("the document")
        .len() as u64;
    assert_eq!(answer["bytes_before"], json!(bytes_before));
    assert_eq!(answer["bytes_after"], json!(bytes_after));
    assert_eq!(bytes_before, ONE.len() as u64);
}

// ──────────────────────────────── the receipt ──────────────────────────────

#[test]
fn case_4_5_a_receipt_is_not_an_envelope() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let answer = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded", "superseded_by": "notes/two.md" },
            "expected_hash": before,
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    // The exact key set of §2.3, minus `id`, which the rpc line echoes. A field
    // that leaks in fails here, and the two envelope keys a receipt must never
    // carry are named on their own.
    let mut keys: Vec<&str> = answer
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "id")
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "api",
            "appended_keys",
            "bytes_after",
            "bytes_before",
            "changed_keys",
            "dry_run",
            "new_hash",
            "ok",
            "old_hash",
            "path",
        ]
    );
    assert!(answer.get("results").is_none(), "{answer}");
    assert!(answer.get("notes").is_none(), "{answer}");
}

// ───────────────────────────────── the gate ────────────────────────────────

#[test]
fn case_4_2_rpc_answers_the_gate_from_the_shared_helper() {
    let dir = scratch();
    let root = dir.path();
    let writes = |allowed| Writes::new("janus rpc", allowed, false);
    let requests: Vec<Value> = WRITE_NAMES
        .iter()
        .enumerate()
        .map(|(id, name)| {
            request(
                id as u64,
                name,
                json!({ "path": "notes/one.md", "patch": {} }),
            )
        })
        .collect();

    // Without the flag every write name answers `write_disabled`, and the object
    // is the helper's own, so rpc did not re-implement the gate.
    let answers = session(Some(root), None, false, &requests);
    for (name, answer) in WRITE_NAMES.iter().zip(&answers) {
        assert_eq!(code(answer), "write_disabled", "{name}: {answer}");
        assert_eq!(answer["api"], json!(1), "{answer}");
        let shared = write_refusal(name, writes(false)).expect("a write name is gated");
        assert_eq!(
            answer["code"],
            json!(shared.code),
            "the shared helper answers {name}"
        );
        let message = answer["message"].as_str().expect("a message");
        assert!(message.contains("janus rpc"), "{message}");
        assert!(message.contains("--allow-write"), "{message}");
    }

    // With the flag the three unlanded names are still refused, and the served
    // one reaches the engine: an empty patch is a usage failure, not a refusal.
    let answers = session(Some(root), None, true, &requests);
    for (name, answer) in WRITE_NAMES.iter().zip(&answers) {
        if *name == "update_frontmatter" {
            assert_eq!(code(answer), "usage", "{name}: {answer}");
        } else {
            assert_eq!(code(answer), "refused", "{name}: {answer}");
            assert!(answer["hint"].is_string(), "{answer}");
            assert!(write_refusal(name, writes(true)).is_some(), "{name}");
        }
    }
    assert!(write_refusal("update_frontmatter", writes(true)).is_none());
    assert!(write_refusal("search", writes(true)).is_none());

    // Not one byte moved through any of it.
    assert_eq!(text(root, "notes/one.md"), ONE);
}

// ──────────────────────────── the corpus boundary ──────────────────────────

#[test]
fn case_2_8_a_path_outside_the_corpus_is_refused() {
    let dir = scratch();
    let root = dir.path();
    // A file that exists next to the corpus, with frontmatter of its own.
    let outside = tempfile::tempdir().expect("tempdir");
    write_document(outside.path(), "outside.md", ONE);
    // And two paths inside the corpus that no write may touch.
    fs::create_dir_all(root.join(".git")).expect("create the git directory");
    write_document(root, ".git/tracked.md", ONE);
    write_document(root, ".janus/kept.md", ONE);
    let outside_path = outside.path().join("outside.md");
    let before = blake3::hash(&fs::read(&outside_path).expect("the outside file"))
        .to_hex()
        .to_string();

    for path in [
        outside_path.to_string_lossy().into_owned(),
        ".git/tracked.md".to_owned(),
        ".janus/kept.md".to_owned(),
        "../outside.md".to_owned(),
    ] {
        let answer = update(
            root,
            true,
            json!({
                "path": path,
                "patch": { "status": "superseded" },
                "expected_hash": "0".repeat(64),
            }),
        );
        assert_eq!(code(&answer), "path_escape", "{path}: {answer}");
    }
    // Not one byte moved, inside or outside.
    assert_eq!(
        blake3::hash(&fs::read(&outside_path).expect("the outside file"))
            .to_hex()
            .to_string(),
        before
    );
    for rel in [".git/tracked.md", ".janus/kept.md"] {
        assert_eq!(text(root, rel), ONE, "{rel}");
    }
}

#[test]
fn case_2_7a_a_read_only_corpus_refuses_the_write() {
    let dir = workspace(true, false);
    let found: PathBuf = dir.path().join("workspace.toml");
    let root = dir.path().join("primary");
    let before = hash(&root, "notes/one.md");
    let answer = write_in(&found, "notes/one.md", &before);
    assert_eq!(code(&answer), "read_only_corpus", "{answer}");
    // The bytes are the same, hashed before and after.
    assert_eq!(hash(&root, "notes/one.md"), before);
}

#[test]
fn case_2_7a_the_refusal_names_the_corpus_the_caller_typed() {
    // The workspace calls this corpus `context`; its own config says `fixtures`.
    // A caller who typed `context` is told that, and then the declared id, so no
    // one hunts for a corpus their workspace does not name.
    let dir = workspace(false, true);
    let found: PathBuf = dir.path().join("workspace.toml");
    let archived = dir.path().join("archive");
    let before = hash(&archived, "notes/one.md");
    let answer = write_in(&found, "context:notes/one.md", &before);
    assert_eq!(code(&answer), "read_only_corpus", "{answer}");
    let message = answer["message"].as_str().expect("a message");
    assert!(message.contains("context"), "{message}");
    assert!(message.contains("fixtures"), "{message}");
    // The bytes are the same, hashed before and after.
    assert_eq!(hash(&archived, "notes/one.md"), before);
}

#[test]
fn case_2_7a_a_writable_corpus_of_the_same_workspace_accepts_the_write() {
    // The same workspace with one key flipped: the refusal is about the corpus
    // and not about workspace mode (M7-16).
    let dir = workspace(false, false);
    let found: PathBuf = dir.path().join("workspace.toml");
    let root = dir.path().join("primary");
    let before = hash(&root, "notes/one.md");
    let answer = write_in(&found, "notes/one.md", &before);
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(answer["path"], json!("notes/one.md"), "{answer}");

    // A qualified path names the second corpus, and its write lands there.
    let archived = dir.path().join("archive");
    let before = hash(&archived, "notes/one.md");
    let answer = write_in(&found, "context:notes/one.md", &before);
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(
        text(&root, "notes/one.md"),
        ONE.replace("status: active", "status: superseded")
    );
}

#[test]
fn case_2_7b_single_corpus_mode_has_no_read_only_refusal() {
    // One request on one document, twice: written with no workspace, where no
    // `read_only` field exists to refuse it, and refused under a workspace that
    // declares one. Deleting the rule leaves the first half green and the second
    // red, which is what makes the pair a test (case 2.7b).
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let answer = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": before,
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_ne!(hash(root, "notes/one.md"), before);

    let refused = workspace(true, false);
    let found: PathBuf = refused.path().join("workspace.toml");
    let declared = refused.path().join("primary");
    let etag = hash(&declared, "notes/one.md");
    let answer = write_in(&found, "notes/one.md", &etag);
    assert_eq!(code(&answer), "read_only_corpus", "{answer}");
    assert_eq!(hash(&declared, "notes/one.md"), etag);
}

// ──────────────────────────────── the target ───────────────────────────────

#[test]
fn m7_52_a_write_needs_a_markdown_document() {
    // The etag is not a secret: a caller can compute it for any file. The
    // extension is what keeps a write on a document (M7-52).
    let dir = scratch();
    let root = dir.path();
    write_document(root, "notes/hidden.txt", ONE);
    let answer = update(
        root,
        true,
        json!({
            "path": "notes/hidden.txt",
            "patch": { "status": "superseded" },
            "expected_hash": hash(root, "notes/hidden.txt"),
        }),
    );
    assert_eq!(code(&answer), "usage", "{answer}");
    let message = answer["message"].as_str().expect("a message");
    assert!(message.contains("Markdown"), "{message}");
    assert!(message.contains(".md"), "{message}");
    // The copy is byte-identical.
    assert_eq!(text(root, "notes/hidden.txt"), ONE);
}

#[test]
fn m7_52_an_excluded_markdown_file_stays_writable() {
    // A draft kept out of search is still a document the user may edit. The
    // exclude rule shapes the index, not the corpus's admission of a document.
    let dir = scratch_excluding();
    let root = dir.path();
    let before = hash(root, "drafts/plan.md");
    let answer = update(
        root,
        true,
        json!({
            "path": "drafts/plan.md",
            "patch": { "status": "superseded" },
            "expected_hash": before,
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(answer["changed_keys"], json!(["status"]), "{answer}");
    assert_ne!(hash(root, "drafts/plan.md"), before);
}

// ───────────────────────────── the refused shapes ──────────────────────────

#[test]
fn every_refusal_of_a_shape_names_its_code() {
    let dir = scratch();
    let root = dir.path();
    write_document(
        root,
        "notes/blocks.md",
        "---\ntitle: Blocks\ntags:\n  - one\n---\n\n# Blocks\n",
    );
    write_document(root, "notes/plain.md", "no frontmatter here\n");
    let etag = hash(root, "notes/one.md");
    let cases: [(&str, Value, &str); 4] = [
        (
            "a block sequence is unsupported",
            json!({
                "path": "notes/blocks.md",
                "patch": { "tags": "one" },
                "expected_hash": hash(root, "notes/blocks.md"),
            }),
            "unsupported_patch",
        ),
        (
            "a key outside the pattern is refused",
            json!({
                "path": "notes/one.md",
                "patch": { "a: b": "x" },
                "expected_hash": etag,
            }),
            "key_shape",
        ),
        (
            "a document without frontmatter is refused",
            json!({
                "path": "notes/plain.md",
                "patch": { "status": "superseded" },
                "expected_hash": hash(root, "notes/plain.md"),
            }),
            "no_frontmatter",
        ),
        (
            "an empty patch is a usage failure",
            json!({
                "path": "notes/one.md",
                "patch": {},
                "expected_hash": etag,
            }),
            "usage",
        ),
    ];
    for (label, params, expected) in cases {
        let answer = update(root, true, params);
        assert_eq!(code(&answer), expected, "{label}: {answer}");
    }
    // A refused shape leaves every file as it was.
    assert_eq!(text(root, "notes/one.md"), ONE);
}
