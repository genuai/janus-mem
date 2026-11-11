//! Cases 8.1–8.5 of the M7 plan: the config is part of the index identity
//! (M7 §3.8, M7-24 to M7-26, M7-32).
//!
//! The manifest records the tokenizer, the chunker, the embedder and the corpus
//! signature, but nothing about the config whose values the index caches. These
//! cases pin the missing fact: a `[paths]` edit makes the index stale, one read
//! cures it, and an edit to a section read per call changes nothing. Two more
//! pin M7-41: a process keeps the config it started with, so it never publishes
//! a manifest under a config the file on disk does not agree with.
//!
//! Every expected value is derived by hand from the corpus each test writes. No
//! test reads the wall clock, sleeps, or opens the network.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use common::run;
use janus_mem::config::{Config, Refresh};
use janus_mem::index::refresh::{self, Decision};
use janus_mem::index::store::LoadOptions;
use janus_mem::vault::{self, WalkOptions};
use serde_json::{Value, json};

/// The config a scratch corpus starts with: one path hint, so the document's
/// type comes from `[paths]` and from nowhere else.
fn config_text(paths_type: &str) -> String {
    format!(
        "index_dir = \".janus\"\ncorpus_id = \"cfg\"\n\n[paths]\n\"README.md\" = \"{paths_type}\"\n"
    )
}

/// Writes the config, replacing whatever was there.
fn write_config(root: &Path, paths_type: &str) {
    fs::write(root.join("janus.toml"), config_text(paths_type)).expect("write the config");
}

/// A corpus with one frontmatter-less document whose type `[paths]` decides.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "spec");
    fs::write(
        dir.path().join("README.md"),
        "# Readme\n\nWidgets and their prose.\n",
    )
    .expect("write the document");
    dir
}

/// The manifest a build published.
fn manifest(root: &Path) -> Value {
    let bytes = fs::read(root.join(".janus/manifest.json")).expect("the manifest");
    serde_json::from_slice(&bytes).expect("a manifest is JSON")
}

/// Rewrites the manifest without `config_hash`, which is what a build by a
/// binary that predates M7-1 leaves behind.
fn strip_config_hash(root: &Path) {
    let path = root.join(".janus/manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&path).expect("read the manifest"))
        .expect("manifest JSON");
    let object = manifest.as_object_mut().expect("the manifest is an object");
    assert!(
        object.remove("config_hash").is_some(),
        "the config hash is present"
    );
    fs::write(
        &path,
        serde_json::to_vec_pretty(&manifest).expect("encode the manifest"),
    )
    .expect("write the manifest");
}

/// One read from a process whose config says `[paths] paths_type`, so the test
/// can hold a config the file on disk has moved past.
fn read_as(root: &Path, paths_type: &str) -> refresh::ReadOutcome {
    let config = Config::from_toml(&config_text(paths_type), &root.join("janus.toml"))
        .expect("the config parses");
    let walk = vault::walk(root, &config, WalkOptions::default()).expect("walk the corpus");
    refresh::on_read(
        &root.join(".janus"),
        &LoadOptions::new(root, &config, common::today()),
        walk,
        Refresh::Auto,
    )
    .expect("a config edit is never an error")
}

/// Both payloads, so a test can prove that no publish happened.
fn payloads(root: &Path) -> (Vec<u8>, Vec<u8>) {
    (
        fs::read(root.join(".janus/index.bin")).expect("the index"),
        fs::read(root.join(".janus/manifest.json")).expect("the manifest"),
    )
}

/// The resolved type the answered generation stored for one document.
fn stored_type<'a>(outcome: &'a refresh::ReadOutcome, rel_path: &str) -> Option<&'a str> {
    let snapshot = &outcome.load.snapshot;
    snapshot
        .docs
        .iter()
        .find(|row| snapshot.strings.get(row.rel_path) == Some(rel_path))
        .and_then(|row| snapshot.strings.get(row.type_name))
}

// ────────────────────────────── case 8.1 ───────────────────────────────────

#[test]
fn case_8_1_a_paths_edit_makes_status_stale_with_reason_config() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);

    // A fresh report carries no reason at all, so its shape does not move.
    let fresh = common::json_ok(root, &["status", "--json"]);
    assert_eq!(fresh["index"]["stale"], json!(false));
    assert!(
        fresh["index"].get("stale_reason").is_none(),
        "a fresh report names no reason: {fresh}"
    );

    // One `[paths]` value flips the type the index cached for `README.md`.
    write_config(root, "note");
    let stale = common::json_ok(root, &["status", "--json"]);
    assert_eq!(stale["index"]["stale"], json!(true), "{stale}");
    assert_eq!(stale["index"]["stale_reason"], json!("config"), "{stale}");
    assert_eq!(stale["index"]["docs"], json!(1), "status never builds");
}

// ────────────────────────────── case 8.2 ───────────────────────────────────

#[test]
fn case_8_2_one_search_applies_the_new_type_and_status_then_reports_fresh() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);
    let before = common::search_json(root, &["widgets"]);
    assert_eq!(before["results"][0]["type"], json!("spec"), "{before}");

    write_config(root, "note");
    let output = run(root, &["search", "widgets", "--json"]);
    assert!(
        output.status.success(),
        "a config edit is not a failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("refresh: config changed"),
        "one line names the config: {stderr}"
    );
    let after: Value = serde_json::from_slice(&output.stdout).expect("the report is JSON");
    assert_eq!(after["results"][0]["type"], json!("note"), "{after}");
    assert_eq!(after["index"]["stale"], json!(false), "{after}");

    let status = common::json_ok(root, &["status", "--json"]);
    assert_eq!(status["index"]["stale"], json!(false));
    assert!(
        status["index"].get("stale_reason").is_none(),
        "the reason is gone once a read cures it: {status}"
    );
}

#[test]
fn a_read_that_sees_a_document_and_a_config_change_names_both() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);

    // One document edit and one `[paths]` edit, both before any read. The edit
    // adds bytes, so the corpus signature sees it without touching an mtime.
    fs::write(
        root.join("README.md"),
        "# Readme\n\nWidgets and their prose, edited.\n",
    )
    .expect("write the document");
    write_config(root, "note");

    // `status` names both causes too: "config" alone would hide the edit.
    let status = common::json_ok(root, &["status", "--json"]);
    assert_eq!(status["index"]["stale"], json!(true), "{status}");
    assert_eq!(
        status["index"]["stale_reason"],
        json!("corpus,config"),
        "{status}"
    );

    let output = run(root, &["search", "widgets", "--json"]);
    assert!(
        output.status.success(),
        "two edits are not a failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("refresh:"))
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        ["refresh: corpus and config changed"],
        "one line, and it names both causes: {stderr}"
    );
    let after: Value = serde_json::from_slice(&output.stdout).expect("the report is JSON");
    assert_eq!(after["results"][0]["type"], json!("note"), "{after}");
}

#[test]
fn case_8_2b_the_build_that_applied_the_change_reports_it_and_the_next_does_not() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);
    // The first build applied nothing: there was no previous manifest.
    let cold = common::json_ok(root, &["index", "--json"]);
    assert!(
        cold.get("config_changed").is_none(),
        "a cold build reports no change: {cold}"
    );

    write_config(root, "note");
    let applied = common::json_ok(root, &["index", "--json"]);
    assert_eq!(applied["config_changed"], json!(true), "{applied}");
    assert_eq!(
        applied["rebuilt"],
        json!(0),
        "a config edit re-chunks nothing"
    );
    assert_eq!(applied["reused"], json!(1), "{applied}");

    let again = common::json_ok(root, &["index", "--json"]);
    assert!(
        again.get("config_changed").is_none(),
        "the second build has nothing to change: {again}"
    );

    // A read that refreshed has already applied the change, so the build after
    // it has nothing left to change either.
    write_config(root, "spec");
    let _ = common::search_json(root, &["widgets"]);
    let after = common::json_ok(root, &["index", "--json"]);
    assert!(
        after.get("config_changed").is_none(),
        "a refresh applied it, so this build did not: {after}"
    );
}

#[test]
fn two_runs_of_index_record_the_same_config_hash() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);
    let first = manifest(root);
    common::index(root);
    let second = manifest(root);

    let hash = first["config_hash"]
        .as_str()
        .expect("the build records a config hash");
    assert_eq!(second["config_hash"].as_str(), Some(hash));

    // A constant hash would pass the two assertions above, so the hash must also
    // move when a covered value moves, and stay when only a comment moves.
    write_config(root, "note");
    common::index(root);
    let changed = manifest(root);
    let changed_hash = changed["config_hash"]
        .as_str()
        .expect("the build records a config hash")
        .to_owned();
    assert_ne!(changed_hash, hash, "a `[paths]` edit changes the hash");

    let commented = format!("{}# a comment carries no value\n", config_text("note"));
    fs::write(root.join("janus.toml"), commented).expect("write the config");
    common::index(root);
    assert_eq!(
        manifest(root)["config_hash"].as_str(),
        Some(changed_hash.as_str()),
        "a comment-only edit changes nothing"
    );
}

// ────────────────────────────── case 8.3 ───────────────────────────────────

#[test]
fn case_8_3_a_manifest_without_a_config_hash_is_stale_once_and_reuses_every_token() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);
    strip_config_hash(root);

    // An index built before the field existed cannot prove it matches.
    let stale = common::json_ok(root, &["status", "--json"]);
    assert_eq!(stale["index"]["stale"], json!(true), "{stale}");
    assert_eq!(stale["index"]["stale_reason"], json!("config"), "{stale}");

    // The cure is the ordinary metadata refresh: every token is reused.
    let cured = common::json_ok(root, &["index", "--json"]);
    assert_eq!(cured["rebuilt"], json!(0), "no re-chunk: {cured}");
    assert_eq!(cured["reused"], json!(1), "{cured}");
    assert_eq!(cured["retokenized"], json!(0), "{cured}");
    assert_eq!(cured["config_changed"], json!(true), "{cured}");
    assert_eq!(
        manifest(root)["format_version"],
        json!(6),
        "the format version does not move"
    );

    let fresh = common::json_ok(root, &["status", "--json"]);
    assert_eq!(fresh["index"]["stale"], json!(false), "{fresh}");
}

// ────────────────────────────── case 8.4 ───────────────────────────────────

#[test]
fn case_8_4_an_edit_to_a_query_time_section_changes_nothing_about_freshness() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);

    // Four sections, each read per call and cached nowhere.
    for extra in [
        "[authority]\nspec = 1.9\n",
        "[decay]\nhalf_life_days = 30\n",
        "[scoring]\ntitle_boost = 0.0\n",
        "[links]\nallow_disjoint_supersession = true\n",
    ] {
        fs::write(
            root.join("janus.toml"),
            format!("{}\n{extra}", config_text("spec")),
        )
        .expect("write the config");
        let status = common::json_ok(root, &["status", "--json"]);
        assert_eq!(status["index"]["stale"], json!(false), "{extra}");
        assert!(
            status["index"].get("stale_reason").is_none(),
            "{extra}: {status}"
        );
    }
}

#[test]
fn case_8_4b_an_edit_to_a_cached_section_makes_the_index_stale() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);

    // Four sections, each feeding a value the index stores per document. The
    // last line continues the `[paths]` table the corpus already has.
    for extra in [
        "\n[frontmatter]\ntype = [\"kind\"]\n",
        "\n[defaults]\ntype = \"idea\"\n",
        "\n[types]\nnote = \"idea\"\n",
        "\"docs/\" = \"principle\"\n",
    ] {
        fs::write(
            root.join("janus.toml"),
            format!("{}\n{extra}", config_text("spec")),
        )
        .expect("write the config");
        let status = common::json_ok(root, &["status", "--json"]);
        assert_eq!(status["index"]["stale"], json!(true), "{extra}");
        assert_eq!(
            status["index"]["stale_reason"],
            json!("config"),
            "{extra}: {status}"
        );
    }
}

// ────────────────────────────── case 8.5 ───────────────────────────────────

#[test]
fn case_8_5_config_changed_is_an_additive_field() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);
    write_config(root, "note");

    let changed = common::json_ok(root, &["index", "--json"]);
    assert_eq!(changed["api"], json!(1), "{changed}");
    assert_eq!(changed["config_changed"], json!(true), "{changed}");

    let again = common::json_ok(root, &["index", "--json"]);
    assert_eq!(again["api"], json!(1), "{again}");
    // The two receipts differ by exactly one key, so a consumer that ignores
    // the new field reads the old receipt unchanged.
    let keys = |value: &Value| {
        value
            .as_object()
            .expect("a receipt is an object")
            .keys()
            .cloned()
            .collect::<BTreeSet<String>>()
    };
    let difference = keys(&changed)
        .difference(&keys(&again))
        .cloned()
        .collect::<BTreeSet<String>>();
    assert_eq!(difference, BTreeSet::from(["config_changed".to_owned()]));
}

// ──────────────────────── the pinned config (M7-41) ────────────────────────

#[test]
fn a_process_whose_config_the_file_agrees_with_adopts_the_manifest() {
    let dir = scratch();
    let root = dir.path();
    write_config(root, "note");
    common::index(root);
    let before = payloads(root);

    // This process started when `[paths]` said `spec`. The file and the manifest
    // both say `note`, so the manifest describes the corpus and this read adopts
    // it. This is the case that stops two processes from overwriting each other
    // forever.
    let outcome = read_as(root, "spec");
    assert_eq!(outcome.decision, Decision::Fresh, "{:?}", outcome.decision);
    assert_eq!(outcome.decision.line(), None, "a fresh read prints nothing");
    assert!(!outcome.stale);
    assert!(!outcome.load.rebuilt, "nothing was published");
    assert_eq!(payloads(root), before, "a stale process writes nothing");
    assert_eq!(stored_type(&outcome, "README.md"), Some("note"));
}

#[test]
fn a_process_whose_config_is_the_stale_one_publishes_nothing_and_says_so() {
    let dir = scratch();
    let root = dir.path();
    write_config(root, "note");
    common::index(root);
    let before = payloads(root);
    // The file moves on to a third value, so the manifest and this process are
    // both behind it. Neither may publish, and one line says so.
    write_config(root, "principle");

    let outcome = read_as(root, "spec");
    assert_eq!(
        outcome.decision,
        Decision::SkippedConfigElsewhere,
        "{:?}",
        outcome.decision
    );
    assert_eq!(
        outcome.decision.line(),
        Some("refresh: skipped, config changed elsewhere")
    );
    assert!(outcome.stale, "the answer may not match the file");
    assert!(!outcome.load.rebuilt, "nothing was published");
    assert_eq!(payloads(root), before, "a stale process writes nothing");
    assert_eq!(
        stored_type(&outcome, "README.md"),
        Some("note"),
        "the answer is the generation's"
    );
}
