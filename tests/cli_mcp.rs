//! Cases 6.1–6.7, 7.1–7.10, 8.1, 8.2 and 10.2 of the M6 plan: the MCP server
//! (M6 §3.6, §3.7, §3.8, §3.10).
//!
//! Every test drives the binary, writes a whole script to stdin and parses every
//! stdout line, so a line that is not a JSON-RPC message fails the test that
//! reads it. No test sleeps, opens a port or starts a thread. Every expected
//! value is written out by hand.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{
    fixture_vault, graph_vault, index, json_ok, mcp, mcp_call, mcp_envelope, mcp_opening,
    write_document,
};
use janus_mem::api::dto::{FactsParams, LineageParams, ReadParams, SearchParams, StatusParams};
use janus_mem::api::{Writes, write_refusal};
use janus_mem::index::store::Lock;
use janus_mem::mcp::protocol::{INSTRUCTIONS, SUPPORTED_VERSIONS};
use serde_json::{Value, json};

/// The fixture document every path-shaped call names.
const POSTGRES: &str = "decisions/2024-11-02-postgres-cache.md";

/// The five wire names, in `tools/list` order (case 6.7).
const WIRE_ORDER: [&str; 5] = [
    "search",
    "get_lineage",
    "get_authoritative_facts",
    "read_document",
    "janus_status",
];

/// The four names of §9.3, which this release lists nowhere (case 8.1).
const WRITE_NAMES: [&str; 4] = [
    "update_frontmatter",
    "write_document",
    "supersede_document",
    "promote_document",
];

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

/// The five listed tools of one session.
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
fn case_9_2_initialize_ships_the_instructions_verbatim() {
    // Product copy, written out by hand: a reworded sentence is a product
    // change, so it must fail here rather than slip into the handshake.
    const EXPECTED: &str = concat!(
        "You are reading a janus-mem corpus: Markdown files, each with a type and a date.\n\n",
        "The type sets how much a file counts in a search, from idea at the bottom to decision at the top. The date sets how fast it fades. Principles, specs and decisions never fade.\n\n",
        "The ladder is idea, note, journal, finding, principle, spec, decision.\n\n",
        "When a note turns out to matter, it is time to promote it. Ask the user to restate it as a principle, a spec or a decision, with its own date. A promoted note keeps answering questions after the note itself would have faded.\n\n",
        "Tags say what a file is about. The type says how much it counts.\n\n",
        "search gives you spans and scores. read_document gives you the words.\n\n",
        "update_frontmatter changes frontmatter keys and returns a receipt. It needs expected_hash: the content_hash of a read of that document. Without it the call changes nothing and answers precondition_required; with a stale one it answers conflict. write_document writes a whole file: a frontmatter block, then the body. mode create refuses a path that exists; mode overwrite needs expected_hash too. A write needs a .md or .markdown path. A server started without --allow-write refuses every write."
    );
    assert_eq!(INSTRUCTIONS, EXPECTED);
    let dir = fixture_vault();
    let answers = session(dir.path(), "");
    assert_eq!(answers[0]["result"]["instructions"], json!(EXPECTED));
}

#[test]
fn case_9_2_no_other_answer_carries_the_instructions() {
    let dir = fixture_vault();
    let script = format!(
        "{}{}\n{}\n{}\n",
        mcp_opening(),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "search", "arguments": { "query": "cache" } },
        })
    );
    let answers = mcp(dir.path(), false, &script).answers;
    // One handshake and three answers, so every later line was read.
    assert_eq!(answers.len(), 4, "{answers:?}");
    assert!(
        answers[0]["result"]["instructions"].is_string(),
        "{answers:?}"
    );
    for answer in &answers[1..] {
        assert!(
            answer["result"].get("instructions").is_none(),
            "only the handshake carries instructions: {answer}"
        );
    }
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
    assert_eq!(
        answers[2]["result"]["tools"].as_array().map(Vec::len),
        Some(5)
    );
}

#[test]
fn case_6_6_every_protocol_error_is_answered_and_the_loop_survives() {
    let dir = fixture_vault();
    let script = format!(
        "{}{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        mcp_opening(),
        "this is not JSON",
        "[1, 2, 3]",
        json!({ "jsonrpc": "2.0", "id": 4 }),
        json!({ "jsonrpc": "2.0", "id": 5, "method": 7 }),
        json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call",
                "params": { "name": "search", "arguments": { "query": 5 } } }),
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/nope" }),
        // The last line proves the loop survived all four errors.
        json!({ "jsonrpc": "2.0", "id": 8, "method": "ping" }),
    );
    let answers = mcp(dir.path(), false, &script).answers;
    assert_eq!(answers.len(), 8, "{answers:?}");
    assert_eq!(answers[1]["error"]["code"], json!(-32700), "{answers:?}");
    assert_eq!(answers[1]["id"], json!(null));
    // An array, a missing method and a method that is not a string: all three
    // are JSON that is not a request object.
    for answer in &answers[2..5] {
        assert_eq!(answer["error"]["code"], json!(-32600), "{answer}");
    }
    assert_eq!(answers[2]["id"], json!(null), "no request, no id");
    assert_eq!(answers[3]["id"], json!(4));
    // A bad argument names the field, and an unknown method names itself.
    let message = answers[5]["error"]["message"].as_str().expect("a message");
    assert!(message.contains("query"), "{message}");
    assert_eq!(answers[5]["error"]["code"], json!(-32602), "{answers:?}");
    assert_eq!(answers[6]["error"]["code"], json!(-32601), "{answers:?}");
    assert_eq!(answers[7]["result"], json!({}), "the loop survived");
}

#[test]
fn case_6_7_tools_list_is_five_tools_in_wire_order() {
    let dir = fixture_vault();
    let tools = listed(dir.path(), false);
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(names, WIRE_ORDER);
    for tool in &tools {
        let description = tool["description"].as_str().expect("a description");
        assert!(!description.is_empty(), "{tool}");
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], json!("object"), "{tool}");
        assert_eq!(schema["additionalProperties"], json!(false), "{tool}");
        assert!(schema["properties"].is_object(), "{tool}");
        assert!(schema["required"].is_array(), "{tool}");
    }
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
        corpus: None,
    };
    let facts = FactsParams {
        query: None,
        tags: None,
        types: None,
        scope: None,
        limit: None,
        corpus: None,
    };
    let lineage = LineageParams {
        path: None,
        topic: None,
    };
    let read = ReadParams {
        path: String::new(),
        max_chars: None,
        chunk_id: None,
        corpus: None,
    };
    let pairs: [(&str, BTreeSet<String>); 5] = [
        ("search", dto_fields(json!(search))),
        ("get_authoritative_facts", dto_fields(json!(facts))),
        ("get_lineage", dto_fields(json!(lineage))),
        ("read_document", dto_fields(json!(read))),
        ("janus_status", dto_fields(json!(StatusParams {}))),
    ];
    for (name, fields) in pairs {
        assert_eq!(fields, schema_fields(&tools, name), "{name}");
    }
}

// ────────────────────────────────── calls ──────────────────────────────────

#[test]
fn case_7_1_a_search_returns_the_envelope() {
    let dir = fixture_vault();
    index(dir.path());
    let answers = session(
        dir.path(),
        &mcp_call(2, "search", json!({ "query": "cache" })),
    );
    let envelope = mcp_envelope(&answers[1]);
    assert_eq!(envelope["api"], json!(1), "{envelope}");
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["intent"], json!("current"), "{envelope}");
    let results = envelope["results"].as_array().expect("results");
    assert!(!results.is_empty(), "{envelope}");
    for row in results {
        // A span and a chunk id, never the text: `read_document` is that path.
        assert!(row["span"].is_array(), "{row}");
        assert!(row["chunk_id"].is_string(), "{row}");
        assert!(row["content_hash"].is_string(), "{row}");
        assert!(row.get("text").is_none(), "{row}");
    }
}

#[test]
fn case_7_2_authoritative_facts_uses_the_cli_predicate() {
    // The graph fixture holds four superseded links and one active decision, so
    // a listing that includes a replaced document would fail here.
    let dir = graph_vault();
    let root = dir.path();
    index(root);
    let answers = session(
        root,
        &mcp_call(2, "get_authoritative_facts", json!({ "limit": 50 })),
    );
    let envelope = mcp_envelope(&answers[1]);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    let paths: Vec<&str> = envelope["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|row| {
            assert_eq!(row["effective_status"], json!("active"), "{row}");
            row["rel_path"].as_str().expect("rel_path")
        })
        .collect();
    assert!(
        paths.contains(&"chain/2022-01-01-cache-returns.md"),
        "{paths:?}"
    );
    let cli = json_ok(root, &["facts", "--limit", "50", "--json"]);
    assert_eq!(
        envelope["results"], cli["results"],
        "one predicate, two surfaces"
    );
}

#[test]
fn case_7_2b_the_types_filter_selects_the_named_types() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"typed\"\n",
    )
    .expect("write the config");
    write_document(
        root,
        "decisions/d.md",
        "---\ntitle: D\ntype: decision\nstatus: active\ndate: 2024-01-01\n---\n\none\n",
    );
    write_document(
        root,
        "principles/p.md",
        "---\ntitle: P\ntype: principle\nstatus: active\ndate: 2024-01-02\n---\n\ntwo\n",
    );
    write_document(
        root,
        "specs/s.md",
        "---\ntitle: S\ntype: spec\nstatus: active\ndate: 2024-01-03\n---\n\nthree\n",
    );
    let answers = session(
        root,
        &mcp_call(
            2,
            "get_authoritative_facts",
            json!({ "types": ["decision", "principle"] }),
        ),
    );
    let envelope = mcp_envelope(&answers[1]);
    let mut paths: Vec<&str> = envelope["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|row| row["rel_path"].as_str().expect("rel_path"))
        .collect();
    paths.sort_unstable();
    // Three files exist; the `spec` is the one the filter drops.
    assert_eq!(paths, ["decisions/d.md", "principles/p.md"]);
}

#[test]
fn case_7_3_a_bad_lineage_combination_is_a_tool_failure() {
    let dir = fixture_vault();
    index(dir.path());
    let script = format!(
        "{}{}{}",
        mcp_opening(),
        mcp_call(2, "get_lineage", json!({})),
        mcp_call(
            3,
            "get_lineage",
            json!({ "path": POSTGRES, "topic": "cache" })
        )
    );
    let answers = mcp(dir.path(), false, &script).answers;
    for answer in &answers[1..] {
        // The shape is legal, so the answer is a result and not a protocol error.
        assert!(answer.get("error").is_none(), "{answer}");
        assert_eq!(answer["result"]["isError"], json!(true), "{answer}");
        let envelope = mcp_envelope(answer);
        assert_eq!(envelope["ok"], json!(false), "{envelope}");
        assert_eq!(envelope["code"], json!("usage"), "{envelope}");
    }
}

#[test]
fn case_7_4_an_unknown_intent_is_a_tool_failure() {
    let dir = fixture_vault();
    index(dir.path());
    let answers = session(
        dir.path(),
        &mcp_call(2, "search", json!({ "query": "cache", "intent": "bogus" })),
    );
    assert!(answers[1].get("error").is_none(), "{}", answers[1]);
    assert_eq!(answers[1]["result"]["isError"], json!(true));
    let envelope = mcp_envelope(&answers[1]);
    assert_eq!(envelope["code"], json!("usage"), "{envelope}");
    let message = envelope["message"].as_str().expect("a message");
    assert!(message.contains("bogus"), "{message}");
}

#[test]
fn case_7_5_the_envelope_arrives_twice() {
    let dir = fixture_vault();
    index(dir.path());
    let answers = session(dir.path(), &mcp_call(2, "janus_status", json!({})));
    let result = &answers[1]["result"];
    let content = result["content"].as_array().expect("content");
    assert_eq!(content.len(), 1, "{result}");
    assert_eq!(content[0]["type"], json!("text"), "{result}");
    // An older client that reads only `text` gets the same answer.
    let text = content[0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("the text is JSON");
    assert_eq!(&parsed, &result["structuredContent"], "{result}");
    assert_eq!(parsed["ok"], json!(true), "{parsed}");
    assert_eq!(result.get("isError"), None, "a success adds no isError");
}

#[test]
fn case_7_6_a_parameter_the_schema_lacks_is_refused() {
    let dir = fixture_vault();
    index(dir.path());
    let answers = session(
        dir.path(),
        &mcp_call(2, "search", json!({ "query": "cache", "max_chars": 20 })),
    );
    assert_eq!(answers[1]["error"]["code"], json!(-32602), "{}", answers[1]);
    let message = answers[1]["error"]["message"].as_str().expect("a message");
    assert!(message.contains("max_chars"), "{message}");
}

#[test]
fn case_7_7_a_missing_index_is_built_and_a_cold_mismatch_refuses() {
    let dir = scratch("cold", "notes/one.md", DECISION);
    let root = dir.path();
    let first = mcp(
        root,
        false,
        &format!(
            "{}{}",
            mcp_opening(),
            mcp_call(2, "search", json!({ "query": "cache" }))
        ),
    );
    assert_eq!(mcp_envelope(&first.answers[1])["ok"], json!(true));
    assert!(
        first.stderr.contains("refresh: no index yet"),
        "stderr:\n{}",
        first.stderr
    );

    // A format the reader cannot repair is a cold mismatch: the answer names the
    // command that cures it, and the generation is left alone.
    let path = root.join(".janus/manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("the manifest"))
            .expect("the manifest is JSON");
    manifest["format_version"] = json!(999);
    fs::write(&path, manifest.to_string()).expect("write the manifest");
    let second = session(root, &mcp_call(2, "search", json!({ "query": "cache" })));
    let envelope = mcp_envelope(&second[1]);
    assert_eq!(envelope["ok"], json!(false), "{envelope}");
    assert_eq!(
        envelope["code"],
        json!("index_needs_full_build"),
        "{envelope}"
    );
    assert_eq!(second[1]["result"]["isError"], json!(true));
}

#[test]
fn case_7_8_a_busy_writer_is_a_skip_and_not_an_error() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    // One edit, so this read has something it would refresh.
    let path = root.join(POSTGRES);
    let text = fs::read_to_string(&path).expect("the fixture document");
    fs::write(&path, format!("{text}\nmore prose\n")).expect("edit the document");

    let lock =
        Lock::acquire(&root.join(".janus"), Duration::from_secs(1)).expect("hold the writer lock");
    let session = mcp(
        root,
        false,
        &format!(
            "{}{}",
            mcp_opening(),
            mcp_call(2, "search", json!({ "query": "cache" }))
        ),
    );
    drop(lock);

    assert_eq!(session.code, Some(0), "stderr:\n{}", session.stderr);
    let envelope = mcp_envelope(&session.answers[1]);
    // The generation it loaded answers, and it says it is older than the corpus.
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["index"]["stale"], json!(true), "{envelope}");
    assert!(
        session.stderr.contains("refresh: skipped, index busy"),
        "stderr:\n{}",
        session.stderr
    );
}

#[test]
fn case_7_9_the_five_calls_equal_the_five_rpc_calls() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    // One call, as both adapters state it: the tool, its arguments, and the rpc
    // request built beside it.
    let calls: [(&str, Value); 5] = [
        (
            "search",
            json!({ "query": "cache", "intent": "current", "limit": 3 }),
        ),
        ("get_lineage", json!({ "path": POSTGRES })),
        (
            "get_authoritative_facts",
            json!({ "tags": ["principle"], "limit": 3 }),
        ),
        ("read_document", json!({ "path": POSTGRES })),
        ("janus_status", json!({})),
    ];
    let mut script = mcp_opening();
    let mut rpc_script = String::new();
    for (index, (name, arguments)) in calls.iter().enumerate() {
        let id = index + 2;
        script.push_str(&mcp_call(id as u64, name, arguments.clone()));
        rpc_script.push_str(&format!(
            "{}\n",
            json!({ "id": id, "tool": name, "params": arguments })
        ));
    }

    let output = common::janus()
        .env("RUST_LOG", "off")
        .arg("--vault")
        .arg(root)
        .arg("rpc")
        .write_stdin(rpc_script)
        .output()
        .expect("run janus rpc");
    let rpc_stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let from_rpc: Vec<Value> = rpc_stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("an rpc answer is JSON"))
        .collect();
    let from_mcp = mcp(root, false, &script).answers;
    // The MCP session opens with the handshake, which the rpc loop has no peer of.
    assert_eq!(from_mcp.len(), from_rpc.len() + 1, "{from_mcp:?}");

    // The only two process-local facts a comparison must drop are `id` (the rpc
    // answer names its request line) and `taken_ms` (a duration). The cache
    // counters count the same five calls in the same order in both processes, so
    // they are compared as they are.
    for (offset, (name, _)) in calls.iter().enumerate() {
        let mut p = from_mcp[1 + offset]["result"]["structuredContent"].clone();
        let mut r = from_rpc[offset].clone();
        r.as_object_mut().expect("an object").remove("id");
        for envelope in [&mut p, &mut r] {
            if envelope.get("taken_ms").is_some() {
                envelope["taken_ms"] = json!(0);
            }
        }
        assert_eq!(p, r, "{name}");
    }
}

#[test]
fn case_7_10_a_retired_node_carries_no_hash() {
    // A -> B, then A leaves the corpus. The tombstone keeps A's place in the
    // A is replaced by B, and B then leaves the corpus. The edge is declared by
    // the predecessor, because a tombstone declares no link of its own, so the
    // retired node is found as A's successor. A tombstone stores no hash, so the
    // answer says `null` for it and a hex digest for the live document
    // (M6 §2.7 rule 2).
    const A: &str = "decisions/2020-01-01-adopt-postgres.md";
    const B: &str = "decisions/2021-01-01-move-to-redis.md";
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"retired\"\n",
    )
    .expect("write the config");
    write_document(
        root,
        A,
        "---\ntitle: Adopt Postgres\ntype: decision\nstatus: superseded\ndate: 2020-01-01\nsuperseded_by: decisions/2021-01-01-move-to-redis.md\n---\n\nPostgres holds the cache.\n",
    );
    write_document(
        root,
        B,
        "---\ntitle: Move to Redis\ntype: decision\nstatus: active\ndate: 2021-01-01\n---\n\nThe cache moves out.\n",
    );
    index(root);
    fs::remove_file(root.join(B)).expect("remove the successor");
    index(root);

    let answers = session(root, &mcp_call(2, "get_lineage", json!({ "path": A })));
    let envelope = mcp_envelope(&answers[1]);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    let nodes = envelope["results"].as_array().expect("results");
    assert_eq!(nodes.len(), 2, "{envelope}");
    // Depth 0 is the live document, depth 1 the retired one.
    assert_eq!(nodes[0]["path"], json!(A), "{}", nodes[0]);
    let hash = nodes[0]["content_hash"].as_str().expect("a live hash");
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{hash}");
    assert_eq!(nodes[1]["path"], json!(B), "{}", nodes[1]);
    assert_eq!(nodes[1]["content_hash"], json!(null), "{}", nodes[1]);
}

// ──────────────────────────────── refusal ──────────────────────────────────

#[test]
fn case_8_1_the_write_tool_is_listed_only_when_the_server_serves_writes() {
    let dir = scratch("list", "notes/one.md", DECISION);

    // Without the flag the five read tools are the whole list: a client cannot
    // discover a write it would be refused (M7-4).
    let plain = listed(dir.path(), false);
    let names: Vec<&str> = plain
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(names, WIRE_ORDER);

    // With the flag the two served write tools join them, last.
    let served = listed(dir.path(), true);
    let names: Vec<&str> = served
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    let mut expected = WIRE_ORDER.to_vec();
    expected.extend(["update_frontmatter", "write_document"]);
    assert_eq!(names, expected);

    // The two whose phases have not landed are listed in neither mode.
    let unlanded = ["supersede_document", "promote_document"];
    for write in unlanded {
        assert!(!names.contains(&write), "{write} must not be listed");
    }
}

#[test]
fn case_8_2_a_write_name_is_refused_by_the_shared_helper() {
    let dir = fixture_vault();
    let root = dir.path();
    index(root);
    let writes = |allowed| Writes::new("janus mcp", allowed, false);
    // The two names whose phases have not landed.
    const UNLANDED: [&str; 2] = ["supersede_document", "promote_document"];

    // Without the flag every write name answers `write_disabled`.
    let answers = mcp(root, false, &write_script()).answers;
    assert_eq!(answers.len(), 5, "{answers:?}");
    for (index, name) in WRITE_NAMES.iter().enumerate() {
        let envelope = mcp_envelope(&answers[index + 1]);
        assert_eq!(envelope["ok"], json!(false), "{envelope}");
        assert_eq!(envelope["code"], json!("write_disabled"), "{envelope}");
        // The same object comes from the shared helper, so MCP did not
        // re-implement the gate.
        let shared = write_refusal(name, writes(false)).expect("a write name is refused");
        assert_eq!(
            envelope,
            &serde_json::to_value(shared).expect("a failure is JSON")
        );
    }

    // With the flag the unlanded two are still refused, and the served two
    // reach the engine: an empty request is a usage failure, not a refusal.
    let script: String = UNLANDED
        .iter()
        .enumerate()
        .map(|(index, name)| mcp_call(index as u64 + 2, name, json!({})))
        .collect();
    let answers = mcp(root, true, &format!("{}{script}", mcp_opening())).answers;
    for (name, answer) in UNLANDED.iter().zip(&answers[1..]) {
        let envelope = mcp_envelope(answer);
        assert_eq!(envelope["code"], json!("refused"), "{name}: {envelope}");
        let shared = write_refusal(name, writes(true)).expect("an unlanded write is refused");
        assert_eq!(
            envelope,
            &serde_json::to_value(shared).expect("a failure is JSON")
        );
    }
    // The two served names reach the tool schema, so a request with no `path`
    // is a parameter error and not a refusal.
    for name in ["update_frontmatter", "write_document"] {
        let script = format!("{}{}", mcp_opening(), mcp_call(2, name, json!({})));
        let answer = mcp(root, true, &script).answers[1].clone();
        assert_eq!(answer["error"]["code"], json!(-32602), "{name}: {answer}");
        assert!(write_refusal(name, writes(true)).is_none(), "{name}");
    }
    assert!(write_refusal("search", writes(true)).is_none());
}

/// The handshake and one call per write name.
fn write_script() -> String {
    let mut script = mcp_opening();
    for (index, name) in WRITE_NAMES.iter().enumerate() {
        script.push_str(&mcp_call(index as u64 + 2, name, json!({})));
    }
    script
}

// ───────────────────────────── stdout discipline ───────────────────────────

#[test]
fn case_10_2_a_whole_session_parses_as_json_rpc() {
    // A config warning, no index yet, and one edit: three diagnostics for stderr
    // across the first build, and not one byte of them on stdout.
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
        mcp_call(3, "janus_status", json!({})),
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
