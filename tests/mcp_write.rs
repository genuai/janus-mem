//! Cases 2.1–2.3, 2.7a, 2.7b, 2.8 and 4.1–4.6 of the M7 plan: the first write
//! tool, reachable only through a gate that is off by default (§3.2, §3.4).
//!
//! Every case drives the binary over stdio and reads the bytes on disk after the
//! call, so a write that happened is a changed file and not a claim. No case
//! sleeps, reads the wall clock or opens a port, and every corpus is a temporary
//! directory. A write needs no index (M7-17), so most cases build none.
//!
//! The exit code of a failure inside a server is `0`: a server answers, it does
//! not exit (§9.1). The exit code each new code carries is asserted where it
//! lives, in `src/api/error.rs`, and on the CLI in `tests/cli_set.rs`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{janus, mcp_call, mcp_envelope, mcp_opening, write_document};
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

/// A workspace whose primary corpus is either writable or read-only.
///
/// The archived corpus declares `read_only = false`, so a qualifier can name it
/// as a writable second corpus; the role decides reads, the key decides writes.
/// The roots are relative to the workspace file, as §6.11 states.
fn workspace(primary_read_only: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (corpus, id) in [("primary", "main"), ("archive", "context")] {
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
             read_only = false\n"
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

/// One finished `janus mcp` session: the answers, stderr, and the exit code.
fn mcp_session(
    vault: Option<&Path>,
    found: Option<&Path>,
    allow_write: bool,
    script: &str,
) -> (Vec<Value>, String, Option<i32>) {
    let mut command = janus();
    command.env("RUST_LOG", "off").arg("mcp");
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
    let output = command.write_stdin(script).output().expect("run janus mcp");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let answers = stdout
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("stdout line is not JSON ({err}):\n{line}"))
        })
        .collect();
    (
        answers,
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        output.status.code(),
    )
}

/// The tool envelope of one `tools/call` over MCP.
fn call(
    vault: Option<&Path>,
    found: Option<&Path>,
    allow_write: bool,
    name: &str,
    arguments: Value,
) -> Value {
    let script = format!("{}{}", mcp_opening(), mcp_call(2, name, arguments));
    let (answers, stderr, code) = mcp_session(vault, found, allow_write, &script);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert_eq!(answers.len(), 2, "{answers:?}");
    mcp_envelope(&answers[1]).clone()
}

/// The envelope of one `update_frontmatter` call over a corpus.
fn update(root: &Path, allow_write: bool, arguments: Value) -> Value {
    call(
        Some(root),
        None,
        allow_write,
        "update_frontmatter",
        arguments,
    )
}

/// `status` as one server answers it.
fn status(vault: Option<&Path>, found: Option<&Path>, allow_write: bool) -> Value {
    call(vault, found, allow_write, "janus_status", json!({}))
}

/// One answer with the request id and the surface name removed.
///
/// A gate refusal names the surface, so the two server surfaces differ in one
/// word and in nothing else.
fn normalized(mut answer: Value, surface: &str) -> Value {
    let Some(fields) = answer.as_object_mut() else {
        return answer;
    };
    fields.remove("id");
    if let Some(message) = fields.get("message").and_then(Value::as_str) {
        let message = message.replace(surface, "janus <surface>");
        fields.insert("message".to_owned(), json!(message));
    }
    answer
}

/// The failure code of one envelope, asserting that it failed.
fn code(envelope: &Value) -> &str {
    assert_eq!(envelope["ok"], json!(false), "{envelope}");
    envelope["code"].as_str().expect("a code")
}

// ──────────────────────────────── the gate ─────────────────────────────────

#[test]
fn case_4_1_mcp_refuses_every_write_name_without_the_flag() {
    let dir = scratch();
    let root = dir.path();
    for name in WRITE_NAMES {
        let envelope = call(
            Some(root),
            None,
            false,
            name,
            json!({ "path": "notes/one.md", "patch": { "status": "superseded" } }),
        );
        assert_eq!(code(&envelope), "write_disabled", "{name}: {envelope}");
        assert_eq!(envelope["api"], json!(1), "{envelope}");
        // The message names the flag, which is the only switch (M7-36).
        let message = envelope["message"].as_str().expect("a message");
        assert!(message.contains("--allow-write"), "{name}: {message}");
    }
    // Not one byte moved.
    assert_eq!(text(root, "notes/one.md"), ONE);
}

#[test]
fn case_4_1_with_the_flag_the_served_tool_reaches_its_contract() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let envelope = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": before,
        }),
    );
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["changed_keys"], json!(["status"]), "{envelope}");
    let written = text(root, "notes/one.md");
    assert_eq!(written, ONE.replace("status: active", "status: superseded"));

    // The three whose phases have not landed are refused even with the flag.
    for name in ["write_document", "supersede_document", "promote_document"] {
        let envelope = call(Some(root), None, true, name, json!({}));
        assert_eq!(code(&envelope), "refused", "{name}: {envelope}");
    }
}

#[test]
fn case_4_2_rpc_answers_the_gate_exactly_as_mcp_does() {
    let dir = scratch();
    let root = dir.path();
    for allow_write in [false, true] {
        let lines: String = WRITE_NAMES
            .iter()
            .enumerate()
            .map(|(id, name)| {
                format!(
                    "{}\n",
                    json!({
                        "id": id,
                        "tool": name,
                        "params": { "path": "notes/one.md", "patch": {} },
                    })
                )
            })
            .collect();
        let output = janus()
            .env("RUST_LOG", "off")
            .arg("--vault")
            .arg(root)
            .arg("rpc")
            .args(if allow_write {
                vec!["--allow-write"]
            } else {
                Vec::new()
            })
            .write_stdin(lines)
            .output()
            .expect("run janus rpc");
        assert_eq!(
            output.status.code(),
            Some(0),
            "a server answers, it exits 0"
        );
        let rpc: Vec<Value> = String::from_utf8(output.stdout)
            .expect("stdout is UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("an answer line"))
            .collect();
        for (name, answer) in WRITE_NAMES.iter().zip(&rpc) {
            let from_mcp = call(
                Some(root),
                None,
                allow_write,
                name,
                json!({ "path": "notes/one.md", "patch": {} }),
            );
            // The one field that may differ: a gate refusal names the surface
            // that answered, so a reader learns which server said no. Every
            // other byte is the same answer (case 4.2).
            if !allow_write {
                let surfaces = [(answer, "janus rpc"), (&from_mcp, "janus mcp")];
                for (answer, surface) in surfaces {
                    let message = answer["message"].as_str().expect("a message");
                    assert!(message.starts_with(surface), "{message}");
                }
            }
            assert_eq!(
                normalized(answer.clone(), "janus rpc"),
                normalized(from_mcp, "janus mcp"),
                "{name}, allow_write = {allow_write}"
            );
        }
    }
}

#[test]
fn case_4_3_status_reports_writes_enabled_truthfully() {
    let dir = scratch();
    let root = dir.path();
    assert_eq!(
        status(Some(root), None, false)["writes_enabled"],
        json!(false)
    );
    assert_eq!(
        status(Some(root), None, true)["writes_enabled"],
        json!(true)
    );

    // On a read-only corpus the flag cannot make the flag true: a capability
    // that promised a write that must fail would be a lie (M7-16).
    let found = workspace(true);
    let envelope = status(None, Some(&found.path().join("workspace.toml")), true);
    assert_eq!(envelope["writes_enabled"], json!(false), "{envelope}");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
}

#[test]
fn case_4_6_a_config_key_cannot_enable_writes_and_warns_once() {
    let dir = scratch();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"write\"\n\n[embedding]\nenabled = false\n\n\
         [mcp]\nallow_write = true\n",
    )
    .expect("enable the ignored key");

    // The key is read, so the config still parses, and it decides nothing.
    let envelope = status(Some(root), None, false);
    assert_eq!(envelope["writes_enabled"], json!(false), "{envelope}");
    let opened = format!(
        "{}",
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })
    );
    let (answers, stderr, code) = mcp_session(
        Some(root),
        None,
        false,
        &format!("{}{opened}\n", mcp_opening()),
    );
    assert_eq!(code, Some(0));
    let listed: Vec<&str> = answers[1]["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    assert!(!listed.contains(&"update_frontmatter"), "{listed:?}");
    // One warning, which names the flag, and nothing else on stderr.
    assert_eq!(
        stderr,
        "warning: `[mcp] allow_write` is ignored; a server serves writes only when it is \
         started with --allow-write, so remove the key\n"
    );

    // The flag itself prints nothing: the M6 statement is gone, and stdout
    // stays a protocol stream (§3.10). The config warning is still the only
    // stderr line.
    let (answers, flagged, code) = mcp_session(Some(root), None, true, "");
    assert_eq!(code, Some(0));
    assert!(answers.is_empty(), "{answers:?}");
    assert_eq!(flagged, stderr, "the flag adds no line of its own");

    // The flag alone decides, so the same config with the flag answers a write.
    let before = hash(root, "notes/one.md");
    let envelope = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": before,
        }),
    );
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
}

// ──────────────────────────────── the receipt ──────────────────────────────

#[test]
fn case_4_5_a_receipt_is_not_an_envelope() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let envelope = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded", "superseded_by": "notes/two.md" },
            "expected_hash": before,
        }),
    );
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    // The exact key set of §2.3. A field that leaks in fails here, and the two
    // envelope keys a receipt must never carry are named on their own.
    let mut keys: Vec<&str> = envelope
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
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
    assert!(envelope.get("results").is_none(), "{envelope}");
    assert!(envelope.get("notes").is_none(), "{envelope}");
}

#[test]
fn case_2_3_a_correct_hash_answers_a_receipt_the_next_read_agrees_with() {
    let dir = scratch();
    let root = dir.path();
    let document = call(
        Some(root),
        None,
        true,
        "read_document",
        json!({ "path": "notes/one.md" }),
    );
    let etag = document["content_hash"].as_str().expect("a content hash");
    let bytes_before = fs::metadata(root.join("notes/one.md"))
        .expect("the document")
        .len() as u64;

    let envelope = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": etag,
        }),
    );
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["old_hash"], json!(etag), "{envelope}");
    assert_eq!(envelope["path"], json!("notes/one.md"), "{envelope}");
    assert_eq!(envelope["dry_run"], json!(false), "{envelope}");
    assert_eq!(envelope["appended_keys"], json!([]), "{envelope}");
    // The new hash is the hash of the bytes on disk, and the next read reports
    // it as `content_hash`, so a receipt is a usable etag (M7-18).
    assert_eq!(envelope["new_hash"], json!(hash(root, "notes/one.md")));
    let after = call(
        Some(root),
        None,
        true,
        "read_document",
        json!({ "path": "notes/one.md" }),
    );
    assert_eq!(after["content_hash"], envelope["new_hash"], "{after}");
    let bytes_after = fs::metadata(root.join("notes/one.md"))
        .expect("the document")
        .len() as u64;
    assert_eq!(envelope["bytes_before"], json!(bytes_before));
    assert_eq!(envelope["bytes_after"], json!(bytes_after));
    assert_eq!(bytes_before, ONE.len() as u64);
}

// ──────────────────────────────── the etag ─────────────────────────────────

#[test]
fn case_2_1_a_write_with_no_etag_is_refused_with_a_one_line_preview() {
    let dir = scratch();
    let root = dir.path();
    let envelope = update(
        root,
        true,
        json!({ "path": "notes/one.md", "patch": { "status": "superseded" } }),
    );
    assert_eq!(code(&envelope), "precondition_required", "{envelope}");
    // One line, naming the key that would change and the two ways forward
    // (M7-13).
    let hint = envelope["hint"].as_str().expect("a hint");
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
    let envelope = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "dry_run": true,
        }),
    );
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["dry_run"], json!(true), "{envelope}");
    assert_eq!(envelope["changed_keys"], json!(["status"]), "{envelope}");
    assert_eq!(envelope["old_hash"], json!(before), "{envelope}");
    assert_ne!(envelope["new_hash"], json!(before), "{envelope}");
    assert_eq!(text(root, "notes/one.md"), ONE);
}

#[test]
fn case_2_2_a_stale_hash_is_a_conflict_that_names_both_hashes() {
    let dir = scratch();
    let root = dir.path();
    let current = hash(root, "notes/one.md");
    let presented = "0".repeat(64);
    let envelope = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": presented,
        }),
    );
    assert_eq!(code(&envelope), "conflict", "{envelope}");
    let message = envelope["message"].as_str().expect("a message");
    assert!(message.contains(&presented), "{message}");
    assert!(message.contains(&current), "{message}");
    // A conflict writes nothing.
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
        let envelope = update(
            root,
            true,
            json!({
                "path": path,
                "patch": { "status": "superseded" },
                "expected_hash": "0".repeat(64),
            }),
        );
        assert_eq!(code(&envelope), "path_escape", "{path}: {envelope}");
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
    let dir = workspace(true);
    let found: PathBuf = dir.path().join("workspace.toml");
    let root = dir.path().join("primary");
    let before = hash(&root, "notes/one.md");
    let envelope = call(
        None,
        Some(&found),
        true,
        "update_frontmatter",
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": before,
        }),
    );
    assert_eq!(code(&envelope), "read_only_corpus", "{envelope}");
    assert_eq!(text(&root, "notes/one.md"), ONE);
}

#[test]
fn case_2_7a_a_writable_corpus_of_the_same_workspace_accepts_the_write() {
    // The same workspace with one key flipped: the refusal is about the corpus
    // and not about workspace mode (M7-16).
    let dir = workspace(false);
    let found: PathBuf = dir.path().join("workspace.toml");
    let root = dir.path().join("primary");
    let before = hash(&root, "notes/one.md");
    let envelope = call(
        None,
        Some(&found),
        true,
        "update_frontmatter",
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": before,
        }),
    );
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["path"], json!("notes/one.md"), "{envelope}");

    // A qualified path names the second corpus, and its write lands there.
    let archived = dir.path().join("archive");
    let before = hash(&archived, "notes/one.md");
    let envelope = call(
        None,
        Some(&found),
        true,
        "update_frontmatter",
        json!({
            "path": "context:notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": before,
        }),
    );
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(
        text(&root, "notes/one.md"),
        ONE.replace("status: active", "status: superseded")
    );
}

#[test]
fn case_2_7b_single_corpus_mode_has_no_read_only_refusal() {
    // No workspace means no role and no `read_only`, so the refusal of 2.7a
    // cannot fire and a caller is never told "read-only" by a mode that has no
    // such field (case 2.7b).
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let envelope = update(
        root,
        true,
        json!({
            "path": "notes/one.md",
            "patch": { "status": "superseded" },
            "expected_hash": before,
        }),
    );
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_ne!(code_or_ok(&envelope), "read_only_corpus", "{envelope}");
}

/// The code of a failure, or `ok` for a success, so one assertion can name both.
fn code_or_ok(envelope: &Value) -> &str {
    envelope["code"].as_str().unwrap_or("ok")
}

// ───────────────────────────── the refused shapes ──────────────────────────

#[test]
fn case_4_1_every_refusal_of_a_shape_names_its_code() {
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
    for (label, arguments, expected) in cases {
        let envelope = update(root, true, arguments);
        assert_eq!(code(&envelope), expected, "{label}: {envelope}");
    }
    // A refused shape leaves every file as it was.
    assert_eq!(text(root, "notes/one.md"), ONE);
}
