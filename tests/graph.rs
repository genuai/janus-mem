//! The supersession graph end to end (M5 §3.4, §3.5).
//!
//! These are the cases a build and a read must agree on: the seeded cycle is
//! broken deterministically, the scope rules reach the counts `janus index
//! --json` reports, and a second run of the receipt is byte-identical.
//!
//! The traversal units live in `src/graph.rs`; resolution lives in
//! `src/link.rs` and its end-to-end cases in `tests/edges.rs`. What can only be
//! proved here is that the receipt and a reader build the same graph. Every
//! vault is a temp copy. The committed fixtures are never written to, and no
//! test reads the wall clock.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::{fixture_vault, graph_vault, json_ok, run, run_ok};
use janus_mem::config::Config;
use janus_mem::graph::{Endpoint, Graph};
use janus_mem::id::DocId;
use janus_mem::index::Snapshot;
use janus_mem::index::store::INDEX_FILE;
use serde_json::Value;

/// Builds the index without a payload: the graph cases read metadata only.
fn index(root: &Path) {
    run_ok(root, &["index", "--no-embeddings"]);
}

/// Runs `janus index --json` and returns the receipt.
fn receipt(root: &Path) -> Value {
    json_ok(root, &["index", "--no-embeddings", "--json"])
}

/// The published generation, decoded from `.janus/index.bin`.
fn snapshot(root: &Path) -> Snapshot {
    let bytes = fs::read(root.join(".janus").join(INDEX_FILE)).expect("read index.bin");
    Snapshot::from_bytes(&bytes).expect("decode index.bin")
}

/// `DocId` → vault-relative path, for readable assertions.
fn paths(snapshot: &Snapshot) -> BTreeMap<DocId, String> {
    let mut out = BTreeMap::new();
    for doc in &snapshot.docs {
        let path = snapshot
            .strings
            .get(doc.rel_path)
            .unwrap_or_default()
            .to_owned();
        out.insert(doc.id, path);
    }
    out
}

/// The graph a read builds for this vault, using its own config.
fn graph(root: &Path) -> (Snapshot, Graph) {
    let snapshot = snapshot(root);
    let config = Config::load(root).expect("load the vault config");
    let graph = Graph::for_snapshot(&snapshot, &config);
    (snapshot, graph)
}

/// The `rel_path`s a document's successors resolve to.
fn successor_paths(snapshot: &Snapshot, graph: &Graph, from: &str) -> Vec<String> {
    let Some((id, _)) = paths(snapshot).into_iter().find(|(_, path)| path == from) else {
        return Vec::new();
    };
    let names = paths(snapshot);
    graph
        .node(id)
        .map(|node| {
            node.successors
                .iter()
                .filter_map(|id| names.get(id).cloned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn case_4_2_the_index_reports_one_cycle_break() {
    // The main fixture's cycle has three edges and one break. The receipt is
    // built from the same `Graph` a read builds, so the counts cannot differ.
    let vault = fixture_vault();
    let report = receipt(vault.path());

    assert_eq!(report["edges"], 3, "{report}");
    assert_eq!(report["dangling_links"], 1, "{report}");
    assert_eq!(report["cycle_breaks"], 1, "{report}");
    assert_eq!(report["inert_links"], 0, "{report}");

    let (snapshot, graph) = graph(vault.path());
    assert_eq!(graph.ignored().len(), 1);
    let names = paths(&snapshot);
    assert_eq!(
        graph
            .ignored()
            .iter()
            .map(|(from, to)| (
                names.get(from).cloned().unwrap_or_default(),
                names.get(to).cloned().unwrap_or_default()
            ))
            .collect::<Vec<_>>(),
        [(
            "decisions/2025-02-01-cache-revisited.md".to_owned(),
            "decisions/2024-12-01-redis-cache.md".to_owned()
        )]
    );
}

#[test]
fn the_graph_fixture_stores_one_inert_edge_and_reports_it() {
    // Nine links resolve: four along the 5-link chain, three scope pairs and two
    // `bad/` successors. `scopes/acme-vs-beta.md` replaces an `org:beta`
    // document from `org:acme` with nothing shared, so that edge is inert.
    let vault = graph_vault();
    let report = receipt(vault.path());

    assert_eq!(report["edges"], 9, "{report}");
    assert_eq!(report["dangling_links"], 1, "{report}");
    assert_eq!(report["cycle_breaks"], 0, "{report}");
    assert_eq!(report["inert_links"], 1, "{report}");
}

#[test]
fn the_graph_fixture_keeps_the_global_and_shared_edges_effective() {
    // Case 5.2 and 5.3 at the receipt level: only the disjoint pair is inert.
    let vault = graph_vault();
    index(vault.path());
    let (snapshot, graph) = graph(vault.path());

    assert_eq!(
        successor_paths(&snapshot, &graph, "scopes/global-claim.md"),
        ["scopes/acme-claim.md"],
        "a global side makes the edge effective"
    );
    assert_eq!(
        successor_paths(&snapshot, &graph, "scopes/shared-old.md"),
        ["scopes/shared-new.md"],
        "a shared tag makes the edge effective"
    );
    assert!(
        successor_paths(&snapshot, &graph, "scopes/beta-claim.md").is_empty(),
        "a disjoint edge contributes no relation"
    );
    let names = paths(&snapshot);
    let inert: Vec<(String, String)> = graph
        .inert()
        .iter()
        .map(|(from, to)| {
            (
                names.get(from).cloned().unwrap_or_default(),
                names.get(to).cloned().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        inert,
        [(
            "scopes/beta-claim.md".to_owned(),
            "scopes/acme-vs-beta.md".to_owned()
        )]
    );
}

#[test]
fn the_chain_fixture_resolves_end_to_end() {
    // The 5-link fixture chain, as the graph stores it after cycle breaking:
    // adopt-postgres → move-to-redis → cache-in-front → drop-cache →
    // cache-returns. Nothing on this path is one-sided or inert.
    let vault = graph_vault();
    index(vault.path());
    let (snapshot, graph) = graph(vault.path());

    let chain = [
        "chain/2020-01-01-adopt-postgres.md",
        "chain/2020-07-01-move-to-redis.md",
        "chain/2021-01-01-cache-in-front.md",
        "chain/2021-07-01-drop-cache.md",
        "chain/2022-01-01-cache-returns.md",
    ];
    for pair in chain.windows(2) {
        assert_eq!(
            successor_paths(&snapshot, &graph, pair[0]),
            [pair[1]],
            "{} must be replaced by {}",
            pair[0],
            pair[1]
        );
    }
    assert!(
        successor_paths(&snapshot, &graph, chain[4]).is_empty(),
        "the newest link is terminal"
    );
}

#[test]
fn case_5_5_the_flag_is_read_from_janus_toml() {
    // The committed graph config sets no flag, so case 5.1's edge is inert.
    // Adding `[links] allow_disjoint_supersession = true` makes it effective
    // and leaves no ignored or inert edge. The key is known, so the build warns
    // about nothing.
    let vault = graph_vault();
    let config_path = vault.path().join("janus.toml");
    let config = fs::read_to_string(&config_path).expect("read the config");
    fs::write(
        &config_path,
        format!("{config}\n[links]\nallow_disjoint_supersession = true\n"),
    )
    .expect("set the flag");

    let output = run(vault.path(), &["index", "--no-embeddings", "--json"]);
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("links") && !stderr.contains("unknown"),
        "the key must be known: {stderr:?}"
    );

    let report: Value = serde_json::from_slice(&output.stdout).expect("the receipt is JSON");
    assert_eq!(report["inert_links"], 0, "{report}");
    assert_eq!(report["cycle_breaks"], 0, "{report}");

    let (snapshot, graph) = graph(vault.path());
    assert_eq!(
        successor_paths(&snapshot, &graph, "scopes/beta-claim.md"),
        ["scopes/acme-vs-beta.md"],
        "the flag makes the disjoint edge effective"
    );
    assert!(graph.inert().is_empty());
}

#[test]
fn a_retired_document_keeps_its_node_in_the_graph() {
    // Delete a linked successor. Its path stays in `path_history`, so the two
    // links still resolve; it has no metadata row, and it keeps a node so P3
    // can still close a predecessor's window (R2-2).
    let vault = graph_vault();
    index(vault.path());
    fs::remove_file(vault.path().join("bad/one-sided-new.md")).expect("remove the target");
    index(vault.path());

    let (snapshot, graph) = graph(vault.path());
    assert_eq!(snapshot.tombstones.len(), 1);
    let id = snapshot.tombstones[0].id;
    assert!(
        matches!(graph.endpoint(id), Some(Endpoint::Retired(_))),
        "a retired endpoint keeps a node"
    );
    assert_eq!(
        graph.node(id).expect("retired node").predecessors.len(),
        2,
        "both one-sided documents still name it"
    );
}

#[test]
fn two_runs_of_index_json_are_identical_except_taken_ms() {
    // A warm corpus: both runs reuse every document, so only `taken_ms` may move.
    let vault = graph_vault();
    index(vault.path());

    let mut first = receipt(vault.path());
    let mut second = receipt(vault.path());
    first["taken_ms"] = Value::from(0);
    second["taken_ms"] = Value::from(0);

    assert_eq!(
        serde_json::to_string(&first).expect("encode"),
        serde_json::to_string(&second).expect("encode")
    );
}
