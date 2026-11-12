//! M6b phase 3 cases 3.1–3.5: the other tools and the three surfaces (§3.3 of
//! the M6b plan).
//!
//! Every case runs the binary against a copy of `tests/fixtures/workspace-read`,
//! which holds one document at the same relative path in two corpora with
//! different bytes. A read therefore proves which corpus answered by its body
//! alone, and both bodies are stated literally below.
//!
//! The workspace is named with `--workspace` and `XDG_CONFIG_HOME` points inside
//! the same temporary tree, so no case reads the developer's own workspace and
//! no case writes to the committed fixture. No case sleeps or reads the clock.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::Command;
use serde_json::{Value, json};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/workspace-read");

/// The one path both corpora hold.
const SHARED: &str = "notes/shared.md";

/// The body the primary corpus holds at [`SHARED`].
const PRIMARY_BODY: &str =
    "\n# Cache plan\n\nThe primary corpus keeps one cache in front of the database.\n";

/// The body the archived corpus holds at [`SHARED`].
const ARCHIVED_BODY: &str =
    "\n# Cache plan\n\nThe archived corpus describes the retired cache fleet.\n";

// ──────────────────────────────── helpers ──────────────────────────────────

/// A private copy of the committed fixture workspace.
fn copied() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir for the fixture copy");
    copy_tree(Path::new(FIXTURE), dir.path());
    dir
}

/// Copies one tree, skipping any index a previous run left behind.
fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            if entry.file_name() == ".janus" {
                continue;
            }
            fs::create_dir_all(&target).expect("create the directory");
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy the file");
        }
    }
}

/// The workspace file inside one fixture copy.
fn workspace(dir: &Path) -> String {
    dir.join("workspace.toml").display().to_string()
}

/// A `janus` command for one fixture copy, with no workspace in the ambient
/// configuration: every case names its workspace with the flag or not at all.
fn janus_in(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("janus").expect("the janus binary must build for tests");
    command
        .current_dir(dir)
        .env("RUST_LOG", "off")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env_remove("JANUS_WORKSPACE")
        .env_remove("JANUS_VAULT")
        .env_remove("APPDATA")
        .env_remove("HOME");
    command
}

/// Runs `janus` and returns the finished process.
fn run(dir: &Path, args: &[&str]) -> Output {
    janus_in(dir).args(args).output().expect("run janus")
}

/// Runs one command that must succeed and returns its stdout.
fn run_ok(dir: &Path, args: &[&str]) -> String {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "janus {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// Runs one command that must succeed and parses its report.
fn json(dir: &Path, args: &[&str]) -> Value {
    let text = run_ok(dir, args);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("stdout is not JSON ({err}):\n{text}"))
}

/// One request line to `janus rpc`, and the envelope it answered.
fn rpc(dir: &Path, tool: &str, params: Value) -> Value {
    let mut command = janus_in(dir);
    command.args(["rpc", "--workspace", &workspace(dir)]);
    let output = command
        .write_stdin(format!(
            "{}\n",
            json!({ "id": 1, "tool": tool, "params": params })
        ))
        .output()
        .expect("run janus rpc");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let line = stdout.lines().next().expect("one answer line");
    let mut answer: Value = serde_json::from_str(line).expect("the answer is JSON");
    // The loop echoes the request's `id` beside the envelope, which is the one
    // difference between its answer and the CLI's.
    answer.as_object_mut().expect("an object").remove("id");
    answer
}

/// One `tools/call` to `janus mcp`, and the envelope inside its answer.
fn mcp(dir: &Path, allow_write: bool, tool: &str, arguments: Value) -> Value {
    let mut command = janus_in(dir);
    command.args(["mcp", "--workspace", &workspace(dir)]);
    if allow_write {
        command.arg("--allow-write");
    }
    let script = format!(
        "{}{}",
        common::mcp_opening(),
        common::mcp_call(2, tool, arguments)
    );
    let output = command.write_stdin(script).output().expect("run janus mcp");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let answer: Value = stdout
        .lines()
        .last()
        .map(|line| serde_json::from_str(line).expect("the answer is JSON"))
        .expect("an answer line");
    answer["result"]["structuredContent"].clone()
}

/// The `(corpus, rel_path)` of every result, in order.
fn rows(report: &Value) -> Vec<(String, String)> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|row| {
            (
                row["corpus"].as_str().expect("corpus").to_owned(),
                row["rel_path"].as_str().expect("rel_path").to_owned(),
            )
        })
        .collect()
}

// ─────────────────────────────── the read ──────────────────────────────────

#[test]
fn case_3_1_a_read_without_a_corpus_reads_the_primary_and_names_it() {
    let dir = copied();
    let report = json(
        dir.path(),
        &[
            "read",
            "--workspace",
            &workspace(dir.path()),
            "--json",
            SHARED,
        ],
    );
    assert_eq!(report["body"], json!(PRIMARY_BODY));
    assert_eq!(report["rel_path"], json!(SHARED));
    assert_eq!(
        report["corpus"],
        json!("main"),
        "the answer names its corpus"
    );
}

#[test]
fn case_3_1_the_named_corpus_answers_and_a_qualified_path_answers_the_same() {
    let dir = copied();
    let workspace = workspace(dir.path());
    let named = json(
        dir.path(),
        &[
            "read",
            "--workspace",
            &workspace,
            "--corpus",
            "context",
            "--json",
            SHARED,
        ],
    );
    assert_eq!(
        named["body"],
        json!(ARCHIVED_BODY),
        "the same path in the archived corpus is a different document"
    );
    assert_eq!(named["corpus"], json!("context"));
    assert_eq!(named["rel_path"], json!(SHARED));

    // The qualifier the link syntax already uses names the corpus the same way.
    let qualified = json(
        dir.path(),
        &[
            "read",
            "--workspace",
            &workspace,
            "--json",
            &format!("context:{SHARED}"),
        ],
    );
    assert_eq!(qualified["body"], json!(ARCHIVED_BODY));
    assert_eq!(qualified["corpus"], json!("context"));
    assert_eq!(qualified["rel_path"], json!(SHARED));

    // The primary has a name of its own, and it is not the archived one.
    let primary = json(
        dir.path(),
        &[
            "read",
            "--workspace",
            &workspace,
            "--json",
            &format!("main:{SHARED}"),
        ],
    );
    assert_eq!(primary["body"], json!(PRIMARY_BODY));
    assert_eq!(primary["corpus"], json!("main"));
}

#[test]
fn case_3_2_an_unknown_corpus_is_a_usage_failure_that_lists_the_ids() {
    let dir = copied();
    let workspace = workspace(dir.path());
    // Two spellings of one question: the parameter and the qualifier.
    let qualified = format!("nope:{SHARED}");
    let attempts: [Vec<&str>; 2] = [
        vec![
            "read",
            "--workspace",
            &workspace,
            "--corpus",
            "nope",
            SHARED,
        ],
        vec!["read", "--workspace", &workspace, &qualified],
    ];
    for args in attempts {
        let output = run(dir.path(), &args);
        assert_eq!(output.status.code(), Some(2), "janus {args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("\"nope\""), "{stderr}");
        assert!(
            stderr.contains("main, context"),
            "the message lists the ids: {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "a refused read prints no document: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn case_3_2_a_single_corpus_read_names_its_own_corpus_and_no_other() {
    // No workspace, so one corpus is served and its declared id is the only
    // name that means anything. Any other name would read a different document.
    let primary = copied();
    let primary = primary.path().join("primary");
    let named = json(&primary, &["read", "--json", &format!("main:{SHARED}")]);
    assert_eq!(named["body"], json!(PRIMARY_BODY));
    assert_eq!(
        named["corpus"],
        Value::Null,
        "single-corpus mode names no corpus"
    );

    let output = run(&primary, &["read", "--corpus", "context", SHARED]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("\"context\""), "{stderr}");
    assert!(
        stderr.contains("\"main\""),
        "it names what it serves: {stderr}"
    );
}

// ────────────────────────────── the status ─────────────────────────────────

#[test]
fn case_3_3_a_workspace_status_lists_each_corpus_with_its_role_and_freshness() {
    let dir = copied();
    let primary = dir.path().join("primary").display().to_string();
    // One corpus is built and one is not, so the report has both states to show.
    run_ok(
        dir.path(),
        &["--vault", &primary, "index", "--no-embeddings"],
    );

    let report = json(
        dir.path(),
        &["status", "--workspace", &workspace(dir.path()), "--json"],
    );
    assert_eq!(report["workspace"], json!(workspace(dir.path())));
    // The head is the primary corpus's own status, unchanged.
    assert_eq!(report["corpus_id"], json!("main"));
    assert_eq!(report["writes_enabled"], json!(false));

    let rows = report["corpora"].as_array().expect("a corpora array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], json!("main"));
    assert_eq!(rows[0]["role"], json!("primary"));
    assert_eq!(rows[0]["read_only"], json!(false));
    assert_eq!(rows[0]["index_present"], json!(true));
    assert_eq!(rows[0]["index_stale"], json!(false));
    assert_eq!(rows[1]["id"], json!("context"));
    assert_eq!(rows[1]["role"], json!("archived"));
    // The workspace says the archive is writable; this release still serves no
    // write, and the row reports what the file says (case 3.5).
    assert_eq!(rows[1]["read_only"], json!(false));
    assert_eq!(rows[1]["index_present"], json!(false));
    assert_eq!(rows[1]["index_stale"], Value::Null);
    assert!(
        !dir.path().join("archive/.janus").exists(),
        "a status call must not build the corpus it reports"
    );
}

#[test]
fn case_3_3_a_single_corpus_status_keeps_its_shape() {
    // No workspace anywhere, so the answer is the one `main` gives: the same
    // nine keys and no `corpora`.
    let dir = copied();
    let report = json(&dir.path().join("primary"), &["status", "--json"]);
    let keys: BTreeSet<&str> = report
        .as_object()
        .expect("a status object")
        .keys()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<&str> = [
        "api",
        "cache",
        "corpus_id",
        "embeddings",
        "index",
        "metadata",
        "ok",
        "vault",
        "writes_enabled",
    ]
    .into_iter()
    .collect();
    assert_eq!(keys, expected, "a single-corpus status gains no field");
}

// ──────────────────────────── the three surfaces ───────────────────────────

#[test]
fn case_3_4_the_five_tools_answer_the_corpus_parameter_alike() {
    let dir = copied();
    let workspace = workspace(dir.path());

    // `read_document`: the CLI, the rpc loop and the MCP server agree byte for
    // byte, because each one encodes the same value.
    let cli = json(
        dir.path(),
        &[
            "read",
            "--workspace",
            &workspace,
            "--corpus",
            "context",
            "--json",
            SHARED,
        ],
    );
    let over_rpc = rpc(
        dir.path(),
        "read_document",
        json!({ "path": SHARED, "corpus": "context" }),
    );
    assert_eq!(over_rpc, cli, "rpc answers what the CLI answers");
    let over_mcp = mcp(
        dir.path(),
        false,
        "read_document",
        json!({ "path": SHARED, "corpus": "context" }),
    );
    assert_eq!(over_mcp, cli, "MCP answers what the CLI answers");

    // The qualifier works over rpc too, and names the same corpus.
    let qualified = rpc(
        dir.path(),
        "read_document",
        json!({ "path": format!("context:{SHARED}") }),
    );
    assert_eq!(qualified["corpus"], json!("context"));
    assert_eq!(qualified["body"], json!(ARCHIVED_BODY));

    // `search` over rpc serves `all`, which one engine could not: the rows and
    // their corpora are the ones the CLI returns. `taken_ms` is a duration and
    // is deliberately not compared.
    let cli_all = json(
        dir.path(),
        &[
            "search",
            "--workspace",
            &workspace,
            "--corpus",
            "all",
            "--json",
            "cache",
        ],
    );
    let rpc_all = rpc(
        dir.path(),
        "search",
        json!({ "query": "cache", "corpus": "all" }),
    );
    assert_eq!(rows(&rpc_all), rows(&cli_all));
    assert_eq!(rpc_all["corpora"], cli_all["corpora"]);
    assert_eq!(rows(&cli_all).len(), 2, "both corpora answered: {cli_all}");

    // `janus_status` over MCP reports the workspace's corpora.
    let status = mcp(dir.path(), false, "janus_status", json!({}));
    assert_eq!(status["workspace"], json!(workspace));
    let ids: Vec<&str> = status["corpora"]
        .as_array()
        .expect("a corpora array")
        .iter()
        .map(|row| row["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["main", "context"]);
}

#[test]
fn case_3_5_a_write_tool_is_refused_whatever_the_corpus_says() {
    let dir = copied();
    // The archived corpus declares `read_only = false`, and the workspace is
    // named, so neither the role nor the flag can be read as a grant.
    let report = json(
        dir.path(),
        &["status", "--workspace", &workspace(dir.path()), "--json"],
    );
    assert_eq!(report["corpora"][1]["read_only"], json!(false));
    // The status command writes nothing, so it reports no write capability.
    assert_eq!(report["writes_enabled"], json!(false));

    // A server without the flag refuses the name; with the flag the name is
    // legal and its phase has not landed, so it is refused on its own account
    // (M6-7, M7-4).
    for (allow_write, expected) in [(false, "write_disabled"), (true, "refused")] {
        let answer = mcp(
            dir.path(),
            allow_write,
            "promote_document",
            json!({ "path": SHARED, "body": "rewritten" }),
        );
        assert_eq!(answer["ok"], json!(false), "{answer}");
        assert_eq!(answer["code"], json!(expected), "{answer}");
    }
    // The archived document is still the bytes the fixture holds.
    let read = json(
        dir.path(),
        &[
            "read",
            "--workspace",
            &workspace(dir.path()),
            "--corpus",
            "context",
            "--json",
            SHARED,
        ],
    );
    assert_eq!(read["body"], json!(ARCHIVED_BODY));
}
