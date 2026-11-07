//! The write gate of M6 §3.8: the `--allow-write` statement and the pinned
//! refusal (cases 8.3 and 8.4).
//!
//! Every test drives the binary over stdio, so each assertion is made on bytes a
//! real client reads. No test sleeps, reads the wall clock or opens a port, and
//! `RUST_LOG` is off, so the only stderr lines are the ones a test puts there.

mod common;

use std::fs;

use common::{graph_vault, mcp, mcp_call, mcp_envelope, mcp_opening};
use serde_json::json;

/// The four names of SPEC §9.3, which this release refuses.
const WRITE_NAMES: [&str; 4] = [
    "update_frontmatter",
    "write_document",
    "supersede_document",
    "promote_document",
];

/// The five names of SPEC §9.2, and the whole of `tools/list`.
const READ_NAMES: [&str; 5] = [
    "search",
    "get_lineage",
    "get_authoritative_facts",
    "read_document",
    "janus_status",
];

/// The one line `--allow-write` prints, and the only thing it changes.
const STATEMENT: &str = "writes are not implemented yet; --allow-write has no effect\n";

#[test]
fn case_8_3_allow_write_prints_one_line_and_leaves_stdout_empty() {
    let dir = graph_vault();
    let root = dir.path();

    // No flag: the server starts, says nothing, and answers nothing, because the
    // script is empty.
    let plain = mcp(root, false, "");
    assert_eq!(plain.code, Some(0), "stderr:\n{}", plain.stderr);
    assert_eq!(plain.stdout, "", "stdout must stay empty");
    assert_eq!(plain.stderr, "", "no flag earns no statement");

    // The flag: one line on stderr, and not one byte on stdout.
    let flagged = mcp(root, true, "");
    assert_eq!(flagged.code, Some(0), "stderr:\n{}", flagged.stderr);
    assert_eq!(flagged.stdout, "", "stdout is the protocol stream");
    assert_eq!(
        flagged.stderr, STATEMENT,
        "exactly one line, exactly these words"
    );

    // The flag changes no answer: the same five tools, the same search.
    let script = format!(
        "{}{}\n{}",
        mcp_opening(),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        mcp_call(
            3,
            "search",
            json!({ "query": "cache", "intent": "current" })
        )
    );
    let session = mcp(root, true, &script);
    assert_eq!(session.answers.len(), 3, "{}", session.stdout);
    let names: Vec<&str> = session.answers[1]["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(names, READ_NAMES);
    let envelope = mcp_envelope(&session.answers[2]);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
}

#[test]
fn case_8_3_writes_enabled_is_false_in_both_modes_whatever_the_config_says() {
    let dir = graph_vault();
    let root = dir.path();
    let config = fs::read_to_string(root.join("janus.toml")).expect("the config");
    fs::write(
        root.join("janus.toml"),
        format!("{config}\n[mcp]\nallow_write = true\n"),
    )
    .expect("enable writes in the config");

    let script = format!(
        "{}{}",
        mcp_opening(),
        mcp_call(2, "janus_status", json!({}))
    );
    for allow_write in [false, true] {
        let answers = mcp(root, allow_write, &script).answers;
        let envelope = mcp_envelope(&answers[1]);
        assert_eq!(envelope["ok"], json!(true), "{envelope}");
        assert_eq!(
            envelope["writes_enabled"],
            json!(false),
            "the config cannot grant a write this release does not have: {envelope}"
        );
    }
}

/// M7 reverses this refusal. M7 must delete this test in the same pull request
/// that serves the first write tool, because the gate it asserts is the gate M7
/// removes: a listed write tool, or an answered one, fails here.
#[test]
fn case_8_4_m7_deletes_this_test_when_it_serves_a_write() {
    let dir = graph_vault();
    let root = dir.path();
    for allow_write in [false, true] {
        let script = format!(
            "{}{}\n",
            mcp_opening(),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })
        );
        let answers = mcp(root, allow_write, &script).answers;
        let listed: Vec<&str> = answers[1]["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(listed, READ_NAMES, "allow_write = {allow_write}");

        for name in WRITE_NAMES {
            let script = format!("{}{}", mcp_opening(), mcp_call(2, name, json!({})));
            let answer = mcp(root, allow_write, &script).answers[1].clone();
            assert_eq!(answer["result"]["isError"], json!(true), "{answer}");
            let envelope = mcp_envelope(&answer);
            assert_eq!(envelope["ok"], json!(false), "{name}: {envelope}");
            assert_eq!(envelope["code"], json!("refused"), "{name}: {envelope}");
            assert_eq!(envelope["api"], json!(1), "{name}: {envelope}");
            assert!(
                envelope.get("results").is_none(),
                "a refusal carries no results: {envelope}"
            );
            let hint = envelope["hint"].as_str().expect("a hint");
            assert!(
                hint.contains("M7"),
                "{name} must name the phase that reverses it: {hint}"
            );
        }
    }
}
