//! `janus index` end to end: receipts, reuse, `--rebuild`, `--dry-run`, `PATHS`.
//!
//! These run the real binary, so every assertion is about what a caller sees:
//! stdout, stderr, exit code, and the files under `.janus/`.

mod common;

use std::fs;
use std::path::Path;

use common::{fixture_vault, index, run, run_ok};

fn receipt(root: &Path, args: &[&str]) -> serde_json::Value {
    let mut all = vec!["index"];
    all.extend_from_slice(args);
    common::json_ok(root, &all)
}

#[test]
fn index_publishes_a_generation_and_reports_the_receipt() {
    let vault = fixture_vault();
    let report = receipt(vault.path(), &["--json"]);

    assert_eq!(report["api"], 1);
    assert!(report["docs"].as_u64().unwrap() > 0, "{report}");
    assert!(report["chunks"].as_u64().unwrap() > 0, "{report}");
    assert!(report["terms"].as_u64().unwrap() > 0, "{report}");
    assert!(report["rebuilt"].as_u64().unwrap() > 0, "{report}");
    assert_eq!(report["reused"], 0);
    assert!(report["retokenized"].as_u64().unwrap() > 0, "{report}");
    assert_eq!(report["dry_run"], false);
    assert!(report["index_sha"].is_string(), "{report}");

    assert!(vault.path().join(".janus/index.bin").exists());
    assert!(vault.path().join(".janus/manifest.json").exists());
}

#[test]
fn a_second_index_reuses_every_document() {
    let vault = fixture_vault();
    index(vault.path());
    let report = receipt(vault.path(), &["--json"]);

    assert_eq!(report["rebuilt"], 0, "{report}");
    assert_eq!(report["retokenized"], 0, "{report}");
    assert_eq!(report["reused"], report["docs"], "{report}");
}

#[test]
fn rebuild_retokenizes_everything() {
    let vault = fixture_vault();
    index(vault.path());
    let report = receipt(vault.path(), &["--rebuild", "--json"]);

    assert_eq!(report["reused"], 0, "{report}");
    assert_eq!(report["rebuilt"], report["docs"], "{report}");
    assert!(report["retokenized"].as_u64().unwrap() > 0, "{report}");
}

#[test]
fn dry_run_writes_nothing() {
    let vault = fixture_vault();
    let report = receipt(vault.path(), &["--dry-run", "--json"]);

    assert_eq!(report["dry_run"], true);
    assert!(report.get("index_sha").is_none(), "{report}");
    assert!(report["retokenized"].as_u64().unwrap() > 0, "{report}");
    assert!(!vault.path().join(".janus").exists());
}

#[test]
fn a_named_path_forces_only_that_document_to_re_tokenize() {
    let vault = fixture_vault();
    let first = receipt(vault.path(), &["--json"]);
    let docs = first["docs"].as_u64().unwrap();

    let report = receipt(vault.path(), &["notes/yaml-comments.md", "--json"]);

    assert_eq!(report["rebuilt"], 1, "{report}");
    assert_eq!(report["reused"].as_u64().unwrap(), docs - 1, "{report}");
    assert!(report["retokenized"].as_u64().unwrap() >= 1, "{report}");
}

#[test]
fn a_path_outside_the_corpus_is_named_not_ignored() {
    let vault = fixture_vault();
    let output = run(vault.path(), &["index", "notes/does-not-exist.md"]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("no document at"), "{stderr}");
    assert!(!vault.path().join(".janus/index.bin").exists());
}

#[test]
fn the_human_summary_names_the_counts() {
    let vault = fixture_vault();
    let stdout = run_ok(vault.path(), &["index"]);

    assert!(stdout.contains("index:"), "{stdout}");
    assert!(stdout.contains("document(s)"), "{stdout}");
    assert!(stdout.contains("reused 0"), "{stdout}");
    assert!(stdout.contains("retokenized"), "{stdout}");
    assert!(stdout.contains("published"), "{stdout}");
}

#[test]
fn a_clean_build_writes_no_stderr() {
    let vault = fixture_vault();
    let output = run(vault.path(), &["index"]);
    assert!(output.status.success());
    // `RUST_LOG` is off in `common::run`; a clean build prints no diagnostics.
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");

    // A second run is a pure reuse and still writes nothing to stderr.
    let output = run(vault.path(), &["index"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn an_empty_corpus_indexes_to_an_empty_generation() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"empty\"\n",
    )
    .unwrap();

    let report = receipt(dir.path(), &["--json"]);
    assert_eq!(report["docs"], 0, "{report}");
    assert_eq!(report["chunks"], 0, "{report}");
    assert_eq!(report["terms"], 0, "{report}");
    assert!(dir.path().join(".janus/index.bin").exists());

    // The empty index is still a generation: a read adopts it, not rebuilds.
    let report = receipt(dir.path(), &["--json"]);
    assert_eq!(report["reused"], 0, "{report}");
    assert_eq!(report["docs"], 0, "{report}");
}
