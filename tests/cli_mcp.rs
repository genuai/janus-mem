//! Cases 6.1–6.6 and 10.2 of the M6 plan: the MCP server (M6 §3.6, §3.10).
//!
//! Every test drives the binary, writes a whole script to stdin and parses every
//! stdout line, so a line that is not a JSON-RPC message fails the test that
//! reads it. No test sleeps, opens a port or starts a thread. Every expected
//! value is written out by hand.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use common::{fixture_vault, mcp, mcp_call, mcp_opening, write_document};
use janus_mem::api::dto::{ReadParams, SearchParams};
use janus_mem::mcp::protocol::SUPPORTED_VERSIONS;
use serde_json::{Value, json};

/// One session on an indexed fixture, opened with the handshake.
fn session(root: &Path, extra: &str) -> Vec<Value> {
    let answers = mcp(root, false, &format!("{}{extra}", mcp_opening())).answers;
    // The handshake answers first, and then one answer per request line.
    assert_eq!(answers[0]["id"], json!(1), "{answers:?}");
    answers
}

/// One corpus with one document and no index.
fn scratch(id: &str, rel: &str, text: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        format!("index_dir = \".janus\"\ncorpus_id = \"{id}\"\n"),
    )
    .expect("write the config");
    write_document(dir.path(), rel, text);
    dir
}

/// A decision document a hand-derived expectation can name.
const DECISION: &str = "---\ntitle: Cache decision\ntype: decision\nstatus: active\ndate: 2024-01-01\n---\n\ncache decision\n";

/// The tools one session lists.
fn listed(root: &Path, allow_write: bool) -> Vec<Value> {
    let script = format!(
        "{}{}\n",
        mcp_opening(),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })
    );
    let answers = mcp(root, allow_write, &script).answers;
    answers[1]["result"]["tools"]
        .as_array()
        .expect("a tools array")
        .clone()
}

/// The keys of one encoded parameter struct.
fn dto_fields(value: Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("a parameter struct encodes as an object")
        .keys()
        .cloned()
        .collect()
}

/// The property names of one listed tool's schema.
fn schema_fields(tools: &[Value], name: &str) -> BTreeSet<String> {
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == name)
        .expect("a listed tool");
    tool["inputSchema"]["properties"]
        .as_object()
        .expect("a properties object")
        .keys()
        .cloned()
        .collect()
}

// ──────────────────────────────── handshake ────────────────────────────────

#[test]
fn case_6_1_initialize_answers_the_current_version() {
    let dir = fixture_vault();
    let answers = session(dir.path(), "");
    let answer = &answers[0];
    assert_eq!(answer["jsonrpc"], json!("2.0"));
    assert_eq!(answer["id"], json!(1));
    let result = &answer["result"];
    assert_eq!(result["protocolVersion"], json!("2025-06-18"), "{result}");
    assert_eq!(result["capabilities"], json!({ "tools": {} }), "{result}");
    assert_eq!(
        result["serverInfo"],
        json!({ "name": "janus-mem", "version": env!("CARGO_PKG_VERSION") }),
        "{result}"
    );
}

#[test]
fn case_6_2_a_supported_version_is_echoed() {
    // Every member of the supported set, named here so a member this test does
    // not name fails the comparison below.
    const SUPPORTED: [&str; 2] = ["2025-06-18", "2025-03-26"];
    assert_eq!(SUPPORTED_VERSIONS, SUPPORTED);
    let dir = scratch("echo", "notes/one.md", DECISION);
    for (index, version) in SUPPORTED.iter().enumerate() {
        let script = format!(
            "{}\n",
            json!({
                "jsonrpc": "2.0",
                "id": index,
                "method": "initialize",
                "params": { "protocolVersion": version },
            })
        );
        let answers = mcp(dir.path(), false, &script).answers;
        assert_eq!(
            answers[0]["result"]["protocolVersion"],
            json!(version),
            "{version} must be echoed"
        );
    }
}

#[test]
fn case_6_3_an_unsupported_version_gets_the_newest_one() {
    let dir = scratch("latest", "notes/one.md", DECISION);
    let script = format!(
        "{}\n",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2019-01-01" },
        })
    );
    let answers = mcp(dir.path(), false, &script).answers;
    // The server answers with the newest revision it speaks, and the client may
    // continue or disconnect. This one continues.
    assert_eq!(answers[0]["result"]["protocolVersion"], json!("2025-06-18"));
}

#[test]
fn case_6_4_no_notification_is_answered() {
    let dir = scratch("quiet", "notes/one.md", DECISION);
    let script = format!(
        "{}{}\n{}\n{}\n{}\n",
        mcp_opening(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 9 } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/nobody/knows/this" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" })
    );
    let session = mcp(dir.path(), false, &script);
    // The handshake request and the ping: the three notifications answer nothing.
    assert_eq!(session.answers.len(), 2, "{}", session.stdout);
    assert_eq!(session.stdout.lines().count(), 2, "{}", session.stdout);
    assert_eq!(session.answers[1]["result"], json!({}));
}

#[test]
fn case_6_5_a_request_before_initialize_is_refused() {
    let dir = fixture_vault();
    // No handshake first, so the list is refused and the session stays alive.
    let script = format!(
        "{}\n{}\n{}\n",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" })
    );
    let answers = mcp(dir.path(), false, &script).answers;
    assert_eq!(answers.len(), 3, "{answers:?}");
    assert_eq!(answers[0]["error"]["code"], json!(-32002), "{answers:?}");
    assert!(answers[0].get("result").is_none(), "{}", answers[0]);
    assert!(
        !answers[2]["result"]["tools"]
            .as_array()
            .expect("a tools array")
            .is_empty(),
        "the answered list must not be empty"
    );
}

#[test]
fn case_6_6_every_protocol_error_is_answered_and_the_loop_survives() {
    let dir = fixture_vault();
    let script = format!(
        "{}{}\n{}\n{}\n{}\n{}\n{}\n",
        mcp_opening(),
        "this is not JSON",
        "[1, 2, 3]",
        json!({ "jsonrpc": "2.0", "id": 4 }),
        json!({ "jsonrpc": "2.0", "id": 5, "method": 7 }),
        json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/nope" }),
        // The last line proves the loop survived every error above.
        json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" }),
    );
    let answers = mcp(dir.path(), false, &script).answers;
    assert_eq!(answers.len(), 7, "{answers:?}");
    assert_eq!(answers[1]["error"]["code"], json!(-32700), "{answers:?}");
    assert_eq!(answers[1]["id"], json!(null));
    // An array, a missing method and a method that is not a string: all three
    // are JSON that is not a request object.
    for answer in &answers[2..5] {
        assert_eq!(answer["error"]["code"], json!(-32600), "{answer}");
    }
    assert_eq!(answers[2]["id"], json!(null), "no request, no id");
    assert_eq!(
        answers[3]["id"],
        json!(4),
        "a request that names itself keeps its id"
    );
    assert_eq!(answers[5]["error"]["code"], json!(-32601), "{answers:?}");
    assert_eq!(answers[6]["result"], json!({}), "the loop survived");
}

#[test]
fn every_tool_schema_matches_its_parameter_struct() {
    // The schema is written by hand, so this test is what keeps it equal to the
    // struct the engine deserializes. A field that moves in either place fails
    // here instead of becoming a silent default (§11).
    let dir = scratch("drift", "notes/one.md", DECISION);
    let tools = listed(dir.path(), false);
    let search = SearchParams {
        query: String::new(),
        limit: None,
        intent: None,
        as_of: None,
        scope: None,
        scope_only: None,
        types: None,
        tags: None,
        include_superseded: None,
        decay_half_life_days: None,
    };
    let read = ReadParams {
        path: String::new(),
        max_chars: None,
        chunk_id: None,
    };
    let pairs: [(&str, BTreeSet<String>); 2] = [
        ("search", dto_fields(json!(search))),
        ("read_document", dto_fields(json!(read))),
    ];
    for (name, fields) in pairs {
        assert_eq!(fields, schema_fields(&tools, name), "{name}");
    }
}

// ───────────────────────────── stdout discipline ───────────────────────────

#[test]
fn case_10_2_a_whole_session_parses_as_json_rpc() {
    // A config warning and no index yet: two diagnostics for stderr across the
    // first build, and not one byte of them on stdout.
    let dir = scratch("quiet", "notes/one.md", DECISION);
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "[authority]\nweird = 0.0\n\n[embedding]\nmodel = \"hash-64\"\n",
    )
    .expect("write the config");
    let script = format!(
        "{}{}{}{}\n",
        mcp_opening(),
        mcp_call(2, "search", json!({ "query": "cache" })),
        mcp_call(3, "read_document", json!({ "path": "notes/one.md" })),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" })
    );
    let session = mcp(root, false, &script);
    assert_eq!(session.code, Some(0), "stderr:\n{}", session.stderr);
    assert_eq!(session.answers.len(), 4, "{}", session.stdout);
    for (index, answer) in session.answers.iter().enumerate() {
        assert_eq!(answer["jsonrpc"], json!("2.0"), "{answer}");
        assert_eq!(answer["id"], json!(index + 1), "{answer}");
        assert!(
            answer.get("result").is_some() || answer.get("error").is_some(),
            "{answer}"
        );
    }
    assert!(
        !session.stdout.contains("refresh:") && !session.stdout.contains("warning:"),
        "a diagnostic reached stdout:\n{}",
        session.stdout
    );
    assert!(
        session.stderr.contains("warning:"),
        "stderr:\n{}",
        session.stderr
    );
    assert!(
        session.stderr.contains("refresh: no index yet"),
        "stderr:\n{}",
        session.stderr
    );
}

#[test]
fn mcp_startup_failure_exits_before_the_loop() {
    // §3.6 states this rule without a case: a startup failure is the one thing
    // that exits, and it exits before any line is read.
    let dir = tempfile::tempdir().expect("tempdir");
    let script = format!(
        "{}\n",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })
    );
    let session = mcp(dir.path(), false, &script);
    assert_eq!(session.code, Some(1));
    assert!(session.stdout.is_empty(), "{}", session.stdout);
    assert!(session.answers.is_empty());
    let config = fs::canonicalize(dir.path())
        .expect("a canonical temporary directory")
        .join("janus.toml");
    assert!(
        session
            .stderr
            .contains(&format!("error: no corpus config at {}", config.display())),
        "stderr:\n{}",
        session.stderr
    );
}
