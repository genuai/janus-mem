//! M6b phase 2 cases 2.1–2.7: the federated query (§3.2 of the M6b plan).
//!
//! Every case runs the binary against a copy of `tests/fixtures/workspace-query`,
//! with `JANUS_WORKSPACE` naming the copied workspace file and `XDG_CONFIG_HOME`
//! in the same temporary tree, so no case reads the developer's own workspace and
//! no case writes to the committed fixture.
//!
//! `--no-embeddings` is part of every invocation: the fixture's scores are then
//! a function of its text and its config alone, and no case depends on whether a
//! model happens to be installed on the machine.
//!
//! The merge itself — exactness against the full union, the tie-break, the
//! weighting arithmetic — is proved by the unit tests beside
//! `src/search/federate.rs`. These cases prove what the command does with it.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;

const WORKSPACE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/workspace-query"
);

/// The first committed two-corpus fixture: one active archived note beside a
/// superseded archived decision.
const CORPORA_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/workspace");

// ──────────────────────────────── helpers ──────────────────────────────────

/// A private copy of a committed fixture workspace.
fn copied(from: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir for the fixture copy");
    copy_tree(Path::new(from), dir.path());
    dir
}

/// A private copy of the federated-query fixture.
fn workspace() -> tempfile::TempDir {
    copied(WORKSPACE_FIXTURE)
}

/// Copies one tree, skipping any index a previous run left behind.
fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            if entry.file_name() == ".janus" {
                continue;
            }
            fs::create_dir_all(&target).expect("create the directory");
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy the file");
        }
    }
}

/// A `janus` command for one fixture copy, with the workspace named.
fn janus_in(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("janus").expect("the janus binary must build for tests");
    command
        .current_dir(dir)
        .env("RUST_LOG", "off")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("JANUS_WORKSPACE", workspace_file(dir))
        .env_remove("JANUS_VAULT")
        .env_remove("APPDATA")
        .env_remove("HOME");
    command
}

/// The workspace file inside one fixture copy.
fn workspace_file(dir: &Path) -> PathBuf {
    dir.join("workspace.toml")
}

/// Runs `janus` with the workspace in play and returns the finished process.
fn run(dir: &Path, args: &[&str]) -> Output {
    janus_in(dir).args(args).output().expect("run janus")
}

/// Runs one query that must succeed and returns its stdout.
fn run_ok(dir: &Path, args: &[&str]) -> String {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "janus {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// Runs one query that must succeed and parses its report.
fn json(dir: &Path, args: &[&str]) -> Value {
    let text = run_ok(dir, args);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("stdout is not JSON ({err}):\n{text}"))
}

/// The `rel_path` of every result, in order.
fn paths(report: &Value) -> Vec<&str> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|row| row["rel_path"].as_str().expect("rel_path"))
        .collect()
}

/// The `corpus` of every result, in order.
fn corpora_of(report: &Value) -> Vec<&str> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|row| row["corpus"].as_str().expect("every row names its corpus"))
        .collect()
}

/// The table header a human report prints, when it printed a table.
fn header(text: &str) -> String {
    text.lines()
        .find(|line| line.starts_with("score"))
        .unwrap_or_default()
        .to_owned()
}

/// The `notes` of one report, joined for a substring check.
fn notes(report: &Value) -> String {
    report["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|note| note.as_str().expect("a note"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ──────────────────────────────── the cases ────────────────────────────────

#[test]
fn case_2_1_a_default_query_reads_the_primary_and_names_what_it_excluded() {
    let dir = workspace();
    let report = json(
        dir.path(),
        &["search", "cache", "--no-embeddings", "--json"],
    );

    assert_eq!(report["corpora"], serde_json::json!(["main"]));
    assert_eq!(corpora_of(&report), vec!["main", "main"]);
    assert!(
        paths(&report).iter().all(|path| !path.starts_with("2019")),
        "the archive answered a default query: {:?}",
        paths(&report)
    );
    assert!(
        notes(&report).contains("1 archived corpus excluded"),
        "the one note must name the count: {}",
        notes(&report)
    );
}

#[test]
fn case_2_2_all_returns_both_and_each_archived_row_carries_the_weight() {
    let dir = workspace();
    let report = json(
        dir.path(),
        &[
            "search",
            "cache",
            "--no-embeddings",
            "--corpus",
            "all",
            "--json",
        ],
    );

    assert_eq!(
        report["corpora"],
        serde_json::json!(["main", "context"]),
        "workspace order, primary first"
    );
    let rows = report["results"].as_array().expect("results");
    assert_eq!(rows.len(), 4);
    assert_eq!(
        corpora_of(&report),
        vec!["main", "context", "main", "context"]
    );
    assert!(notes(&report).is_empty(), "{}", notes(&report));

    // The documented formula is final = relevance * authority * decay * corpus,
    // evaluated in that order. Every factor below is the row's own, so the
    // arithmetic is exact and no second run has to agree with this one.
    for row in rows {
        let scores = &row["scores"];
        let relevance = scores["relevance"].as_f64().expect("a relevance");
        let authority = scores["authority"].as_f64().expect("an authority");
        let decay = scores["decay"].as_f64().expect("a decay");
        let factor = scores["corpus"].as_f64().unwrap_or(1.0);
        let final_score = scores["final"].as_f64().expect("a final score");
        assert_eq!(
            final_score,
            relevance * authority * decay * factor,
            "final must be the product of its factors: {row}"
        );
        match row["corpus"].as_str() {
            Some("context") => {
                assert_eq!(factor, 0.8, "the archived weight: {row}");
                assert_eq!(scores["corpus"], serde_json::json!(0.8), "{row}");
            }
            other => {
                assert_eq!(other, Some("main"), "{row}");
                assert!(
                    scores.get("corpus").is_none(),
                    "a neutral factor of 1.0 is not printed: {row}"
                );
            }
        }
    }
}

#[test]
fn case_2_2_the_committed_two_corpus_fixture_shows_a_row_from_each_corpus() {
    // The fixture a reader copies from must demonstrate the rule on a default
    // query: one active archived note answers with the weight beside its score,
    // and the archived decision, which is superseded, stays hidden. The role
    // says when a corpus is read; the status says whether a document is true.
    let dir = copied(CORPORA_FIXTURE);
    let report = json(
        dir.path(),
        &[
            "search",
            "cache",
            "--no-embeddings",
            "--corpus",
            "all",
            "--json",
        ],
    );

    assert_eq!(report["corpora"], serde_json::json!(["main", "context"]));
    let corpora = corpora_of(&report);
    assert_eq!(corpora.first(), Some(&"main"), "{report}");
    assert!(corpora.contains(&"context"), "{report}");
    assert!(
        !paths(&report).contains(&"2019-01-01-cache-server.md"),
        "a superseded document is still hidden under `current`: {report}"
    );

    for row in report["results"].as_array().expect("results") {
        if row["corpus"] == serde_json::json!("context") {
            assert_eq!(
                row["scores"]["corpus"],
                serde_json::json!(0.8),
                "an archived row reports its weight: {row}"
            );
        }
    }
}

#[test]
fn case_2_3_a_named_archived_corpus_is_read_alone_and_keeps_its_weight() {
    let dir = workspace();
    let report = json(
        dir.path(),
        &[
            "search",
            "cache",
            "--no-embeddings",
            "--corpus",
            "context",
            "--json",
        ],
    );

    assert_eq!(report["corpora"], serde_json::json!(["context"]));
    assert_eq!(corpora_of(&report), vec!["context", "context"]);
    for row in report["results"].as_array().expect("results") {
        assert_eq!(row["scores"]["corpus"], serde_json::json!(0.8), "{row}");
    }
    assert!(
        notes(&report).is_empty(),
        "a named corpus excludes nothing: {}",
        notes(&report)
    );
}

#[test]
fn case_2_5_an_unknown_corpus_exits_two_and_lists_the_ids() {
    let dir = workspace();
    let output = run(
        dir.path(),
        &["search", "cache", "--no-embeddings", "--corpus", "nope"],
    );
    assert_eq!(output.status.code(), Some(2), "a usage error exits 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("nope"), "{stderr}");
    assert!(stderr.contains("main"), "{stderr}");
    assert!(stderr.contains("context"), "{stderr}");
}

#[test]
fn case_2_6_the_limit_is_a_total_across_corpora_not_a_limit_per_corpus() {
    let dir = workspace();
    let one = json(
        dir.path(),
        &[
            "search",
            "cache",
            "--no-embeddings",
            "--corpus",
            "all",
            "--limit",
            "1",
            "--json",
        ],
    );
    assert_eq!(one["results"].as_array().expect("results").len(), 1);

    // Four documents match, so a per-corpus reading of `--limit 3` would return
    // up to six rows.
    let three = json(
        dir.path(),
        &[
            "search",
            "cache",
            "--no-embeddings",
            "--corpus",
            "all",
            "--limit",
            "3",
            "--json",
        ],
    );
    assert_eq!(three["results"].as_array().expect("results").len(), 3);
}

#[test]
fn case_2_7_facts_keeps_the_authority_order_and_reports_the_weight() {
    // The archived principle has the higher type authority (1.50) and the lower
    // `final` (0.7266 against 0.7819): authority orders this list, and the weight
    // only scales the score beside it (§7.1).
    let dir = workspace();
    let report = json(
        dir.path(),
        &[
            "facts",
            "--query",
            "cache",
            "--no-embeddings",
            "--corpus",
            "all",
            "--json",
        ],
    );

    let rows = report["results"].as_array().expect("results");
    assert_eq!(rows.len(), 2, "{report}");
    assert_eq!(rows[0]["corpus"], serde_json::json!("context"));
    assert_eq!(rows[0]["scores"]["type_authority"], serde_json::json!(1.5));
    assert_eq!(rows[0]["scores"]["corpus"], serde_json::json!(0.8));
    assert_eq!(rows[1]["corpus"], serde_json::json!("main"));
    assert_eq!(rows[1]["scores"]["type_authority"], serde_json::json!(1.2));
    assert!(
        rows[1]["scores"].get("corpus").is_none(),
        "the primary row's factor is 1.0 and is not printed: {}",
        rows[1]
    );

    let archived = rows[0]["scores"]["final"].as_f64().expect("a final score");
    let primary = rows[1]["scores"]["final"].as_f64().expect("a final score");
    assert!(
        archived < primary,
        "the fixture must keep the archived row's final lower: {archived} vs {primary}"
    );
}

#[test]
fn the_human_table_shows_the_weight_only_when_it_is_not_neutral() {
    let dir = workspace();
    let plain = run_ok(dir.path(), &["search", "cache", "--no-embeddings"]);
    assert!(
        !header(&plain).contains("corpus"),
        "a primary-only answer carries no weight, so the table has no column: {plain}"
    );
    assert!(
        plain.contains("main:decisions/2024-03-01-one-cache.md"),
        "{plain}"
    );

    let federated = run_ok(
        dir.path(),
        &["search", "cache", "--no-embeddings", "--corpus", "all"],
    );
    assert!(
        federated.contains("corpora: main, context"),
        "the table names the corpora it read: {federated}"
    );
    assert!(
        federated.contains("main:decisions/2024-03-01-one-cache.md"),
        "a row names its corpus in the qualified-path form: {federated}"
    );
    assert!(
        federated.contains("context:2019-01-01-cache-server.md"),
        "{federated}"
    );
    assert!(
        header(&federated).contains("corpus"),
        "the weight column appears when a row carries a weight: {federated}"
    );
}

#[test]
fn no_workspace_keeps_the_single_corpus_report_shape() {
    // Case 1.2 in miniature: without a workspace the answer is what it was
    // before federation existed. No `corpora`, no row `corpus`, and no neutral
    // factor in the breakdown.
    let dir = workspace();
    let output = janus_in(dir.path())
        .env_remove("JANUS_WORKSPACE")
        .arg("--vault")
        .arg(dir.path().join("primary"))
        .args(["search", "cache", "--no-embeddings", "--json"])
        .output()
        .expect("run janus");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");

    assert!(report.get("corpora").is_none(), "{report}");
    for row in report["results"].as_array().expect("results") {
        assert!(row.get("corpus").is_none(), "{row}");
        assert!(row["scores"].get("corpus").is_none(), "{row}");
    }
}

#[test]
fn a_corpus_root_and_a_workspace_cannot_both_name_the_query() {
    let dir = workspace();
    let output = janus_in(dir.path())
        .arg("--vault")
        .arg(dir.path().join("primary"))
        .args(["search", "cache", "--no-embeddings"])
        .output()
        .expect("run janus");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--workspace"), "{stderr}");
}
