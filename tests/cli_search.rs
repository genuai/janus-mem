//! `janus search` end to end: the §3.1 result contract, JSON output, staleness
//! and refusal.

mod common;

use std::fs;

use common::{fixture_vault, index, result_paths, run, run_ok, search_json};
use serde_json::Value;

/// A vault where one document owns many chunk scores and nine others match.
fn contract_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"contract\"\n\n[chunking]\nmax_chars = 120\noverlap = 0.0\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();

    let mut body = String::from("---\ntitle: Dominant\ndate: 2025-03-01\n---\n\n# Dominant\n\n");
    for _ in 0..20 {
        body.push_str("alpha beta gamma delta. alpha beta gamma delta.\n\n");
    }
    fs::write(root.join("notes/dominant.md"), body).unwrap();

    for i in 0..9 {
        let text = format!(
            "---\ntitle: Other {i}\ndate: 2025-03-{:02}\n---\n\n# Other {i}\n\nalpha once.\n",
            i + 2
        );
        fs::write(root.join(format!("notes/other-{i}.md")), text).unwrap();
    }
    dir
}

#[test]
fn limit_returns_documents_not_chunks_of_one_document() {
    let vault = contract_vault();
    index(vault.path());

    let report = search_json(vault.path(), &["alpha", "--limit", "10"]);
    let got = result_paths(&report);
    assert_eq!(got.len(), 10, "{report}");
    assert_eq!(
        got.iter().collect::<std::collections::BTreeSet<_>>().len(),
        10,
        "a document appeared twice: {got:?}"
    );
    assert_eq!(
        got.iter()
            .filter(|path| *path == "notes/dominant.md")
            .count(),
        1,
        "the dominant document must appear once: {got:?}"
    );

    // The dominant document really does own several chunks' worth of score.
    let chunks = common::chunks_json(vault.path(), &[]);
    let dominant = chunks["docs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|doc| doc["rel_path"] == "notes/dominant.md")
        .unwrap();
    assert!(
        dominant["chunks"].as_array().unwrap().len() > 1,
        "{dominant}"
    );

    assert_eq!(
        result_paths(&search_json(vault.path(), &["alpha", "--limit", "3"])).len(),
        3
    );
}

#[test]
fn search_reports_spans_breadcrumbs_and_the_breakdown_but_never_text() {
    let vault = fixture_vault();
    index(vault.path());
    let stdout = run_ok(
        vault.path(),
        &["search", "postgres", "--intent", "history", "--json"],
    );
    let report: Value = serde_json::from_str(&stdout).unwrap();
    let hit = &report["results"][0];

    assert_eq!(report["api"], 1);
    assert_eq!(report["intent"], "history");
    for field in [
        "semantic",
        "lexical",
        "relevance",
        "type_authority",
        "status_factor",
        "authority",
        "decay",
        "final",
    ] {
        assert!(
            hit["scores"][field].is_number(),
            "missing scores.{field}: {hit}"
        );
    }
    assert!(hit["span"][0].is_number(), "{hit}");
    assert!(hit["span"][1].is_number(), "{hit}");
    assert!(hit["breadcrumb"].is_array(), "{hit}");
    assert_eq!(hit["chunk_id"].as_str().unwrap().len(), 64, "{hit}");

    // §8.5: the snapshot has no body, so no chunk text can reach the report.
    assert!(
        !stdout.contains("First link in the supersession cycle"),
        "{stdout}"
    );
    assert!(!stdout.contains("\"text\""), "{stdout}");
}

#[test]
fn the_human_table_has_the_documented_columns() {
    let vault = fixture_vault();
    index(vault.path());
    let stdout = run_ok(vault.path(), &["search", "cache", "--intent", "history"]);

    for header in ["score", "authority", "status", "date", "path"] {
        assert!(stdout.contains(header), "missing {header}: {stdout}");
    }
    assert!(stdout.contains("result(s)"), "{stdout}");
}

#[test]
fn an_empty_corpus_returns_no_results() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"empty\"\n",
    )
    .unwrap();

    let report = search_json(dir.path(), &["anything"]);
    assert!(report["results"].as_array().unwrap().is_empty(), "{report}");
    assert!(dir.path().join(".janus/index.bin").exists());
}

#[test]
fn a_stale_index_rebuilds_with_exactly_one_stderr_line() {
    let vault = fixture_vault();
    index(vault.path());

    let config_path = vault.path().join("janus.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    let rewritten = config.replace("corpus_id = \"fixtures\"", "corpus_id = \"renamed\"");
    assert_ne!(config, rewritten, "fixture config must carry a corpus_id");
    fs::write(&config_path, rewritten).unwrap();

    let output = run(vault.path(), &["search", "cache"]);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let stale: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("stale:"))
        .collect();
    assert_eq!(stale.len(), 1, "expected one stale line, got: {stderr:?}");
    assert!(stale[0].contains("corpus_id"), "{stale:?}");
    assert!(stale[0].contains("renamed"), "{stale:?}");

    // The rebuild published a generation that now agrees with the config.
    let output = run(vault.path(), &["search", "cache"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn search_without_an_index_builds_one() {
    let vault = fixture_vault();
    let report = search_json(vault.path(), &["cache"]);

    assert!(!result_paths(&report).is_empty(), "{report}");
    assert!(vault.path().join(".janus/index.bin").exists());
}

#[test]
fn no_rebuild_on_a_never_built_vault_says_so_and_writes_nothing() {
    let vault = fixture_vault();
    let output = run(vault.path(), &["search", "cache", "--no-rebuild"]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("no index yet"), "{stderr}");
    assert!(stderr.contains("janus index"), "{stderr}");
    assert!(!stderr.contains("unstable"), "{stderr}");
    assert!(!vault.path().join(".janus/index.bin").exists());
    assert!(!vault.path().join(".janus/manifest.json").exists());
}

#[test]
fn no_rebuild_on_a_stale_index_refuses_without_writing() {
    let vault = fixture_vault();
    index(vault.path());
    let dir = vault.path().join(".janus");
    let index_bytes = fs::read(dir.join("index.bin")).unwrap();
    let manifest_bytes = fs::read(dir.join("manifest.json")).unwrap();

    let config_path = vault.path().join("janus.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        config.replace("corpus_id = \"fixtures\"", "corpus_id = \"renamed\""),
    )
    .unwrap();

    let output = run(vault.path(), &["search", "cache", "--no-rebuild"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("index is unstable"), "{stderr}");
    assert!(stderr.contains("index.bin"), "{stderr}");

    assert_eq!(fs::read(dir.join("index.bin")).unwrap(), index_bytes);
    assert_eq!(fs::read(dir.join("manifest.json")).unwrap(), manifest_bytes);
}

#[test]
fn no_rebuild_reads_a_valid_generation() {
    let vault = fixture_vault();
    index(vault.path());
    let report = search_json(vault.path(), &["cache", "--no-rebuild"]);
    assert!(!result_paths(&report).is_empty(), "{report}");
}

#[test]
fn two_identical_runs_are_identical() {
    let vault = fixture_vault();
    index(vault.path());

    let args = ["search", "cache", "--intent", "history", "--json"];
    let mut first: Value = serde_json::from_str(&run_ok(vault.path(), &args)).unwrap();
    let mut second: Value = serde_json::from_str(&run_ok(vault.path(), &args)).unwrap();
    first["taken_ms"] = Value::from(0);
    second["taken_ms"] = Value::from(0);
    assert_eq!(first, second);
}

#[test]
fn search_finds_the_corpus_from_a_subdirectory() {
    let vault = fixture_vault();
    index(vault.path());

    let output = common::janus()
        .env("RUST_LOG", "off")
        .current_dir(vault.path().join("decisions"))
        .args(["search", "cache", "--intent", "history"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("decisions/"));
}

#[test]
fn an_unknown_intent_is_a_usage_error() {
    let vault = fixture_vault();
    index(vault.path());
    let output = run(vault.path(), &["search", "cache", "--intent", "sometime"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unknown intent"), "{stderr}");
    assert!(stderr.contains("history"), "{stderr}");
}
