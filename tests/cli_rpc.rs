//! Cases 5.1–5.8 and 10.1 of the M6 plan: the line-delimited loop (M6 §3.5,
//! §3.8, §3.10).
//!
//! Every test drives the binary, writes a whole script to stdin and parses
//! every stdout line, so a line that is not a response fails the test that
//! reads it. No test sleeps, opens a port or starts a thread. Every expected
//! value is written out by hand.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{fixture_vault, index, janus, write_document};
use janus_mem::api::write_refusal;
use janus_mem::index::store::Lock;
use serde_json::{Value, json};

/// The fixture document every path-shaped call names.
const POSTGRES: &str = "decisions/2024-11-02-postgres-cache.md";

/// The five wire names, in `Tool::ALL` order (case 5.4).
const WIRE_NAMES: &str =
    "search, get_authoritative_facts, get_lineage, read_document, janus_status";

/// One finished `janus rpc` session.
struct Session {
    /// Every stdout line, parsed as one JSON object.
    answers: Vec<Value>,
    /// Stdout as text, so a test can count lines and look for a diagnostic.
    stdout: String,
    /// Stderr as text. Diagnostics belong here and nowhere else.
    stderr: String,
    /// The exit code, when the process exited rather than died by a signal.
    code: Option<i32>,
}

/// Runs one `janus rpc` session: the script goes to stdin, the answers come
/// back parsed.
fn rpc_session(root: &Path, allow_write: bool, script: &str) -> Session {
    let mut command = janus();
    command
        .env("RUST_LOG", "off")
        .arg("--vault")
        .arg(root)
        .arg("rpc");
    if allow_write {
        command.arg("--allow-write");
    }
    let output = command.write_stdin(script).output().expect("run janus rpc");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let answers = stdout
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("stdout line is not JSON ({err}):\n{line}"))
        })
        .collect();
    Session {
        answers,
        stdout,
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        code: output.status.code(),
    }
}

/// The answers of one script, which must all be present.
fn answers(root: &Path, allow_write: bool, script: &str) -> Vec<Value> {
    let session = rpc_session(root, allow_write, script);
    assert_eq!(session.code, Some(0), "stderr:\n{}", session.stderr);
    session.answers
}

/// A corpus with one config line and no `[mcp]` section.
fn scratch(id: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        format!("index_dir = \".janus\"\ncorpus_id = \"{id}\"\n"),
    )
    .expect("write the config");
    write_document(
        dir.path(),
        "notes/one.md",
        "---\ntitle: One\ndate: 2024-01-01\n---\n\nfirst prose\n",
    );
    dir
}

/// One answer with the two process-local facts removed (§3.5, case 5.7).
///
/// `id` names the request line and the CLI has no request to name. `taken_ms`
/// is a duration and `cache` counts calls in this process, so neither can be
/// equal across two processes.
fn normalized(mut answer: Value) -> Value {
    let Some(object) = answer.as_object_mut() else {
        return answer;
    };
    object.remove("id");
    if object.contains_key("taken_ms") {
        object.insert("taken_ms".to_owned(), json!(0));
    }
    if object.contains_key("cache") {
        object.insert("cache".to_owned(), Value::Null);
    }
    answer
}

// ──────────────────────────────── framing ──────────────────────────────────

#[test]
fn case_5_1_two_requests_answer_two_lines_in_order() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    // The blank and space-only lines answer nothing, and the last request has
    // no trailing newline: end of input must still answer it, and exit 0.
    let script = format!(
        "{}\n\n   \n{}\n{}",
        json!({ "id": 1, "tool": "janus_status" }),
        json!({ "id": 2, "tool": "janus_status" }),
        json!({ "id": 3, "tool": "janus_status" })
    );
    let session = rpc_session(root, false, &script);
    assert_eq!(session.code, Some(0), "stderr:\n{}", session.stderr);
    assert_eq!(session.answers.len(), 3, "{}", session.stdout);
    assert_eq!(session.stdout.lines().count(), 3, "{}", session.stdout);
    assert_eq!(
        session
            .answers
            .iter()
            .map(|answer| answer["id"].clone())
            .collect::<Vec<Value>>(),
        vec![json!(1), json!(2), json!(3)]
    );
    for answer in &session.answers {
        assert_eq!(answer["api"], json!(1));
        assert_eq!(answer["ok"], json!(true));
        assert_eq!(answer["corpus_id"], json!("fixtures"));
    }
}

#[test]
fn case_5_2_a_malformed_line_answers_once_and_the_loop_continues() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    let script = format!(
        "this is not JSON\n[1, 2, 3]\n{}\n{}\n",
        json!({ "id": 3, "tool": 7 }),
        json!({ "id": 4, "tool": "janus_status" })
    );
    let answers = answers(root, false, &script);
    assert_eq!(answers.len(), 4, "{answers:?}");
    // Three lines had no request shape, so there is no `id` to echo.
    for answer in &answers[..3] {
        assert_eq!(answer["id"], json!(null), "{answer}");
        assert_eq!(answer["api"], json!(1), "{answer}");
        assert_eq!(answer["ok"], json!(false), "{answer}");
        assert_eq!(answer["code"], json!("usage"), "{answer}");
        let message = answer["message"].as_str().expect("a message");
        assert!(!message.is_empty(), "{answer}");
        let hint = answer["hint"].as_str().expect("a hint");
        assert!(hint.contains("request"), "{hint}");
    }
    // The next line is answered normally.
    assert_eq!(answers[3]["id"], json!(4));
    assert_eq!(answers[3]["ok"], json!(true));
}

#[test]
fn case_5_3_the_answer_echoes_the_request_id() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    let script = format!(
        "{}\n{}\n{}\n{}\n",
        json!({ "id": 41, "tool": "janus_status" }),
        json!({ "id": "abc", "tool": "janus_status" }),
        json!({ "id": null, "tool": "janus_status" }),
        json!({ "tool": "janus_status" })
    );
    let answers = answers(root, false, &script);
    assert_eq!(
        answers
            .iter()
            .map(|answer| answer["id"].clone())
            .collect::<Vec<Value>>(),
        vec![json!(41), json!("abc"), json!(null), json!(null)]
    );
}

#[test]
fn case_5_4_an_unknown_tool_lists_the_five_wire_names() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    let script = format!(
        "{}\n{}\n",
        json!({ "id": 5, "tool": "janus_search" }),
        json!({ "id": 6, "tool": "Search" })
    );
    let answers = answers(root, false, &script);
    for answer in &answers {
        assert_eq!(answer["ok"], json!(false), "{answer}");
        assert_eq!(answer["code"], json!("usage"), "{answer}");
        assert_eq!(answer["hint"], json!(format!("one of: {WIRE_NAMES}")));
    }
    assert_eq!(answers[0]["id"], json!(5));
    assert_eq!(answers[1]["id"], json!(6));
}

#[test]
fn case_5_5_a_changed_corpus_refreshes_without_touching_stdout() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    let path = root.join(POSTGRES);
    let text = fs::read_to_string(&path).expect("the fixture document");
    fs::write(&path, format!("{text}\nmore prose\n")).expect("edit the document");

    let script = format!(
        "{}\n",
        json!({ "id": 7, "tool": "search", "params": { "query": "cache" } })
    );
    let session = rpc_session(root, false, &script);
    assert_eq!(session.code, Some(0), "stderr:\n{}", session.stderr);
    assert_eq!(session.answers.len(), 1, "{}", session.stdout);
    assert_eq!(session.answers[0]["ok"], json!(true));
    // The read refreshed, so the answer it reports is not stale.
    assert_eq!(session.answers[0]["index"]["stale"], json!(false));
    assert!(
        !session.stdout.contains("refresh:") && !session.stdout.contains("warning:"),
        "a diagnostic reached stdout:\n{}",
        session.stdout
    );
    assert!(
        session.stderr.contains("refresh: corpus changed"),
        "stderr:\n{}",
        session.stderr
    );
}

#[test]
fn case_5_6_a_bare_status_request_builds_nothing_and_takes_no_lock() {
    let dir = scratch("bare");
    let root = dir.path();
    // No `id` and no `params`: the answer still names the request as `null`.
    let script = format!("{}\n", json!({ "tool": "janus_status" }));
    let first = answers(root, false, &script);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["id"], json!(null));
    assert_eq!(first[0]["ok"], json!(true));
    assert_eq!(first[0]["index"]["present"], json!(false));
    assert!(
        !root.join(".janus").exists(),
        "a status call must not build or take a lock"
    );

    // A writer holds the index: status still answers, and it waits for nothing.
    index(root);
    let lock =
        Lock::acquire(&root.join(".janus"), Duration::from_secs(1)).expect("hold the writer lock");
    let held = answers(root, false, &script);
    assert_eq!(held[0]["ok"], json!(true), "{:?}", held[0]);
    assert_eq!(held[0]["index"]["present"], json!(true));
    assert_eq!(held[0]["index"]["stale"], json!(false));
    assert_eq!(held[0]["index"]["docs"], json!(1));
    drop(lock);
}

// ──────────────────────────────── parity ───────────────────────────────────

#[test]
fn case_5_7_the_five_calls_equal_the_five_cli_calls() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    // One rpc request and its CLI equivalent, from the arguments a human types.
    let calls: [(Value, Vec<&str>); 5] = [
        (
            json!({ "id": 1, "tool": "search",
                    "params": { "query": "cache", "intent": "current", "limit": 3 } }),
            vec![
                "search", "cache", "--intent", "current", "--limit", "3", "--json",
            ],
        ),
        (
            json!({ "id": 2, "tool": "get_lineage", "params": { "path": POSTGRES } }),
            vec!["lineage", POSTGRES, "--json"],
        ),
        (
            json!({ "id": 3, "tool": "get_authoritative_facts",
                    "params": { "tags": ["principle"], "limit": 3 } }),
            vec!["facts", "--tags", "principle", "--limit", "3", "--json"],
        ),
        (
            json!({ "id": 4, "tool": "read_document", "params": { "path": POSTGRES } }),
            vec!["read", POSTGRES, "--json"],
        ),
        (
            json!({ "id": 5, "tool": "janus_status", "params": {} }),
            vec!["status", "--json"],
        ),
    ];
    let script: String = calls
        .iter()
        .map(|(request, _)| format!("{request}\n"))
        .collect();
    let session = rpc_session(root, false, &script);
    assert_eq!(session.code, Some(0), "stderr:\n{}", session.stderr);
    assert_eq!(session.answers.len(), 5, "{}", session.stdout);

    for ((request, args), answer) in calls.iter().zip(&session.answers) {
        assert_eq!(answer["id"], request["id"], "{}", session.stdout);
        let from_cli = common::json_ok(root, args);
        assert_eq!(normalized(answer.clone()), normalized(from_cli), "{args:?}");
    }

    // Each process counts its own calls: the rpc session answered `search`
    // with a miss that cached the load, then `lineage` and
    // `get_authoritative_facts` with hits. `read_document` loads no generation,
    // and `janus_status` reports the counters without adding to them.
    assert_eq!(
        session.answers[4]["cache"],
        json!({ "hits": 2, "misses": 1 })
    );
    // The CLI answered five separate processes, so this one counts nothing.
    let cli_status = common::json_ok(root, &["status", "--json"]);
    assert_eq!(cli_status["cache"], json!({ "hits": 0, "misses": 0 }));
}

// ──────────────────────────────── refusal ──────────────────────────────────

#[test]
fn case_5_8_a_write_tool_is_refused_by_the_shared_helper() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    const WRITE: [&str; 4] = [
        "update_frontmatter",
        "write_document",
        "supersede_document",
        "promote_document",
    ];
    for allow_write in [false, true] {
        let script: String = WRITE
            .iter()
            .enumerate()
            .map(|(index, name)| format!("{}\n", json!({ "id": index, "tool": name })))
            .collect();
        let answers = answers(root, allow_write, &script);
        assert_eq!(answers.len(), 4, "{answers:?}");
        for (index, (name, answer)) in WRITE.iter().zip(&answers).enumerate() {
            assert_eq!(answer["id"], json!(index), "{answer}");
            assert_eq!(answer["api"], json!(1), "{answer}");
            assert_eq!(answer["ok"], json!(false), "{answer}");
            assert_eq!(answer["code"], json!("refused"), "{answer}");
            let hint = answer["hint"].as_str().expect("a hint");
            assert!(hint.contains("M7"), "{hint}");
            // The same object comes from the shared helper, so rpc did not
            // re-implement the refusal.
            let shared = write_refusal(name, allow_write).expect("a write name is refused");
            assert_eq!(
                normalized(answer.clone()),
                serde_json::to_value(shared).expect("a failure is JSON"),
                "{name}"
            );
        }
    }
    // A read name is not refused: it goes to the tool table.
    assert!(write_refusal("search", false).is_none());
    assert!(write_refusal("search", true).is_none());
}

// ───────────────────────────── stdout discipline ───────────────────────────

#[test]
fn case_10_1_the_rpc_loop_keeps_stderr_off_stdout() {
    // A config warning and a changed corpus: both are diagnostics, and both
    // belong on stderr (case 10.1).
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "[authority]\nweird = 0.0\n\n[embedding]\nmodel = \"hash-64\"\n",
    )
    .expect("write the config");
    let rel = "decisions/2020-01-01-cache.md";
    let text =
        "---\ntitle: Cache\ntype: decision\nstatus: active\ndate: 2020-01-01\n---\n\ncache\n";
    write_document(root, rel, text);
    index(root);
    // One edit, so the read has something to refresh.
    fs::write(root.join(rel), format!("{text}cache\n")).expect("edit the document");

    let script = format!(
        "{}\n",
        json!({ "id": 1, "tool": "search", "params": { "query": "cache" } })
    );
    let session = rpc_session(root, false, &script);
    assert_eq!(session.code, Some(0), "stderr:\n{}", session.stderr);
    assert_eq!(session.stdout.lines().count(), 1, "{}", session.stdout);
    assert_eq!(session.answers[0]["ok"], json!(true));
    assert!(
        !session.stdout.contains("warning:") && !session.stdout.contains("refresh:"),
        "a diagnostic reached stdout:\n{}",
        session.stdout
    );
    assert!(
        session.stderr.contains("warning:"),
        "stderr:\n{}",
        session.stderr
    );
    assert!(
        session.stderr.contains("refresh:"),
        "stderr:\n{}",
        session.stderr
    );
}
