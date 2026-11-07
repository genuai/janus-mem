//! Cases 1.1 and 1.2 re-asserted through the engine (M6 §3.1, §3.4).
//!
//! The CLI half of both cases is `tests/api_envelope.rs`. This file answers the
//! same questions from `Engine::call`, because the engine is the second caller
//! of one compute: an envelope or a hash that differs between the two surfaces
//! is a defect in the split, not in the report.
//!
//! Every expected path below is written by hand from the committed fixtures or
//! from `tests/snapshots/golden_search__search_no_embeddings.snap`. The one
//! computed value is a file's blake3, read here so the assertion is about the
//! bytes and not about the index.

mod common;

use std::fs;
use std::path::Path;

use common::{engine, engine_paths, fixture_vault};
use janus_mem::api::{Call, Tool};
use janus_mem::engine::Response;
use serde_json::{Value, json};

/// The sorted keys of one object as one line, so a test can state a shape.
fn shape(value: &Value) -> String {
    value
        .as_object()
        .expect("keys")
        .keys()
        .map(String::as_str)
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Asserts that one row's `content_hash` is the blake3 of the file it names.
fn assert_hash_of_file(root: &Path, row: &Value) {
    let rel = row["rel_path"].as_str().expect("rel_path");
    let bytes = fs::read(root.join(rel)).expect("read the fixture file");
    let expected = blake3::hash(&bytes).to_hex().to_string();
    assert_eq!(row["content_hash"], json!(expected), "{row}");
}

fn search(params: Value) -> Call {
    Call::new(Tool::Search, params)
}

fn paths(response: &Response) -> Vec<String> {
    assert!(response.ok, "the call failed: {}", response.json);
    engine_paths(response)
}

#[test]
fn case_1_1_search_through_the_engine_keeps_the_envelope_and_one_hash_per_row() {
    let vault = fixture_vault();
    let engine = engine(vault.path());
    let response = engine.call(&search(json!({
        "query": "cache",
        "intent": "history",
        "limit": 3
    })));

    assert_eq!(response.json["api"], json!(1), "{}", response.json);
    assert_eq!(response.json["ok"], json!(true), "{}", response.json);
    assert_eq!(response.code, None);
    // The envelope adds `ok` and nothing else: these are the report's own keys.
    assert_eq!(
        shape(&response.json),
        "alpha api corpus_id degraded index intent limit notes ok query results taken_ms vault"
    );
    // The three ranked paths are the committed golden's, for the same query.
    assert_eq!(
        paths(&response).join(" "),
        "decisions/2024-12-01-redis-cache.md decisions/2025-02-01-cache-revisited.md \
         decisions/2024-11-02-postgres-cache.md"
    );
    for row in response.json["results"].as_array().expect("results") {
        assert_hash_of_file(vault.path(), row);
    }
}

#[test]
fn case_1_2_facts_through_the_engine_keeps_the_envelope_and_one_hash_per_row() {
    let vault = fixture_vault();
    let engine = engine(vault.path());
    let response = engine.call(&Call::new(Tool::AuthoritativeFacts, json!({})));

    assert_eq!(response.json["api"], json!(1), "{}", response.json);
    assert_eq!(response.json["ok"], json!(true), "{}", response.json);
    assert_eq!(response.code, None);
    assert_eq!(
        shape(&response.json),
        "alpha api corpus_id degraded index limit notes ok results taken_ms vault"
    );
    // Authority order, then date: one decision and two specs, and the newest
    // spec does not outrank the decision.
    assert_eq!(
        paths(&response).join(" "),
        "decisions/2025-03-01-broken-link.md specs/body-starts-with-rule.md specs/crlf-spec.md"
    );
    for row in response.json["results"].as_array().expect("results") {
        assert_hash_of_file(vault.path(), row);
    }
}

#[test]
fn case_1_2_lineage_through_the_engine_keeps_the_envelope_and_one_hash_per_node() {
    let vault = fixture_vault();
    let engine = engine(vault.path());
    let response = engine.call(&Call::new(
        Tool::Lineage,
        json!({"path": "decisions/2024-12-01-redis-cache.md"}),
    ));

    assert_eq!(response.json["api"], json!(1), "{}", response.json);
    assert_eq!(response.json["ok"], json!(true), "{}", response.json);
    assert_eq!(
        shape(&response.json),
        "api corpus_id cycle_breaks notes ok path results taken_ms terminal vault"
    );
    assert_eq!(
        response.json["path"],
        json!("decisions/2024-12-01-redis-cache.md"),
        "{}",
        response.json
    );
    let nodes = response.json["results"].as_array().expect("results");
    assert!(!nodes.is_empty(), "a chain has at least one node");
    for node in nodes {
        // A lineage node names its file by corpus path, not by `rel_path`.
        let rel = node["path"].as_str().expect("path");
        let bytes = fs::read(vault.path().join(rel)).expect("read the node's file");
        assert_eq!(
            node["content_hash"],
            json!(blake3::hash(&bytes).to_hex().to_string()),
            "{node}"
        );
    }
}
