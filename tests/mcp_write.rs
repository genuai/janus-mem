//! Cases 4.1, 4.3, 4.4 and 4.6 of the M7 plan: the MCP surface of the write tool
//! — its gate, its capability flag, its handshake paragraph and the config key
//! that cannot enable it (§3.4).
//!
//! Every case drives the binary over stdio and reads the bytes on disk after the
//! call. No case sleeps, reads the wall clock or opens a port, and every corpus
//! is a temporary directory. A write needs no index (M7-17), so no case builds
//! one.
//!
//! The engine's own cases and the etag live in `tests/write_rpc.rs`, which runs
//! them over `janus rpc`; nothing is asserted twice here.

mod common;

use std::fs;
use std::path::Path;

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

    // The two whose phases have not landed are refused even with the flag.
    for name in ["supersede_document", "promote_document"] {
        let envelope = call(Some(root), None, true, name, json!({}));
        assert_eq!(code(&envelope), "refused", "{name}: {envelope}");
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
    let workspace_file = found.path().join("workspace.toml");
    let envelope = status(None, Some(&workspace_file), true);
    assert_eq!(envelope["writes_enabled"], json!(false), "{envelope}");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");

    // The list must agree with the flag. It describes the primary, so on a
    // read-only primary the served tool is not advertised: every call to an
    // advertised name would answer `read_only_corpus` (M7-49).
    let script = format!(
        "{}{}\n",
        mcp_opening(),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })
    );
    let (answers, _, code) = mcp_session(None, Some(&workspace_file), true, &script);
    assert_eq!(code, Some(0));
    let listed: Vec<&str> = answers[1]["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    assert!(!listed.contains(&"update_frontmatter"), "{listed:?}");
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
    const WARNING: &str = "warning: `[mcp] allow_write` is ignored; a server serves writes only when it is \
         started with --allow-write, so remove the key\n";

    // The key is read, so the config still parses, and it decides nothing.
    let envelope = status(Some(root), None, false);
    assert_eq!(envelope["writes_enabled"], json!(false), "{envelope}");

    // `status` is the command a person runs to ask "can this write?", so it says
    // the line too, once, without building anything.
    let report = common::run(root, &["status", "--json"]);
    assert_eq!(report.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(report.stderr).expect("stderr is UTF-8"),
        WARNING
    );
    let report: Value = serde_json::from_slice(&report.stdout).expect("the report is JSON");
    assert_eq!(report["writes_enabled"], json!(false), "{report}");

    // A server says it once per session, on the read that builds: the build
    // prints it, and nothing prints it a second time (case 4.6).
    let script = format!(
        "{}{}",
        mcp_opening(),
        mcp_call(2, "search", json!({ "query": "prose" }))
    );
    let (answers, stderr, code) = mcp_session(Some(root), None, false, &script);
    assert_eq!(code, Some(0));
    assert_eq!(answers.len(), 2, "{answers:?}");
    assert_eq!(stderr.matches(WARNING).count(), 1, "{stderr}");

    // Not listed while the server serves no write, so a client cannot find a
    // name that would answer `write_disabled`.
    let opened = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let (answers, _, _) = mcp_session(
        Some(root),
        None,
        false,
        &format!("{}{opened}\n", mcp_opening()),
    );
    let listed: Vec<&str> = answers[1]["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    assert!(!listed.contains(&"update_frontmatter"), "{listed:?}");

    // The flag alone decides, so the same config with the flag answers a write,
    // and the flag adds no line of its own.
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
