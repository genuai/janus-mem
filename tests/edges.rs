//! Supersession edges end to end (M5 §3.2, §3.3).
//!
//! These are the cases only a build can prove: that a link resolves from the
//! generation's own tables, that a move and a deletion leave a chain intact,
//! and that a declared valid-time end reaches `index.bin`.
//!
//! The units of resolution live in `src/link.rs`. The traversal cases — the
//! cycle break, the scope rule and the receipt's four counts — live in
//! `tests/graph.rs`. Every vault is a temp copy. The committed fixtures are
//! never written to, and no test reads the wall clock.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::{FIXTURE_DOCUMENTS, fixture_vault, graph_vault, json_ok, run_ok};
use janus_mem::id::DocId;
use janus_mem::index::Snapshot;
use janus_mem::index::store::INDEX_FILE;
use serde_json::Value;

/// Builds the index without a payload: these cases read metadata only.
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

/// The resolved edges as `(from path, to path)` pairs, sorted.
fn edge_paths(snapshot: &Snapshot) -> Vec<(String, String)> {
    let names = paths(snapshot);
    let mut out: Vec<(String, String)> = snapshot
        .edges
        .iter()
        .map(|edge| {
            (
                names.get(&edge.from).cloned().unwrap_or_default(),
                names.get(&edge.to).cloned().unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn case_2_1_the_main_fixture_resolves_three_edges_and_one_dangling() {
    // The main fixture declares four links. Three close its 3-document cycle:
    //   (redis, postgres), (revisited, redis), (postgres, revisited)
    // The fourth names `decisions/2025-04-01-never-written.md`, which does not
    // exist, so it is one dangling row with `NotFound`.
    let vault = fixture_vault();
    index(vault.path());

    let snapshot = snapshot(vault.path());
    assert_eq!(snapshot.edges.len(), 3, "{:?}", edge_paths(&snapshot));
    assert_eq!(snapshot.dangling_links.len(), 1);
    assert_eq!(
        snapshot.dangling_links[0].reason,
        janus_mem::index::DanglingReason::NotFound
    );
    assert_eq!(
        snapshot.dangling_links[0].target,
        "decisions/2025-04-01-never-written.md"
    );
    assert_eq!(
        edge_paths(&snapshot),
        [
            (
                "decisions/2024-11-02-postgres-cache.md".to_owned(),
                "decisions/2025-02-01-cache-revisited.md".to_owned()
            ),
            (
                "decisions/2024-12-01-redis-cache.md".to_owned(),
                "decisions/2024-11-02-postgres-cache.md".to_owned()
            ),
            (
                "decisions/2025-02-01-cache-revisited.md".to_owned(),
                "decisions/2024-12-01-redis-cache.md".to_owned()
            ),
        ]
        .into_iter()
        .collect::<Vec<_>>()
    );

    // The receipt counts the same two tables, so the numbers agree.
    let report = receipt(vault.path());
    assert_eq!(report["edges"], 3, "{report}");
    assert_eq!(report["dangling_links"], 1, "{report}");

    // The fixture still yields the same documents: P2 adds no file to it.
    assert_eq!(snapshot.docs.len(), FIXTURE_DOCUMENTS);
}

#[test]
fn case_2_2_a_link_follows_a_move_through_the_alias_map() {
    // Move `bad/one-sided-new.md` to `bad/newer.md` unchanged. `bad/one-sided.md`
    // still names the old path, so only the alias map can resolve it: the edge
    // count stays at 9 and the moved target does not become dangling.
    let vault = graph_vault();
    index(vault.path());
    assert_eq!(snapshot(vault.path()).edges.len(), 9);

    let old = vault.path().join("bad/one-sided-new.md");
    let new = vault.path().join("bad/newer.md");
    fs::rename(&old, &new).expect("move the linked document");
    index(vault.path());

    let after = snapshot(vault.path());
    // The only dangling row is `bad/dangling.md`'s, not the moved target's.
    assert_eq!(after.dangling_links.len(), 1);
    assert_eq!(after.dangling_links[0].target, "bad/never-written.md");
    assert_eq!(after.edges.len(), 9, "{:?}", edge_paths(&after));
    assert!(
        edge_paths(&after)
            .iter()
            .any(|(from, to)| from == "bad/one-sided.md" && to == "bad/newer.md"),
        "{:?}",
        edge_paths(&after)
    );
}

#[test]
fn case_2_6_a_link_to_a_tombstoned_id_resolves() {
    // Delete the target of two links. Its path stays in `path_history`, so both
    // edges survive and the target has no `DocMeta` row of its own.
    let vault = graph_vault();
    index(vault.path());

    fs::remove_file(vault.path().join("bad/one-sided-new.md")).expect("remove the target");
    index(vault.path());

    let snapshot = snapshot(vault.path());
    assert_eq!(snapshot.edges.len(), 9, "{:?}", edge_paths(&snapshot));
    let retired = snapshot
        .tombstones
        .iter()
        .map(|meta| meta.id)
        .collect::<Vec<_>>();
    assert_eq!(retired.len(), 1, "one document left the corpus");
    let id = retired[0];
    assert!(
        snapshot.docs.iter().all(|doc| doc.id != id),
        "a retired document has no metadata row"
    );
    // Both declarations still name it as the replaced document's successor.
    let names = paths(&snapshot);
    let mut successors = snapshot
        .edges
        .iter()
        .filter(|edge| edge.to == id)
        .filter_map(|edge| names.get(&edge.from).cloned())
        .collect::<Vec<_>>();
    successors.sort();
    assert_eq!(
        successors,
        ["bad/one-sided.md", "bad/status-lies.md"],
        "{:?}",
        edge_paths(&snapshot)
    );
}

#[test]
fn effective_to_survives_a_build() {
    // `DocMeta.effective_to` is what the window rule reads in M5 phase 3. The
    // graph fixture declares it twice, and both values must come back from
    // `index.bin`.
    let vault = graph_vault();
    index(vault.path());
    let snapshot = snapshot(vault.path());
    let names = paths(&snapshot);

    let mut seen = BTreeMap::new();
    for doc in &snapshot.docs {
        let name = names.get(&doc.id).cloned().unwrap_or_default();
        if name == "bad/expired.md" || name == "bad/invalid-window.md" {
            seen.insert(name, doc.effective_to);
        }
    }
    assert_eq!(
        seen,
        [
            (
                "bad/expired.md".to_owned(),
                chrono::NaiveDate::from_ymd_opt(2020, 1, 1)
            ),
            (
                "bad/invalid-window.md".to_owned(),
                chrono::NaiveDate::from_ymd_opt(2020, 1, 1)
            ),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>()
    );
}

#[test]
fn a_repeated_build_publishes_the_same_edges() {
    // Determinism: the second build reuses the first generation and must
    // publish byte-identical edges and dangling rows.
    let vault = graph_vault();
    index(vault.path());
    let first = snapshot(vault.path());
    index(vault.path());
    let second = snapshot(vault.path());

    assert_eq!(first.edges, second.edges);
    assert_eq!(first.dangling_links, second.dangling_links);
}
