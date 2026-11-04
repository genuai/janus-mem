//! `janus lineage` end to end: cases 9.1–9.9 of M5 §3.9.
//!
//! Every vault is a temp copy. The committed fixtures are never written to, and
//! no test reads the wall clock.
//!
//! The expected chain order and every terminal state below are derived by hand
//! from the fixture files, in the comment that asserts them: the graph owns the
//! relations, so a test that re-derived the answer from the code would prove
//! nothing.

mod common;

use std::fs;
use std::path::Path;

use common::{fixture_vault, graph_vault, json_ok, run, run_ok, search_json};
use serde_json::Value;

/// Runs `lineage` with `--json --no-embeddings` and returns the report.
fn lineage(root: &Path, args: &[&str]) -> Value {
    let mut all = vec!["lineage"];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--json", "--no-embeddings"]);
    json_ok(root, &all)
}

/// Every node's path, in report order.
fn paths(report: &Value) -> Vec<String> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|node| node["path"].as_str().expect("path").to_owned())
        .collect()
}

/// Every node's id, in report order.
fn ids(report: &Value) -> Vec<String> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|node| node["id"].as_str().expect("id").to_owned())
        .collect()
}

/// `(depth, path)` per node, in report order.
fn depth_paths(report: &Value) -> Vec<(u64, String)> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|node| {
            (
                node["depth"].as_u64().expect("depth"),
                node["path"].as_str().expect("path").to_owned(),
            )
        })
        .collect()
}

/// The path of the node whose `self` flag is true.
fn self_path(report: &Value) -> Option<&str> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|node| node["self"] == true)
        .and_then(|node| node["path"].as_str())
}

/// Writes a config and one Markdown file per entry into a temp vault.
fn vault(docs: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"lineage\"\n",
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

/// Builds the index without a payload: these cases read the graph only.
fn index(root: &Path) {
    run_ok(root, &["index", "--no-embeddings"]);
}

/// The three-link chain every temp vault in this file uses.
const A: &str = "decisions/2020-01-01-adopt-postgres.md";
const B: &str = "decisions/2021-01-01-move-to-redis.md";
const C: &str = "decisions/2022-01-01-cache-returns.md";

const A_TEXT: &str = "---\ntitle: Adopt Postgres\ntype: decision\nstatus: superseded\ndate: 2020-01-01\n---\n# Adopt Postgres\n\nPostgres holds the data and the cache.\n";
const B_TEXT: &str = "---\ntitle: Move to Redis\ntype: decision\nstatus: superseded\ndate: 2021-01-01\nsupersedes: decisions/2020-01-01-adopt-postgres.md\n---\n# Move to Redis\n\nThe cache moves out of Postgres.\n";
const C_TEXT: &str = "---\ntitle: Cache returns\ntype: decision\nstatus: active\ndate: 2022-01-01\nsupersedes: decisions/2021-01-01-move-to-redis.md\n---\n# Cache returns\n\nA smaller cache, with a measured budget.\n";

// ────────────────────────────── cases 9.1–9.4 ─────────────────────────────

#[test]
fn case_9_1_the_five_link_chain_renders_root_first() {
    // The graph fixture `chain/` holds five files. Each of links 2–5 declares
    // `supersedes` for the file before it, and link 1 declares nothing, so the
    // chain is one root and four successors. `cache-returns` is the last link,
    // so it is the `self` row at depth 4 and the only leaf: it declares
    // `status: active` and nothing replaces it, so the terminal state is
    // `active`.
    let vault = graph_vault();
    let report = lineage(vault.path(), &["chain/2022-01-01-cache-returns.md"]);

    assert_eq!(
        depth_paths(&report),
        [
            (0, "chain/2020-01-01-adopt-postgres.md".to_owned()),
            (1, "chain/2020-07-01-move-to-redis.md".to_owned()),
            (2, "chain/2021-01-01-cache-in-front.md".to_owned()),
            (3, "chain/2021-07-01-drop-cache.md".to_owned()),
            (4, "chain/2022-01-01-cache-returns.md".to_owned()),
        ]
    );
    assert_eq!(
        self_path(&report),
        Some("chain/2022-01-01-cache-returns.md")
    );
    assert_eq!(report["terminal"]["state"], "active", "{report}");
    assert_eq!(
        report["terminal"]["paths"],
        serde_json::json!(["chain/2022-01-01-cache-returns.md"]),
        "{report}"
    );
    // The first link declares `effective_from: 2020-01-01` and its window ends
    // where the second link starts, because that successor replaces it.
    let root = &report["results"][0];
    assert_eq!(root["valid_from"], "2020-01-01", "{root}");
    assert_eq!(root["valid_to"], "2020-07-01", "{root}");
    // The four replaced links are superseded by the link below them; the last
    // one keeps its declared `active`.
    assert_eq!(root["effective_status"], "superseded", "{root}");
    assert_eq!(root["successors"][0], "chain/2020-07-01-move-to-redis.md");
}

#[test]
fn case_9_2_the_root_sees_the_same_chain() {
    // Asking for the first link reports the same five nodes and the same ids,
    // with `self` at depth 0. Nothing above a root exists, so the two reports
    // differ in nothing but the `self` flag.
    let vault = graph_vault();
    let from_last = lineage(vault.path(), &["chain/2022-01-01-cache-returns.md"]);
    let from_first = lineage(vault.path(), &["chain/2020-01-01-adopt-postgres.md"]);

    assert_eq!(depth_paths(&from_first), depth_paths(&from_last));
    assert_eq!(ids(&from_first), ids(&from_last));
    assert_eq!(
        self_path(&from_first),
        Some("chain/2020-01-01-adopt-postgres.md")
    );
    assert_eq!(
        from_first["results"][0]["predecessors"],
        serde_json::json!([])
    );
}

#[test]
fn case_9_3_a_quiet_expiry_is_a_one_node_chain() {
    // `bad/quiet-expiry.md` declares `superseded` and has no successor, so the
    // chain is one root that is also the leaf, and the terminal state is
    // `quiet_expired`: the declared status survives because nothing replaces
    // the document.
    let vault = graph_vault();
    let report = lineage(vault.path(), &["bad/quiet-expiry.md"]);

    assert_eq!(paths(&report), ["bad/quiet-expiry.md"]);
    assert_eq!(report["results"][0]["depth"], 0);
    assert_eq!(report["results"][0]["declared_status"], "superseded");
    assert_eq!(report["results"][0]["effective_status"], "superseded");
    assert_eq!(report["terminal"]["state"], "quiet_expired", "{report}");
}

#[test]
fn case_9_4_the_seeded_cycle_is_broken_and_reported() {
    // The main fixture's three decisions form one cycle:
    //   postgres.supersedes = redis, redis.supersedes = revisited,
    //   revisited.supersedes = postgres.
    // The breaker drops the edge whose `(from path, to path)` sorts greatest,
    // which is `(revisited, redis)`, leaving `redis -> postgres -> revisited`
    // (graph case 4.1). All three rows are still listed, and the ignored pair
    // is one note, not an error: the command exits 0.
    let vault = fixture_vault();
    let report = lineage(vault.path(), &["decisions/2024-12-01-redis-cache.md"]);

    assert_eq!(
        depth_paths(&report),
        [
            (0, "decisions/2024-12-01-redis-cache.md".to_owned()),
            (1, "decisions/2024-11-02-postgres-cache.md".to_owned()),
            (2, "decisions/2025-02-01-cache-revisited.md".to_owned()),
        ]
    );
    assert_eq!(
        report["cycle_breaks"],
        serde_json::json!([{
            "from": "decisions/2025-02-01-cache-revisited.md",
            "to": "decisions/2024-12-01-redis-cache.md",
        }]),
        "{report}"
    );
    assert_eq!(
        report["notes"],
        serde_json::json!([
            "cycle break: (decisions/2025-02-01-cache-revisited.md, decisions/2024-12-01-redis-cache.md) ignored"
        ]),
        "one note naming the ignored pair"
    );
    // `revisited` is the leaf the breaker separates. It declares `superseded`
    // and its one successor edge was dropped, so it reports `quiet_expired`.
    assert_eq!(report["terminal"]["state"], "quiet_expired", "{report}");
}

// ────────────────────────────── cases 9.5–9.8 ─────────────────────────────

#[test]
fn case_9_5_a_path_that_names_nothing_is_exit_one() {
    let vault = graph_vault();
    let output = run(vault.path(), &["lineage", "nope.md", "--no-embeddings"]);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("nope.md"),
        "{output:?}"
    );
}

#[test]
fn case_9_6_the_topic_renders_the_top_current_result() {
    // The chain of a topic is the chain of the top `current` result, so this
    // case reads that result from `search` instead of assuming a ranking: a
    // scoring change must not fail a lineage contract. What the case pins is
    // the wiring — the same chain, and the topic echoed.
    let vault = graph_vault();
    let found = search_json(vault.path(), &["cache", "--no-embeddings"]);
    let top = found["results"][0]["rel_path"]
        .as_str()
        .expect("rel_path")
        .to_owned();

    let by_topic = lineage(vault.path(), &["--topic", "cache"]);
    let by_path = lineage(vault.path(), &[top.as_str()]);

    assert_eq!(by_topic["topic"], "cache", "{by_topic}");
    assert_eq!(self_path(&by_topic), Some(top.as_str()));
    assert_eq!(ids(&by_topic), ids(&by_path));
    assert_eq!(by_topic["terminal"], by_path["terminal"]);
}

#[test]
fn case_9_7_no_argument_is_exit_two() {
    let vault = graph_vault();
    let output = run(vault.path(), &["lineage", "--no-embeddings"]);

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("PATH") && stderr.contains("--topic"),
        "{stderr}"
    );
}

#[test]
fn case_9_8_both_arguments_are_exit_two() {
    let vault = graph_vault();
    let output = run(
        vault.path(),
        &[
            "lineage",
            "bad/quiet-expiry.md",
            "--topic",
            "cache",
            "--no-embeddings",
        ],
    );

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not both"), "{stderr}");
}

#[test]
fn case_9_6a_a_topic_with_no_current_result_is_exit_one() {
    // `zzzqqqxxx` is no token in this corpus, so every lexical score is zero
    // and the query returns nothing: there is no document whose chain could be
    // rendered.
    let vault = graph_vault();
    let output = run(
        vault.path(),
        &["lineage", "--topic", "zzzqqqxxx", "--no-embeddings"],
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("zzzqqqxxx"),
        "{output:?}"
    );
}

// ─────────────────────────────── case 9.9 ─────────────────────────────────

#[test]
fn case_9_9_a_moved_tombstoned_target_renders_retired_at_its_last_path() {
    // The chain is A -> B -> C. Each link is declared by the **predecessor**
    // (`superseded_by`), because a retired document declares nothing: after `C`
    // leaves the corpus, only `B` is left to hold the edge that names it.
    //
    // `C` moves to `decisions/cache-returns.md` and is then deleted, so the
    // tombstone keeps the last path it held (`cache-returns.md`, not the path
    // it started on). A tombstone holds only its title, date and `valid_from`,
    // so `type`, `declared_status`, `effective_status` and `valid_to` are null:
    // nothing is invented (R4-1).
    let vault = vault(&[
        (
            A,
            "---\ntitle: Adopt Postgres\ntype: decision\nstatus: superseded\ndate: 2020-01-01\nsuperseded_by: decisions/2021-01-01-move-to-redis.md\n---\n# Adopt Postgres\n\nPostgres holds the data and the cache.\n",
        ),
        (
            B,
            "---\ntitle: Move to Redis\ntype: decision\nstatus: superseded\ndate: 2021-01-01\nsuperseded_by: decisions/2022-01-01-cache-returns.md\n---\n# Move to Redis\n\nThe cache moves out of Postgres.\n",
        ),
        (C, C_TEXT),
    ]);
    let root = vault.path();
    index(root);
    let before = ids(&lineage(root, &[A]));
    assert_eq!(before.len(), 3);

    let moved_path = "decisions/cache-returns.md";
    let text = fs::read_to_string(root.join(C)).expect("read the last link");
    fs::remove_file(root.join(C)).expect("remove the old path");
    write(root, moved_path, &text);
    index(root);

    fs::remove_file(root.join(moved_path)).expect("remove the moved document");
    index(root);

    let report = lineage(root, &[A]);
    assert_eq!(ids(&report), before, "the retired node kept its id");
    assert_eq!(
        paths(&report),
        [A.to_owned(), B.to_owned(), moved_path.to_owned()],
        "the retired node holds the last path it occupied"
    );
    let retired = &report["results"][2];
    assert_eq!(retired["title"], "Cache returns", "{retired}");
    assert_eq!(retired["date"], "2022-01-01", "{retired}");
    assert_eq!(retired["valid_from"], "2022-01-01", "{retired}");
    assert!(retired["type"].is_null(), "{retired}");
    assert!(retired["declared_status"].is_null(), "{retired}");
    assert!(retired["effective_status"].is_null(), "{retired}");
    assert!(retired["valid_to"].is_null(), "{retired}");
    assert_eq!(report["terminal"]["state"], "retired", "{report}");
    assert_eq!(
        report["terminal"]["paths"],
        serde_json::json!([moved_path]),
        "{report}"
    );

    // The retired path is an argument like any other: the node it names keeps
    // its place in the chain.
    let by_retired_path = lineage(root, &[moved_path]);
    assert_eq!(ids(&by_retired_path), before);
    assert_eq!(self_path(&by_retired_path), Some(moved_path));
}

// ────────────────────────── the documented limit ──────────────────────────

#[test]
fn a_move_that_changes_title_and_date_is_a_new_document() {
    // The documented limit. Identity is matched on declared metadata, never on
    // similarity (D23). `A` moves and changes **both** its title and its date,
    // so no rung matches: not the declared id (there is none), not the content
    // hash (the bytes changed), not title+date, not the title alone. `A` is
    // therefore a new document with a new id, and the old id retires with its
    // place in the chain.
    let vault = vault(&[(A, A_TEXT), (B, B_TEXT)]);
    let root = vault.path();
    index(root);
    let before = ids(&lineage(root, &[B]));
    assert_eq!(before.len(), 2, "A -> B");

    let moved_path = "decisions/adopt-postgres-revised.md";
    let revised = "---\ntitle: Adopt Postgres, revised\ntype: decision\nstatus: superseded\ndate: 2020-06-01\n---\n# Adopt Postgres, revised\n\nPostgres holds the data and the cache.\n";
    fs::remove_file(root.join(A)).expect("remove the old path");
    write(root, moved_path, revised);
    index(root);

    // `B` still names `decisions/2020-01-01-adopt-postgres.md`, which the alias
    // map resolves to the retired id: the predecessor row is retired.
    let chain = lineage(root, &[B]);
    assert_eq!(paths(&chain), [A.to_owned(), B.to_owned()]);
    assert_eq!(ids(&chain), before, "the retired predecessor kept its id");
    let predecessor = &chain["results"][0];
    assert!(predecessor["type"].is_null(), "{predecessor}");
    assert!(predecessor["declared_status"].is_null(), "{predecessor}");
    assert!(predecessor["effective_status"].is_null(), "{predecessor}");
    assert_eq!(predecessor["title"], "Adopt Postgres", "{predecessor}");

    // The moved file is a different document, alone in its own chain.
    let new = lineage(root, &[moved_path]);
    assert_eq!(new["results"].as_array().expect("results").len(), 1);
    assert_ne!(ids(&new), [before[0].clone()], "a new document, a new id");
    assert_eq!(new["results"][0]["predecessors"], serde_json::json!([]));
    assert_eq!(new["results"][0]["successors"], serde_json::json!([]));
    assert_eq!(self_path(&new), Some(moved_path));
}

// ────────────────────────────── determinism ───────────────────────────────

#[test]
fn two_runs_render_the_same_answer() {
    // §6 determinism: everything but `taken_ms` is stable between two runs.
    let vault = graph_vault();
    let first = lineage(vault.path(), &["chain/2022-01-01-cache-returns.md"]);
    let second = lineage(vault.path(), &["chain/2022-01-01-cache-returns.md"]);

    assert_eq!(first["results"], second["results"], "two runs, one chain");
    assert_eq!(first["terminal"], second["terminal"]);
    assert_eq!(first["notes"], second["notes"]);
    assert_eq!(first["cycle_breaks"], second["cycle_breaks"]);
}

#[test]
fn an_absolute_path_inside_the_corpus_is_accepted() {
    // A reader copies the absolute path a search result prints, so the command
    // strips the root lexically rather than refusing it.
    let vault = graph_vault();
    let absolute = vault.path().join("bad/quiet-expiry.md");
    let report = lineage(vault.path(), &[absolute.to_str().expect("UTF-8 path")]);

    assert_eq!(paths(&report), ["bad/quiet-expiry.md"]);
}

// ─────────────────────────── a path held twice ────────────────────────────

const ALPHA: &str = "---\ntitle: Alpha\n---\nfirst\n";
const EPSILON: &str = "---\ntitle: Epsilon\n---\nsecond\n";

#[test]
fn case_9_10_a_reused_path_resolves_to_the_live_document() {
    // `notes/a.md` is deleted and then created again under the same path. Two
    // nodes now hold that path: the live file and the tombstone whose last path
    // it was (§2.1). The path names the file on disk, so the live document is
    // the answer, and `ids` already reads it that way.
    //
    // The two ids are derived from the titles, so which one sorts lower is an
    // accident. `Alpha` sorts lower than `Epsilon`, so an id-ordered scan that
    // returns the first match returns the **retired** document. That ordering
    // is the point of this case: revert `by_path` to a first-match scan and
    // this test fails.
    let vault = vault(&[("notes/a.md", ALPHA)]);
    let root = vault.path();
    index(root);
    let retired = ids(&lineage(root, &["notes/a.md"]));
    assert_eq!(retired.len(), 1);

    fs::remove_file(root.join("notes/a.md")).expect("remove the document");
    index(root);
    write(root, "notes/a.md", EPSILON);
    index(root);

    let report = lineage(root, &["notes/a.md"]);
    let live = ids(&report);
    assert_eq!(report["results"][0]["title"], "Epsilon", "{report}");
    assert_ne!(live, retired, "the new document has its own id");
    assert!(
        retired[0] < live[0],
        "the retired id must sort lower for this case to prove anything: {live:?} {retired:?}"
    );
    assert_eq!(report["results"][0]["path"], "notes/a.md");
    // `ids` names the same document for the same path, so the two commands
    // cannot disagree about a path held twice.
    let row = json_ok(root, &["ids", "--json", "notes/a.md"]);
    assert_eq!(row["rows"][0]["id"], report["results"][0]["id"], "{row}");
    assert_eq!(row["rows"][0]["retired"], false, "{row}");
}
