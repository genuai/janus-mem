//! The dream cycle: three reorganisations of one vault, and the chain survives
//! every one (M5 §5 P4, DoD rows 4–8).
//!
//! One temp vault, three steps:
//!
//! 1. a linked document moves, byte for byte;
//! 2. a linked document with no `id` anywhere moves **and** its body changes;
//! 3. a third document's body is rewritten in place.
//!
//! After every step the chain still renders with the same ids, and every vault
//! file outside `.janus/` is byte identical with an identical mtime. The
//! comparison straddles the command run: the step's own move or edit is this
//! test's write, and the claim is that the **program** writes nothing. `.janus/`
//! is excluded on purpose: it is a rebuildable cache, and a read may refresh it.
//!
//! The commands run with no stdin, so a confirmation prompt would fail the test
//! instead of waiting for an answer.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use common::{json_ok, run_ok};
use janus_mem::id::Blake3Hash;
use serde_json::Value;

const A: &str = "decisions/2020-01-01-adopt-postgres.md";
const B: &str = "decisions/2021-01-01-move-to-redis.md";
const C: &str = "decisions/2022-01-01-cache-returns.md";

/// Step 1's path: the same bytes at a new name, so only the path moved.
const A_MOVED: &str = "decisions/adopt-postgres.md";
/// Step 2's path: the document moves and its body changes.
const B_MOVED: &str = "decisions/move-to-redis.md";

const A_TEXT: &str = "---\ntitle: Adopt Postgres\ntype: decision\nstatus: superseded\ndate: 2020-01-01\n---\n# Adopt Postgres\n\nPostgres holds the data and the cache.\n";
const B_TEXT: &str = "---\ntitle: Move to Redis\ntype: decision\nstatus: superseded\ndate: 2021-01-01\nsupersedes: decisions/2020-01-01-adopt-postgres.md\n---\n# Move to Redis\n\nThe cache moves out of Postgres.\n";
const C_TEXT: &str = "---\ntitle: Cache returns\ntype: decision\nstatus: active\ndate: 2022-01-01\nsupersedes: decisions/2021-01-01-move-to-redis.md\n---\n# Cache returns\n\nA smaller cache, with a measured budget.\n";
/// An unrelated document, so "every file in the vault" means more than the chain.
const NOTE: &str = "---\ntitle: Flow mapping\ndate: 2022-03-01\n---\n# Flow mapping\n\nThe flow crosses the cache.\n";

/// A three-link chain plus one unrelated document. No document declares an
/// `id`, so a move is proved by the bytes and then by the title and the date,
/// never by a claim the test wrote into the file.
fn vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"dream\"\n",
    )
    .expect("write config");
    for (rel_path, text) in [
        (A, A_TEXT),
        (B, B_TEXT),
        (C, C_TEXT),
        ("notes/flow.md", NOTE),
    ] {
        write(root, rel_path, text);
    }
    dir
}

/// Writes one document, creating its directories.
fn write(root: &Path, rel_path: &str, text: &str) {
    let path = root.join(rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("directory");
    }
    fs::write(path, text).expect("write document");
}

/// Every vault file outside `.janus/`: path, content hash and mtime.
fn snapshot(root: &Path) -> BTreeMap<String, (String, SystemTime)> {
    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    assert!(
        files.len() >= 5,
        "the snapshot must cover the vault: {files:?}"
    );
    files
}

fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<String, (String, SystemTime)>) {
    for entry in fs::read_dir(dir).expect("read a vault directory") {
        let entry = entry.expect("directory entry");
        if entry.file_name() == ".janus" {
            continue;
        }
        let path = entry.path();
        if entry.file_type().expect("file type").is_dir() {
            collect(root, &path, out);
            continue;
        }
        let bytes = fs::read(&path).expect("read a vault file");
        let rel_path = path
            .strip_prefix(root)
            .expect("every entry is under the root")
            .to_string_lossy()
            .replace('\\', "/");
        let modified = entry
            .metadata()
            .expect("file metadata")
            .modified()
            .expect("mtime");
        out.insert(rel_path, (Blake3Hash::of_bytes(&bytes).to_hex(), modified));
    }
}

/// The chain rendered from one path: ids, depths and paths per node.
fn chain(root: &Path, path: &str) -> Vec<(String, u64, String)> {
    let report = json_ok(root, &["lineage", path, "--json", "--no-embeddings"]);
    nodes(&report)
}

/// The same projection from an already-parsed report.
fn nodes(report: &Value) -> Vec<(String, u64, String)> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|node| {
            (
                node["id"].as_str().expect("id").to_owned(),
                node["depth"].as_u64().expect("depth"),
                node["path"].as_str().expect("path").to_owned(),
            )
        })
        .collect()
}

/// The ids alone, which is what "the same chain" means across a move.
fn ids(root: &Path, path: &str) -> Vec<String> {
    chain(root, path).into_iter().map(|node| node.0).collect()
}

/// Runs one step's reads and proves they wrote no vault file.
///
/// The chain is rendered from the root, from the middle link and from the leaf,
/// so a break anywhere in it fails the step rather than only the end a single
/// query happens to see.
fn step(root: &Path, root_path: &str, middle: &str, leaf: &str) -> Vec<Vec<(String, u64, String)>> {
    let before = snapshot(root);
    let rendered = vec![
        chain(root, root_path),
        chain(root, middle),
        chain(root, leaf),
    ];
    let after = snapshot(root);
    assert_eq!(
        after, before,
        "a lineage read changed a vault file: the command must never write"
    );
    rendered
}

#[test]
fn the_chain_survives_a_dream_cycle() {
    let vault = vault();
    let root = vault.path();
    run_ok(root, &["index", "--no-embeddings"]);

    // The chain starts as A -> B -> C: B declares `supersedes` for A and C for
    // B. `ids` reads the generation the build just published, so this is the
    // baseline every step must reproduce.
    let baseline = ids(root, C);
    assert_eq!(baseline.len(), 3, "three links, root first");

    // ── Step 1: move a linked document, byte for byte ─────────────────────
    // Nothing but the path changed, so the content-hash rung proves the move
    // and `id` is unchanged. B's `supersedes` still names the path A left, and
    // the alias map answers with A's id, so the chain is unbroken.
    let text = fs::read_to_string(root.join(A)).expect("read the first link");
    fs::remove_file(root.join(A)).expect("remove the old path");
    write(root, A_MOVED, &text);
    let rendered = step(root, A_MOVED, B, C);
    for (index, chain) in rendered.iter().enumerate() {
        let ids: Vec<&String> = chain.iter().map(|node| &node.0).collect();
        assert_eq!(
            ids,
            baseline.iter().collect::<Vec<_>>(),
            "step 1, chain {index}: the ids survived the move"
        );
        assert_eq!(
            chain.iter().map(|node| node.1).collect::<Vec<_>>(),
            [0, 1, 2],
            "step 1, chain {index}: root first, one depth per link"
        );
    }
    let leaf_report = json_ok(root, &["lineage", C, "--json", "--no-embeddings"]);
    assert_eq!(leaf_report["terminal"]["state"], "active", "{leaf_report}");

    // ── Step 2: move and edit a document that declares no `id` ────────────
    // The bytes differ, so the content-hash rung cannot match. The title and
    // the date are unchanged, so rung 3 carries the id across both the move and
    // the edit. C's `supersedes` still names the path B left.
    let text = fs::read_to_string(root.join(B)).expect("read the second link");
    let edited = text.replace(
        "The cache moves out of Postgres.",
        "The cache moves to Redis, and the move costs nothing.",
    );
    assert_ne!(
        edited, text,
        "the step must edit the body to prove anything"
    );
    fs::remove_file(root.join(B)).expect("remove the old path");
    write(root, B_MOVED, &edited);
    let rendered = step(root, A_MOVED, B_MOVED, C);
    for (index, chain) in rendered.iter().enumerate() {
        let ids: Vec<&String> = chain.iter().map(|node| &node.0).collect();
        assert_eq!(
            ids,
            baseline.iter().collect::<Vec<_>>(),
            "step 2, chain {index}: the id survived a move and an edit"
        );
    }
    // The edited link is still the middle row, at its new path.
    assert_eq!(
        rendered[0][1].2, B_MOVED,
        "the chain names the path the document has now"
    );

    // ── Step 3: rewrite a third body in place ─────────────────────────────
    // The path did not move, so rung 6 (`rel_path`) alone keeps the id, and no
    // link text has to resolve through the alias map.
    let text = fs::read_to_string(root.join(C)).expect("read the last link");
    let rewritten = text.replace(
        "A smaller cache, with a measured budget.",
        "A smaller cache, and the budget is measured every week.",
    );
    assert_ne!(
        rewritten, text,
        "the step must edit the body to prove anything"
    );
    write(root, C, &rewritten);
    let rendered = step(root, A_MOVED, B_MOVED, C);
    for (index, chain) in rendered.iter().enumerate() {
        let ids: Vec<&String> = chain.iter().map(|node| &node.0).collect();
        assert_eq!(
            ids,
            baseline.iter().collect::<Vec<_>>(),
            "step 3, chain {index}: the id survived an in-place rewrite"
        );
        assert_eq!(
            chain.iter().map(|node| node.2.clone()).collect::<Vec<_>>(),
            [A_MOVED.to_owned(), B_MOVED.to_owned(), C.to_owned()],
            "step 3, chain {index}: the three rows are the three files"
        );
    }

    let report = json_ok(root, &["lineage", C, "--json", "--no-embeddings"]);
    assert_eq!(report["terminal"]["state"], "active", "{report}");
    assert_eq!(
        report["terminal"]["paths"],
        serde_json::json!([C]),
        "{report}"
    );
    assert_eq!(report["cycle_breaks"], serde_json::json!([]), "{report}");
    assert_eq!(report["notes"], serde_json::json!([]), "{report}");
}
