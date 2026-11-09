//! Cases 9.1–9.4c and 10.3 of the M6 plan: index freshness and the cache (M6
//! §2.6, §3.9, §3.10).
//!
//! `status` reads the manifest and walks the corpus. It never builds and never
//! takes `.janus/lock`, which cases 9.1, 9.2 and 9.3 pin. Every expected value
//! is hand-derived from the vault the test writes. No test reads the wall clock.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{engine, fixture_vault, write_document};
use janus_mem::api::{Call, Tool};
use janus_mem::index::store::Lock;
use serde_json::{Value, json};

/// A corpus with one config line and one document.
fn scratch(id: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        format!("index_dir = \".janus\"\ncorpus_id = \"{id}\"\n"),
    )
    .expect("write the config");
    write_document(
        dir.path(),
        "notes/one.md",
        "---\ntitle: One\ndate: 2024-01-01\n---\n\nfirst prose\n",
    );
    dir
}

/// The manifest a build published.
fn manifest(root: &Path) -> Value {
    let bytes = fs::read(root.join(".janus/manifest.json")).expect("the manifest");
    serde_json::from_slice(&bytes).expect("a manifest is JSON")
}

// ─────────────────────────────── freshness ─────────────────────────────────

#[test]
fn case_9_1_before_the_first_build_every_index_fact_is_null() {
    let dir = fixture_vault();
    let root = dir.path();
    let report = common::json_ok(root, &["status", "--json"]);

    assert_eq!(report["api"], json!(1));
    assert_eq!(report["ok"], json!(true));
    assert_eq!(
        report["vault"],
        json!(
            fs::canonicalize(root)
                .expect("canonical root")
                .display()
                .to_string()
        )
    );
    assert_eq!(report["corpus_id"], json!("fixtures"));
    assert_eq!(report["writes_enabled"], json!(false));
    assert_eq!(report["cache"], json!({ "hits": 0, "misses": 0 }));
    // Only `present` is a fact: no build has run, so nothing else is known.
    assert_eq!(
        report["index"],
        json!({
            "present": false,
            "stale": null,
            "docs": null,
            "chunks": null,
            "built_at": null,
            "format_version": null,
            "index_sha": null,
        })
    );
    assert!(
        !root.join(".janus").exists(),
        "a status call must not build or take a lock"
    );
}

#[test]
fn case_9_2_a_built_index_reports_the_manifest_and_goes_stale() {
    let dir = fixture_vault();
    let root = dir.path();
    // The refusal of writes belongs to this phase, whatever the config says.
    let config = fs::read_to_string(root.join("janus.toml")).expect("the config");
    fs::write(
        root.join("janus.toml"),
        format!("{config}\n[mcp]\nallow_write = true\n"),
    )
    .expect("enable writes in the config");
    common::index(root);

    let built = manifest(root);
    let fresh = common::json_ok(root, &["status", "--json"]);
    assert_eq!(fresh["index"]["present"], json!(true));
    assert_eq!(fresh["index"]["stale"], json!(false));
    assert_eq!(fresh["index"]["docs"], built["docs"]);
    assert_eq!(fresh["index"]["chunks"], built["chunks"]);
    assert_eq!(fresh["index"]["built_at"], built["built_at"]);
    assert_eq!(fresh["index"]["format_version"], built["format_version"]);
    assert_eq!(fresh["index"]["index_sha"], built["index_sha"]);
    assert_eq!(fresh["writes_enabled"], json!(false), "M6 serves no write");

    // One edit makes the generation stale, and status still builds nothing.
    let payload = fs::read(root.join(".janus/index.bin")).expect("the built index");
    write_document(
        root,
        "notes/new.md",
        "---\ntitle: New\n---\n\nfresh prose\n",
    );
    let stale = common::json_ok(root, &["status", "--json"]);
    assert_eq!(stale["index"]["stale"], json!(true));
    assert_eq!(stale["index"]["docs"], built["docs"], "no build ran");
    assert_eq!(
        fs::read(root.join(".janus/index.bin")).expect("the index"),
        payload,
        "status never writes the payload"
    );
}

#[test]
fn case_9_3_status_answers_while_a_writer_holds_the_lock() {
    let dir = scratch("locked");
    let root = dir.path();
    common::index(root);
    let lock =
        Lock::acquire(&root.join(".janus"), Duration::from_secs(1)).expect("hold the writer lock");

    // A status call never asks for the lock, so it answers at once.
    let response = engine(root).call(&Call::new(Tool::Status, json!({})));
    assert!(response.ok, "{}", response.json);
    assert_eq!(response.json["index"]["present"], json!(true));
    assert_eq!(response.json["index"]["stale"], json!(false));
    assert_eq!(response.json["index"]["docs"], json!(1));
    drop(lock);
}

#[test]
fn case_9_3b_status_refuses_a_parameter_it_does_not_have() {
    // An illegal parameter is a usage failure, never a silent ignore (§2.5).
    let dir = scratch("params");
    let response = engine(dir.path()).call(&Call::new(Tool::Status, json!({ "verbose": true })));
    assert!(!response.ok, "{}", response.json);
    assert_eq!(response.code, Some("usage"));
}

// ──────────────────────────────── embeddings ───────────────────────────────

#[test]
fn case_9_4_a_hash_model_resolves_without_the_feature() {
    // The fixture config declares `[embedding] model = "hash-64"`, and hash
    // embedding needs no `embeddings` feature (§2.6, case 9.4).
    let dir = fixture_vault();
    let root = dir.path();
    common::index(root);

    let report = common::json_ok(root, &["status", "--json"]);
    assert_eq!(report["embeddings"]["available"], json!(true));
    assert_eq!(report["embeddings"]["model"], json!("hash-64"));

    // Case 9.4c: the generation keeps its vectors, and this process cannot
    // embed. The two facts disagree on purpose.
    let config = fs::read_to_string(root.join("janus.toml")).expect("the config");
    fs::write(
        root.join("janus.toml"),
        config.replace(
            "model = \"hash-64\"",
            "model = \"hash-64\"\nenabled = false",
        ),
    )
    .expect("disable embeddings");
    let disabled = common::json_ok(root, &["status", "--json"]);
    assert_eq!(disabled["embeddings"]["available"], json!(false));
    assert_eq!(
        disabled["embeddings"]["model"],
        json!("hash-64"),
        "the manifest still names the rows it holds"
    );
    let reason = disabled["embeddings"]["degraded"]
        .as_str()
        .expect("a reason");
    assert!(reason.contains("disabled"), "{reason}");
}

#[cfg(not(feature = "embeddings"))]
#[test]
fn case_9_4b_a_model_that_needs_the_feature_is_named_as_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"needs-feature\"\n\n[embedding]\nmodel = \"bge-small-en-v1.5\"\n",
    )
    .expect("write the config");
    write_document(root, "notes/one.md", "---\ntitle: One\n---\n\ntext\n");
    // A lexical build: this process cannot produce vectors for that model.
    common::run_ok(root, &["index", "--no-embeddings"]);

    let report = common::json_ok(root, &["status", "--json"]);
    assert_eq!(report["embeddings"]["available"], json!(false));
    assert_eq!(report["embeddings"]["model"], json!(null));
    let reason = report["embeddings"]["degraded"].as_str().expect("a reason");
    assert!(reason.contains("embeddings not compiled"), "{reason}");
}

// ───────────────────────────── stdout discipline ───────────────────────────

/// The modules allowed to write to stdout: each owns a command's renderer.
const RENDERERS: [&str; 12] = [
    "src/chunk/dump.rs",
    "src/doctor.rs",
    "src/ids.rs",
    "src/index/cmd.rs",
    "src/index/store.rs",
    "src/lineage.rs",
    "src/main.rs",
    "src/moves.rs",
    "src/progress.rs",
    "src/scan.rs",
    "src/search/mod.rs",
    "src/suggest.rs",
];

/// Every `.rs` file below `dir`, recursively.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read the source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn case_10_3_only_the_cli_renderers_write_to_stdout() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let allowed: BTreeSet<&str> = RENDERERS.into_iter().collect();
    let mut files = Vec::new();
    sources(&crate_root.join("src"), &mut files);
    assert!(files.len() > 20, "the walk must see the crate: {files:?}");

    for file in files {
        let rel_path = file
            .strip_prefix(crate_root)
            .expect("a path below the crate root")
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&file).expect("a source file");
        for (number, line) in text.lines().enumerate() {
            if line.contains("println!") || line.contains("stdout()") {
                assert!(
                    allowed.contains(rel_path.as_str()),
                    "{rel_path}:{} writes to stdout, and is not a renderer",
                    number + 1
                );
            }
        }
    }
}

// ──────────────────────────── metadata quality ─────────────────────────────

#[test]
fn case_3_2_status_reports_the_undated_share_without_building() {
    let dir = scratch("quality");
    let root = dir.path();
    // The scratch corpus holds one dated document; add one without a date.
    write_document(
        root,
        "notes/undated.md",
        "---\ntitle: Undated\n---\n\nno date here\n",
    );

    let report = common::json_ok(root, &["status", "--json"]);
    assert_eq!(
        report["metadata"],
        json!({ "dated": 1, "undated": 1, "share_undated": 0.5 }),
        "the same numbers scan reports, from the walk status already makes"
    );
    assert!(
        !root.join(".janus").exists(),
        "a status call must not build, with or without the metadata block"
    );
}

#[test]
fn case_3_2_status_counts_the_fixture_the_same_way_scan_does() {
    let dir = fixture_vault();
    let status = common::json_ok(dir.path(), &["status", "--json"]);
    let scan = common::scan_json(dir.path(), &[]);

    assert_eq!(status["metadata"]["dated"], scan["metadata"]["dated"]);
    assert_eq!(status["metadata"]["undated"], scan["metadata"]["undated"]);
    assert_eq!(scan["metadata"]["undated"], json!(1));
    // 1 of 19 is 5%, and the share is that percentage over one hundred.
    assert_eq!(status["metadata"]["share_undated"], json!(0.05));
}
