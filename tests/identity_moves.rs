//! Identity survives a build, a move, an edit and a refresh (M5 §3.1).
//!
//! These are the end-to-end cases: a real temp vault, the real binary, and the
//! `id` each command prints. The units of the ladder live in
//! `src/vault/identity.rs`; what can only be proved here is that a build
//! publishes the tables, that the next build reads them, and that every reader
//! agrees on the answer.
//!
//! Every vault is built fresh in a temp directory. Nothing writes into the
//! committed fixture and no test reads the wall clock.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;

/// A `janus` command ready to configure.
fn janus() -> Command {
    Command::cargo_bin("janus").expect("the janus binary must build for tests")
}

/// Writes a config and one Markdown file per entry into a temp vault.
fn vault(docs: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"identity\"\n",
    )
    .expect("write config");
    for (rel_path, text) in docs {
        write(dir.path(), rel_path, text);
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

/// Runs `janus --vault <root> <args>` and returns its output.
fn run(root: &Path, args: &[&str]) -> std::process::Output {
    janus()
        .arg("--vault")
        .arg(root)
        .args(args)
        .output()
        .expect("run janus")
}

/// Runs a command that must succeed and returns its parsed `--json` stdout.
fn json(root: &Path, args: &[&str]) -> Value {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// `janus index --no-embeddings`, which is the only writer in this file.
fn index(root: &Path, extra: &[&str]) {
    let mut args = vec!["index", "--no-embeddings"];
    args.extend_from_slice(extra);
    let output = run(root, &args);
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The id each live document has, keyed by path, from `janus ids --json`.
fn ids(root: &Path) -> BTreeMap<String, String> {
    let report = json(root, &["ids", "--json"]);
    let mut map = BTreeMap::new();
    for row in report["rows"].as_array().expect("rows") {
        assert_eq!(row["retired"], false, "every document is live: {row}");
        map.insert(
            row["path"].as_str().expect("path").to_owned(),
            row["id"].as_str().expect("id").to_owned(),
        );
    }
    map
}

/// The id each live document has, keyed by path, from `janus scan --json`.
fn scan_ids(root: &Path) -> BTreeMap<String, String> {
    let report = json(root, &["scan", "--json"]);
    let mut map = BTreeMap::new();
    for doc in report["docs"].as_array().expect("docs") {
        map.insert(
            doc["rel_path"].as_str().expect("rel_path").to_owned(),
            doc["id"].as_str().expect("id").to_owned(),
        );
    }
    map
}

/// The id `search` reports for one document, or `None` when it is not returned.
fn search_id(root: &Path, query: &str, rel_path: &str) -> Option<String> {
    let report = json(root, &["search", query, "--json", "--no-embeddings"]);
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|hit| hit["rel_path"] == rel_path)
        .map(|hit| hit["id"].as_str().expect("id").to_owned())
}

const DECISION: &str = "---\ntitle: Cache revisited\ndate: 2025-02-01\n---\nthe cache is redis\n";
const NOTE: &str = "---\ntitle: Flow mapping\ndate: 2025-03-01\n---\nthe flow crosses the cache\n";

// ───────────────────────────── in-place edits ─────────────────────────────

#[test]
fn an_in_place_title_edit_keeps_the_id_and_every_reader_agrees() {
    // Case 1.2 and 1.10. The tier is the one that produced the id, so it stays
    // `title_date` even though the title it names has changed.
    let vault = vault(&[("decisions/cache.md", DECISION)]);
    let root = vault.path();
    index(root, &[]);
    let before = ids(root);

    write(
        root,
        "decisions/cache.md",
        "---\ntitle: Cache comes back\ndate: 2025-02-01\n---\nthe cache is redis\n",
    );
    index(root, &[]);

    let after = ids(root);
    assert_eq!(after, before, "the path did not move, so the id did not");
    let row = json(root, &["ids", "--json", "decisions/cache.md"]);
    assert_eq!(row["rows"][0]["tier"], "title_date", "{row}");
    assert!(
        row["rows"][0]["keys"]
            .as_array()
            .expect("keys")
            .iter()
            .any(|key| key.as_str().expect("key").contains("\"Cache comes back\"")),
        "the stored key names the current title: {row}"
    );
    // `scan` and `search` read the same generation, so both agree with `ids`.
    assert_eq!(scan_ids(root), after);
    assert_eq!(
        search_id(root, "redis", "decisions/cache.md").as_deref(),
        after.get("decisions/cache.md").map(String::as_str)
    );
}

// ─────────────────────────────── moves ────────────────────────────────────

#[test]
fn a_rebuild_keeps_the_id_after_an_in_place_edit() {
    // Case 1.8 and M5-2: `--rebuild` drops the reuse source and never the
    // identity basis. Without the basis, a rebuild re-identifies and this id
    // churns.
    let vault = vault(&[("decisions/cache.md", DECISION)]);
    let root = vault.path();
    index(root, &[]);
    let before = ids(root);

    write(
        root,
        "decisions/cache.md",
        "---\ntitle: Cache comes back\ndate: 2025-02-01\n---\nthe cache is redis\n",
    );
    index(root, &["--rebuild"]);

    assert_eq!(
        ids(root),
        before,
        "a rebuild re-tokenizes, never re-identifies"
    );
}

#[test]
fn a_rebuild_after_a_move_reports_the_content_hash_rung() {
    // Case 1.3: a byte-identical move is proved by rung 2, which is a match key
    // and could not fire before a basis existed. The tier is the rung that
    // matched, so this build reports `content_hash` even though the first build
    // derived the id from the title and the date.
    let vault = vault(&[("notes/flow.md", NOTE), ("decisions/cache.md", DECISION)]);
    let root = vault.path();
    index(root, &[]);
    let before = ids(root);
    assert_eq!(
        json(root, &["ids", "--json", "notes/flow.md"])["rows"][0]["tier"],
        "title_date",
        "the first build derived the id from metadata"
    );

    let moved = fs::read_to_string(root.join("notes/flow.md")).expect("read");
    fs::remove_file(root.join("notes/flow.md")).expect("remove");
    write(root, "notes/flow-v2.md", &moved);
    index(root, &["--rebuild"]);

    let after = ids(root);
    assert_eq!(
        after.get("notes/flow-v2.md"),
        before.get("notes/flow.md"),
        "the moved document kept its id: {before:?} -> {after:?}"
    );
    assert_eq!(
        after.get("decisions/cache.md"),
        before.get("decisions/cache.md")
    );
    // The old path is remembered, and it is not a tombstone: the document lives.
    let report = json(root, &["ids", "--json", "notes/flow-v2.md"]);
    assert_eq!(
        report["rows"][0]["previous_path"], "notes/flow.md",
        "{report}"
    );
    assert_eq!(report["rows"][0]["inherited"], true, "{report}");
    assert_eq!(
        report["rows"][0]["tier"], "content_hash",
        "rung 2 proves the move: {report}"
    );
}

#[test]
fn a_refresh_keeps_the_tables_so_a_later_build_still_knows_the_move() {
    // The failure this guards against: a snapshot built from the current scan
    // alone erases the history, so the *next* build re-derives every id with all
    // tests still passing. `search` refreshes the corpus, and the refresh must
    // publish the tables it read.
    let vault = vault(&[("notes/flow.md", NOTE), ("decisions/cache.md", DECISION)]);
    let root = vault.path();
    index(root, &[]);
    let before = ids(root);

    let moved = fs::read_to_string(root.join("notes/flow.md")).expect("read");
    fs::remove_file(root.join("notes/flow.md")).expect("remove");
    write(root, "notes/flow-v3.md", &moved);
    // A read refreshes: that build is the one under test.
    assert_eq!(
        search_id(root, "cache", "decisions/cache.md").as_deref(),
        before.get("decisions/cache.md").map(String::as_str)
    );
    let after_refresh = ids(root);
    assert_eq!(
        after_refresh.get("notes/flow-v3.md"),
        before.get("notes/flow.md"),
        "the refresh kept the id"
    );

    // A second build reads the refresh's own tables: one more move, and the id
    // still follows, which is only true if the history survived the refresh.
    let moved = fs::read_to_string(root.join("notes/flow-v3.md")).expect("read");
    fs::remove_file(root.join("notes/flow-v3.md")).expect("remove");
    write(root, "notes/flow-v4.md", &moved);
    index(root, &[]);
    let twice = ids(root);
    assert_eq!(
        twice.get("notes/flow-v4.md"),
        before.get("notes/flow.md"),
        "the id survived two moves across three builds"
    );
    let report = json(root, &["ids", "--json", "notes/flow-v4.md"]);
    assert_eq!(report["rows"][0]["previous_path"], "notes/flow-v3.md");
}

// ────────────────────────────── tombstones ────────────────────────────────

#[test]
fn a_tombstoned_path_reports_retired_with_its_last_title() {
    // Case 1.12: the path is gone, and the report says so instead of failing.
    let vault = vault(&[("notes/flow.md", NOTE), ("decisions/cache.md", DECISION)]);
    let root = vault.path();
    index(root, &[]);

    fs::remove_file(root.join("notes/flow.md")).expect("remove");
    index(root, &[]);

    let report = json(root, &["ids", "--json", "notes/flow.md"]);
    let row = &report["rows"][0];
    assert_eq!(row["retired"], true, "{report}");
    assert_eq!(row["title"], "Flow mapping", "{report}");
    assert_eq!(row["path"], "notes/flow.md", "{report}");
    assert!(row["id"].as_str().expect("id").len() == 32, "{report}");
    // A live document never appears with `retired`.
    assert_eq!(
        json(root, &["ids", "--json"]).get("rows").map(|r| {
            r.as_array()
                .expect("rows")
                .iter()
                .all(|row| row["retired"] == false)
        }),
        Some(true)
    );
}

#[test]
fn a_path_that_is_neither_live_nor_retired_is_a_named_error() {
    let vault = vault(&[("notes/flow.md", NOTE)]);
    let root = vault.path();
    index(root, &[]);

    let output = run(root, &["ids", "notes/nowhere.md"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("notes/nowhere.md"),
        "{output:?}"
    );
}

// ──────────────────────────── basis absence ───────────────────────────────

#[test]
fn a_corrupt_index_re_derives_ids_with_one_warning_and_still_builds() {
    // Case 1.9. A cache must not fail a build, and the user must learn why the
    // ids were re-derived.
    let vault = vault(&[("notes/flow.md", NOTE)]);
    let root = vault.path();
    index(root, &[]);
    let before = ids(root);
    fs::write(root.join(".janus/index.bin"), b"not a snapshot").expect("corrupt the index");

    let output = run(root, &["index", "--no-embeddings"]);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("identity: no previous generation"))
        .collect();
    assert_eq!(lines.len(), 1, "one warning: {stderr:?}");

    // An unmoved document derives the same id from the same metadata.
    assert_eq!(ids(root), before);
}

// ────────────────────────────── determinism ───────────────────────────────

#[test]
fn two_runs_of_ids_are_byte_identical_and_sorted_by_path() {
    // Case 1.11.
    let vault = vault(&[
        ("notes/flow.md", NOTE),
        ("decisions/cache.md", DECISION),
        ("a.md", "---\ntitle: A\n---\nalpha\n"),
    ]);
    let root = vault.path();
    index(root, &[]);

    let first = run(root, &["ids", "--json"]);
    let second = run(root, &["ids", "--json"]);
    assert!(first.status.success(), "{first:?}");
    assert_eq!(first.stdout, second.stdout, "two runs, one report");

    let report: Value = serde_json::from_slice(&first.stdout).expect("JSON");
    let paths: Vec<&str> = report["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["path"].as_str().expect("path"))
        .collect();
    let mut sorted = paths.clone();
    sorted.sort_unstable();
    assert_eq!(paths, sorted, "sorted by path");
    assert_eq!(paths.len(), 3);
    // Every id is inherited from the first build, and none names a previous path.
    for row in report["rows"].as_array().expect("rows") {
        assert_eq!(row["inherited"], true, "{row}");
        assert!(row["previous_path"].is_null(), "{row}");
    }
}
