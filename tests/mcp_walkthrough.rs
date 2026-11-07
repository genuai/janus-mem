//! Cases 11.1–11.3 of the M6 plan: the scripted transcript of the milestone's
//! done-when (M6 §3.11).
//!
//! The session is one script against a temp copy of the graph fixture, so a test
//! never writes to a committed fixture. Every expected value below is read by
//! hand from the fixture files: the chain's five links and their order, and the
//! sentence the last link's body holds.

mod common;

use std::path::Path;

use common::{graph_vault, mcp, mcp_call, mcp_envelope, mcp_opening, write_document};
use serde_json::{Value, json};

/// The last link of the chain, and the only document in it still current.
const CACHE_RETURNS: &str = "chain/2022-01-01-cache-returns.md";

/// The four links the last one replaced, in chain order.
const REPLACED: [&str; 4] = [
    "chain/2020-01-01-adopt-postgres.md",
    "chain/2020-07-01-move-to-redis.md",
    "chain/2021-01-01-cache-in-front.md",
    "chain/2021-07-01-drop-cache.md",
];

/// One scripted session: the handshake, then W1 through `search`, W2 through
/// `get_lineage` and `read_document`.
fn transcript(root: &Path) -> Vec<Value> {
    let script = format!(
        "{}{}{}{}",
        mcp_opening(),
        mcp_call(
            2,
            "search",
            json!({ "query": "cache decision", "intent": "current" })
        ),
        mcp_call(3, "get_lineage", json!({ "path": CACHE_RETURNS })),
        mcp_call(4, "read_document", json!({ "path": CACHE_RETURNS })),
    );
    let session = mcp(root, false, &script);
    assert_eq!(session.code, Some(0), "stderr:\n{}", session.stderr);
    assert_eq!(session.answers.len(), 4, "{}", session.stdout);
    session.answers
}

#[test]
fn case_11_1_w1_the_current_decision_is_present_and_nothing_superseded_is() {
    let dir = graph_vault();
    let answers = transcript(dir.path());
    let envelope = mcp_envelope(&answers[1]);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    assert_eq!(envelope["intent"], json!("current"), "{envelope}");
    let results = envelope["results"].as_array().expect("results");
    assert!(!results.is_empty(), "{envelope}");

    // The set, not the rank: every row is effective-active, the current decision
    // is in it, and no replaced link is. The four other chain documents declare
    // `status: superseded`, so `current` hides all four (M5 §3.7).
    let paths: Vec<&str> = results
        .iter()
        .map(|row| {
            assert_eq!(row["effective_status"], json!("active"), "{row}");
            row["rel_path"].as_str().expect("rel_path")
        })
        .collect();
    assert!(paths.contains(&CACHE_RETURNS), "{paths:?}");
    for replaced in REPLACED {
        assert!(!paths.contains(&replaced), "{replaced} is superseded");
    }
}

#[test]
fn case_11_2_w2_the_chain_has_five_nodes_and_the_text_holds_the_budget() {
    let dir = graph_vault();
    let answers = transcript(dir.path());

    // The chain: `adopt-postgres` replaced by `move-to-redis`, replaced by
    // `cache-in-front`, replaced by `drop-cache`, replaced by `cache-returns`.
    let envelope = mcp_envelope(&answers[2]);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    let nodes = envelope["results"].as_array().expect("results");
    let chain: Vec<(u64, &str)> = nodes
        .iter()
        .map(|node| {
            (
                node["depth"].as_u64().expect("depth"),
                node["path"].as_str().expect("path"),
            )
        })
        .collect();
    assert_eq!(
        chain,
        [
            (0, REPLACED[0]),
            (1, REPLACED[1]),
            (2, REPLACED[2]),
            (3, REPLACED[3]),
            (4, CACHE_RETURNS),
        ]
    );
    assert_eq!(envelope["terminal"]["state"], json!("active"), "{envelope}");
    assert_eq!(
        envelope["terminal"]["paths"],
        json!([CACHE_RETURNS]),
        "{envelope}"
    );

    // The text path: the last link's body says what the decision measured.
    let text = mcp_envelope(&answers[3]);
    assert_eq!(text["ok"], json!(true), "{text}");
    let body = text["body"].as_str().expect("a body");
    assert!(body.contains("measured budget"), "{body}");
}

#[test]
fn case_11_3_w3_one_added_principle_is_the_answer() {
    // The committed fixtures hold no principle, so the test supplies one: an
    // empty list would prove nothing about the tag filter.
    let dir = graph_vault();
    let root = dir.path();
    write_document(
        root,
        "principles/pricing.md",
        "---\ntitle: Price on value\ntype: principle\nstatus: active\ndate: 2022-02-01\ntags: [pricing]\n---\n\nPrice on the value the reader gets, never on the cost of the work.\n",
    );
    let script = format!(
        "{}{}",
        mcp_opening(),
        mcp_call(2, "get_authoritative_facts", json!({ "tags": ["pricing"] }))
    );
    let answers = mcp(root, false, &script).answers;
    let envelope = mcp_envelope(&answers[1]);
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    let paths: Vec<&str> = envelope["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|row| row["rel_path"].as_str().expect("rel_path"))
        .collect();
    assert_eq!(paths, ["principles/pricing.md"]);
    assert_eq!(envelope["results"][0]["effective_status"], json!("active"));
    // The committed fixture is untouched by the copy the test wrote.
    assert!(
        !Path::new(common::GRAPH_FIXTURE)
            .join("principles/pricing.md")
            .exists()
    );
}
