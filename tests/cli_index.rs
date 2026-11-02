//! `janus index` end to end: receipts, reuse, `--rebuild`, `--dry-run`, `PATHS`.
//!
//! These run the real binary, so every assertion is about what a caller sees:
//! stdout, stderr, exit code, and the files under `.janus/`.

mod common;

use std::fs;
use std::path::Path;

use common::{fixture_vault, index, janus, run, run_ok};

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

// ─────────────────── the two-file generation through the CLI ───────────────

/// Every file in `.janus/`, so "wrote nothing" can be asserted byte-for-byte.
fn dir_snapshot(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    for entry in fs::read_dir(dir).expect("the index directory exists") {
        let entry = entry.expect("index directory entry");
        files.insert(
            entry.file_name().to_string_lossy().into_owned(),
            fs::read(entry.path()).expect("read an index directory file"),
        );
    }
    files
}

fn manifest(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(root.join(".janus/manifest.json")).expect("read manifest"))
        .expect("the manifest is JSON")
}

/// Rewrites `manifest.json`, so a stale or corrupt generation is built directly.
fn patch_manifest(root: &Path, patch: impl FnOnce(&mut serde_json::Value)) {
    let path = root.join(".janus/manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read manifest")).expect("manifest JSON");
    patch(&mut manifest);
    fs::write(
        path,
        serde_json::to_vec_pretty(&manifest).expect("encode manifest"),
    )
    .expect("patch manifest");
}

#[test]
fn index_writes_vectors_f32_that_match_the_manifest() {
    let vault = fixture_vault();
    let report = receipt(vault.path(), &["--json"]);
    let manifest = manifest(vault.path());

    let bytes = fs::read(vault.path().join(".janus/vectors.f32")).expect("the payload exists");
    let chunks = report["chunks"].as_u64().expect("chunks");
    assert_eq!(manifest["format_version"], 2);
    assert_eq!(manifest["embedder"]["name"], "hash-64");
    assert_eq!(manifest["embedder"]["dim"], 64);
    assert_eq!(manifest["vectors_len"], bytes.len() as u64);
    assert_eq!(
        manifest["vectors_sha"],
        blake3::hash(&bytes).to_hex().to_string()
    );
    // 64 f32 values per row, one row per chunk occurrence.
    assert_eq!(bytes.len() as u64, chunks * 64 * 4);
    assert_eq!(manifest["chunks"], chunks);
}

#[test]
fn a_second_index_produces_byte_identical_payloads() {
    let vault = fixture_vault();
    index(vault.path());
    let first_index = fs::read(vault.path().join(".janus/index.bin")).unwrap();
    let first_vectors = fs::read(vault.path().join(".janus/vectors.f32")).unwrap();
    let first_sha = manifest(vault.path())["vectors_sha"].clone();

    index(vault.path());

    assert_eq!(
        fs::read(vault.path().join(".janus/index.bin")).unwrap(),
        first_index,
        "the second build reuses tokens and reproduces the snapshot"
    );
    assert_eq!(
        fs::read(vault.path().join(".janus/vectors.f32")).unwrap(),
        first_vectors,
        "the embedder is deterministic, so re-embedding everything is byte-identical"
    );
    assert_eq!(manifest(vault.path())["vectors_sha"], first_sha);
}

#[test]
fn no_embeddings_drops_the_payload_with_one_line_and_the_next_read_loads() {
    let vault = fixture_vault();
    index(vault.path());
    assert!(vault.path().join(".janus/vectors.f32").exists());

    // Default tracing on purpose: the drop is a diagnostic, and silencing it
    // would make "says so" unverifiable.
    let output = janus()
        .arg("--vault")
        .arg(vault.path())
        .args(["index", "--no-embeddings"])
        .output()
        .expect("run janus index --no-embeddings");
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!vault.path().join(".janus/vectors.f32").exists());

    let stderr = String::from_utf8(output.stderr).unwrap();
    let about_vectors: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("vectors"))
        .collect();
    assert_eq!(
        about_vectors.len(),
        1,
        "expected exactly one line about the dropped payload: {stderr:?}"
    );
    assert!(about_vectors[0].contains("dropped"), "{stderr:?}");

    let manifest = manifest(vault.path());
    assert!(manifest["embedder"].is_null(), "{manifest}");
    assert!(manifest["vectors_sha"].is_null(), "{manifest}");
    assert!(manifest["vectors_len"].is_null(), "{manifest}");

    // The next read resolves to `embedder: None`, so nothing is stale and
    // `--no-rebuild` loads the generation unchanged.
    let before = dir_snapshot(&vault.path().join(".janus"));
    let output = run(vault.path(), &["search", "cache", "--no-rebuild", "--json"]);
    assert!(
        output.status.success(),
        "a vectors-less generation must load: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
    assert_eq!(dir_snapshot(&vault.path().join(".janus")), before);
}

#[test]
fn indexing_again_after_no_embeddings_restores_the_payload() {
    let vault = fixture_vault();
    index(vault.path());
    run_ok(vault.path(), &["index", "--no-embeddings"]);
    assert!(!vault.path().join(".janus/vectors.f32").exists());

    index(vault.path());
    assert!(vault.path().join(".janus/vectors.f32").exists());
    assert_eq!(manifest(vault.path())["embedder"]["name"], "hash-64");
}

#[test]
fn a_format_version_1_manifest_is_refused_with_a_warning_and_rebuilt() {
    let vault = fixture_vault();
    index(vault.path());
    patch_manifest(vault.path(), |manifest| {
        manifest["format_version"] = serde_json::json!(1)
    });

    let output = run(vault.path(), &["search", "cache"]);
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let stale: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("stale:"))
        .collect();
    assert_eq!(stale.len(), 1, "expected one stale line, got {stderr:?}");
    assert!(stale[0].contains("format_version 1 != 2"), "{stale:?}");
    assert_eq!(manifest(vault.path())["format_version"], 2);

    // `--no-rebuild` refuses the same state without writing.
    patch_manifest(vault.path(), |manifest| {
        manifest["format_version"] = serde_json::json!(1)
    });
    let before = dir_snapshot(&vault.path().join(".janus"));
    let output = run(vault.path(), &["search", "cache", "--no-rebuild"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains(".janus/index.bin"),
        "the refusal names the generation it could not use"
    );
    assert_eq!(dir_snapshot(&vault.path().join(".janus")), before);
}

#[test]
fn index_says_why_it_could_not_embed() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"no-model\"\n\n[embedding]\nmodel = \"AllMiniLML6V2\"\n",
    )
    .unwrap();

    // Default tracing: a degraded resolution is a diagnostic, not silence.
    let output = janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("index")
        .output()
        .expect("run janus index");
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("embeddings unavailable"))
        .collect();
    assert_eq!(lines.len(), 1, "expected one reason line: {stderr:?}");
    assert!(lines[0].contains("not compiled"), "{stderr:?}");
    assert!(lines[0].contains("AllMiniLML6V2"), "{stderr:?}");
    assert!(!dir.path().join(".janus/vectors.f32").exists());

    // `--no-embeddings` is an explicit choice, so it explains nothing.
    let output = janus()
        .arg("--vault")
        .arg(dir.path())
        .args(["index", "--no-embeddings"])
        .output()
        .expect("run janus index --no-embeddings");
    assert!(output.status.success());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("embeddings unavailable"),
        "the flag is not a degradation"
    );
}
