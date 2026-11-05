//! `janus search` end to end: the §3.1 result contract, JSON output, staleness
//! and refusal.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::{fixture_vault, index, result_paths, run, run_ok, search_json};
use serde_json::Value;

/// Every file in `dir`, so "wrote nothing" can be asserted byte-for-byte.
fn dir_snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("the index directory exists") {
        let entry = entry.expect("index directory entry");
        files.insert(
            entry.file_name().to_string_lossy().into_owned(),
            fs::read(entry.path()).expect("read an index directory file"),
        );
    }
    files
}

/// Copies a vault, `.janus/` included: a refresh oracle needs the generation as
/// it stood before the read (M4D §5 P2).
fn copy_vault(from: &Path, to: &Path) {
    common::copy_tree(from, to).expect("copy the corpus");
    copy_whole_tree(&from.join(".janus"), &to.join(".janus")).expect("copy the index");
}

fn copy_whole_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_whole_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

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
        "title_boost",
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

// M4D P2 row 1: a `corpus_id` change is cold. A read refuses with one line that
// names the command, and `.janus/` keeps every byte it had.
#[test]
fn a_changed_corpus_id_refuses_with_one_line() {
    let vault = fixture_vault();
    index(vault.path());
    let before = dir_snapshot(&vault.path().join(".janus"));

    let config_path = vault.path().join("janus.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    let rewritten = config.replace("corpus_id = \"fixtures\"", "corpus_id = \"renamed\"");
    assert_ne!(config, rewritten, "fixture config must carry a corpus_id");
    fs::write(&config_path, rewritten).unwrap();

    let output = run(vault.path(), &["search", "cache"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "expected exactly one line, got: {stderr:?}");
    assert!(
        lines[0].contains("index needs a full build: run janus index"),
        "{stderr:?}"
    );
    assert!(lines[0].contains("corpus_id"), "{stderr:?}");
    assert!(lines[0].contains("renamed"), "{stderr:?}");
    assert_eq!(
        dir_snapshot(&vault.path().join(".janus")),
        before,
        "a refused read writes nothing"
    );

    // `janus index` is the cure, and the next read prints nothing.
    index(vault.path());
    let output = run(vault.path(), &["search", "cache"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn search_json_reports_a_fresh_index_with_no_stderr_line() {
    let vault = fixture_vault();
    index(vault.path());

    let output = run(vault.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(report["api"], 1, "the key is additive");
    assert_eq!(report["index"]["stale"], false, "{report}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "",
        "this phase adds no line"
    );
}

#[test]
fn an_edited_document_is_refreshed_and_reported_fresh() {
    let vault = fixture_vault();
    index(vault.path());
    let path = vault.path().join("notes/yaml-comments.md");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\nan edit made after the build\n");
    fs::write(&path, text).unwrap();

    let output = run(vault.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    // M4D P2: the read refreshed the index, so the answer is current and the
    // report says so. One line, and the `stale:` word is retired.
    assert_eq!(report["index"]["stale"], false, "{report}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        vec!["refresh: corpus changed"],
        "{stderr:?}"
    );
    assert!(!stderr.contains("stale:"), "{stderr}");

    // The edit is in the index: a fresh read finds the new text and says nothing.
    let report = search_json(vault.path(), &["cache"]);
    assert_eq!(report["index"]["stale"], false, "{report}");
}

#[test]
fn an_edited_document_is_refreshed_before_facts_answer() {
    let vault = fixture_vault();
    index(vault.path());
    let path = vault.path().join("notes/yaml-comments.md");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\nan edit made after the build\n");
    fs::write(&path, text).unwrap();

    // M4D P2: `facts` inherits the refresh, so it answers from a current index.
    let output = run(vault.path(), &["facts", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: corpus changed\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["index"]["stale"], false, "{report}");
}

#[test]
fn search_without_an_index_builds_one() {
    let vault = fixture_vault();
    let output = run(vault.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: no index yet\n",
        "a read that builds says so, once (§3.3 row 3)"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!result_paths(&report).is_empty(), "{report}");
    assert!(vault.path().join(".janus/index.bin").exists());

    // The second read adopts the generation and prints nothing.
    let output = run(vault.path(), &["search", "cache", "--json"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
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

// M4D P2 DoD 1: one edit, one line, and payloads a cold build would write.
#[test]
fn a_refreshed_index_is_byte_identical_to_a_cold_rebuild_of_the_same_corpus() {
    let vault = fixture_vault();
    index(vault.path());
    let note = vault.path().join("2025-01-05-corpus-notes.md");
    let text = fs::read_to_string(&note).expect("read a fixture document");
    fs::write(&note, format!("{text}\n\nquokka refresh marker\n")).expect("edit a document");

    // The oracle is built **before** the refresh, from the same corpus and the
    // same old generation. Comparing against the refreshed bytes would prove
    // nothing.
    let copy = tempfile::tempdir().unwrap();
    copy_vault(vault.path(), copy.path());
    let cold = run(copy.path(), &["index", "--rebuild"]);
    assert!(cold.status.success(), "{cold:?}");
    let cold_index = fs::read(copy.path().join(".janus/index.bin")).expect("cold index");
    let cold_vectors = fs::read(copy.path().join(".janus/vectors.f32")).expect("cold vectors");

    let output = run(vault.path(), &["search", "quokka", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: corpus changed\n",
        "a refresh prints exactly one line"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["index"]["stale"], false, "{report}");
    assert!(
        result_paths(&report).contains(&"2025-01-05-corpus-notes.md".to_owned()),
        "the edit is visible: {report}"
    );

    assert_eq!(
        fs::read(vault.path().join(".janus/index.bin")).expect("refreshed index"),
        cold_index,
        "a refresh writes the bytes a cold build writes"
    );
    assert_eq!(
        fs::read(vault.path().join(".janus/vectors.f32")).expect("refreshed vectors"),
        cold_vectors,
        "a refresh reuses rows, and the payload still equals a cold build"
    );
}

// M4D P2 DoD 5 and D-11: a busy writer is a skip, not a failure.
#[test]
fn a_held_writer_lock_makes_a_stale_read_skip_and_still_answer() {
    let vault = fixture_vault();
    index(vault.path());
    let dir = vault.path().join(".janus");
    let before = dir_snapshot(&dir);
    fs::write(
        vault.path().join("notes/quokka.md"),
        "---\ntitle: Quokka\ndate: 2025-03-20\n---\n\n# Quokka\n\nquokka marker\n",
    )
    .expect("add a document after the build");

    // This thread holds the lock for the whole read: no sleep, no second thread,
    // so the read cannot race the holder.
    let lock = janus_mem::index::store::Lock::acquire(&dir, std::time::Duration::from_secs(1))
        .expect("take the writer lock");
    let output = run(vault.path(), &["search", "quokka", "--json"]);
    assert!(
        output.status.success(),
        "a busy lock must not fail a read: {output:?}"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: skipped, index busy\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["index"]["stale"], true, "{report}");
    assert!(
        !result_paths(&report).contains(&"notes/quokka.md".to_owned()),
        "the old generation cannot know the new document: {report}"
    );
    assert_eq!(
        dir_snapshot(&dir),
        before,
        "a skipped refresh writes nothing"
    );
    drop(lock);

    // Once the writer is gone the next read refreshes and finds the document.
    let output = run(vault.path(), &["search", "quokka", "--json"]);
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: corpus changed\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result_paths(&report).contains(&"notes/quokka.md".to_owned()),
        "{report}"
    );
}

// M4D P2 R-23: `--no-rebuild` keeps reading, says why it skipped, and writes
// nothing.
#[test]
fn no_rebuild_on_a_changed_corpus_reads_and_writes_nothing() {
    let vault = fixture_vault();
    index(vault.path());
    let dir = vault.path().join(".janus");
    let before = dir_snapshot(&dir);
    fs::write(
        vault.path().join("notes/quokka.md"),
        "---\ntitle: Quokka\ndate: 2025-03-20\n---\n\n# Quokka\n\nquokka marker\n",
    )
    .expect("add a document after the build");

    let output = run(
        vault.path(),
        &["search", "quokka", "--no-rebuild", "--json"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: skipped, --no-rebuild\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["index"]["stale"], true, "{report}");
    assert!(
        !result_paths(&report).contains(&"notes/quokka.md".to_owned()),
        "{report}"
    );
    assert_eq!(dir_snapshot(&dir), before, "--no-rebuild writes nothing");
}

// M4D P2 D-4: `refresh = "manual"` disables the corpus-changed row only.
#[test]
fn manual_refresh_on_a_changed_corpus_reads_and_writes_nothing() {
    let vault = fixture_vault();
    let config_path = vault.path().join("janus.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    let manual = config.replace("[index]\n", "[index]\nrefresh = \"manual\"\n");
    assert_ne!(
        config, manual,
        "the fixture config must carry an [index] table"
    );
    fs::write(&config_path, manual).unwrap();
    index(vault.path());

    let dir = vault.path().join(".janus");
    let before = dir_snapshot(&dir);
    fs::write(
        vault.path().join("notes/quokka.md"),
        "---\ntitle: Quokka\ndate: 2025-03-20\n---\n\n# Quokka\n\nquokka marker\n",
    )
    .expect("add a document after the build");

    let output = run(vault.path(), &["search", "quokka", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: skipped, refresh = manual\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["index"]["stale"], true, "{report}");
    assert_eq!(
        dir_snapshot(&dir),
        before,
        "a manual refresh writes nothing"
    );
}

// M4D P2 §3.3 row 4: a pair that no longer validates is rebuilt once.
#[test]
fn a_corrupt_pair_is_rebuilt_with_the_index_repaired_line() {
    let vault = fixture_vault();
    index(vault.path());
    fs::write(vault.path().join(".janus/index.bin"), b"not a snapshot").unwrap();

    let output = run(vault.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    // Two lines, both earned: the identity cache is gone (M5 case 1.9) and the
    // generation was rebuilt (§3.3 row 4). A build must not fail on either.
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "identity: no previous generation; ids re-derived\nrefresh: index repaired\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["index"]["stale"], false, "{report}");
    assert!(!result_paths(&report).is_empty(), "{report}");
}

// M4D P2 §3.3 row 6: a moved vault loads, repairs the manifest, and answers.
#[test]
fn a_moved_vault_repairs_the_manifest_and_answers() {
    let vault = fixture_vault();
    index(vault.path());
    let moved = tempfile::tempdir().unwrap();
    copy_vault(vault.path(), moved.path());

    // A moved root is a repair, and `--no-rebuild` declines it with its own
    // line: the generation still answers and nothing is written.
    let before = dir_snapshot(&moved.path().join(".janus"));
    let output = run(moved.path(), &["search", "cache", "--no-rebuild", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: skipped, --no-rebuild\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!result_paths(&report).is_empty(), "{report}");
    assert_eq!(dir_snapshot(&moved.path().join(".janus")), before);

    let output = run(moved.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: manifest repaired\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!result_paths(&report).is_empty(), "{report}");

    // The repair is complete: the next read prints nothing at all.
    let output = run(moved.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

// M4D P2 §3.4 step 4: a manifest repair is skipped by a busy lock too, and the
// generation still answers.
#[test]
fn a_held_writer_lock_skips_a_manifest_repair_and_still_answers() {
    let vault = fixture_vault();
    index(vault.path());
    let moved = tempfile::tempdir().unwrap();
    copy_vault(vault.path(), moved.path());
    let dir = moved.path().join(".janus");
    let before = dir_snapshot(&dir);

    let lock = janus_mem::index::store::Lock::acquire(&dir, std::time::Duration::from_secs(1))
        .expect("take the writer lock");
    let output = run(moved.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: skipped, index busy\n"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!result_paths(&report).is_empty(), "{report}");
    assert_eq!(dir_snapshot(&dir), before, "the manifest was not rewritten");
    drop(lock);

    // With the lock free the same read repairs the manifest and prints its line.
    let output = run(moved.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "refresh: manifest repaired\n"
    );
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

/// A vault with four documents covering owning scopes, `shared_with`, and global.
fn scope_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"scopes\"\n",
    )
    .unwrap();

    let docs = [
        ("acme.md", "title: Acme\nscope: org:acme", "scope probe"),
        ("beta.md", "title: Beta\nscope: org:beta", "scope probe"),
        ("global.md", "title: Global", "scope probe"),
        (
            "shared.md",
            "title: Shared\nscope: org:beta\nshared_with: [shared:finance]",
            "scope probe",
        ),
    ];
    for (path, front, body) in docs {
        fs::write(
            root.join(path),
            format!("---\n{front}\n---\n\n# {path}\n\n{body}\n"),
        )
        .unwrap();
    }
    dir
}

/// A vault where status, type and decay disagree, all dated for determinism.
fn status_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"statuses\"\n",
    )
    .unwrap();

    let docs = [
        (
            "draft-decision.md",
            "type: decision\nstatus: draft\ndate: 2024-06-01",
        ),
        (
            "active-decision.md",
            "type: decision\nstatus: active\ndate: 2024-06-01",
        ),
        (
            "frozen-spec.md",
            "type: spec\nstatus: frozen\ndate: 2024-06-01",
        ),
        (
            "retired-principle.md",
            "type: principle\nstatus: deprecated\ndate: 2024-06-01",
        ),
        (
            "active-note.md",
            "type: note\nstatus: active\ndate: 2024-06-01",
        ),
    ];
    for (path, front) in docs {
        fs::write(
            root.join(path),
            format!("---\ntitle: {path}\n{front}\n---\n\n# {path}\n\nquorum rules.\n"),
        )
        .unwrap();
    }
    dir
}

fn paths(report: &Value) -> std::collections::BTreeSet<String> {
    result_paths(report).into_iter().collect()
}

#[test]
fn types_and_tags_filter_from_the_published_snapshot() {
    let vault = fixture_vault();
    index(vault.path());

    let by_alias = search_json(
        vault.path(),
        &["cache", "--intent", "history", "--types", "adr"],
    );
    assert_eq!(result_paths(&by_alias).len(), 3, "{by_alias}");
    for hit in by_alias["results"].as_array().unwrap() {
        assert_eq!(hit["type"], "decision", "{hit}");
    }

    let specs = search_json(vault.path(), &["crlf", "--types", "spec"]);
    assert!(!result_paths(&specs).is_empty(), "{specs}");
    for hit in specs["results"].as_array().unwrap() {
        assert_eq!(hit["type"], "spec", "{hit}");
    }

    let tagged = search_json(vault.path(), &["yaml", "--tags", "comments"]);
    assert_eq!(
        result_paths(&tagged),
        vec!["notes/yaml-comments.md".to_owned()],
        "{tagged}"
    );

    // Tag matching folds case; the frontmatter tag is `yaml`.
    let folded = search_json(vault.path(), &["yaml", "--tags", "YAML"]);
    assert_eq!(
        result_paths(&folded),
        vec!["notes/yaml-comments.md".to_owned()]
    );

    let excluded = search_json(
        vault.path(),
        &["yaml", "--tags", "comments", "--types", "spec"],
    );
    assert!(result_paths(&excluded).is_empty(), "{excluded}");
}

#[test]
fn scopes_filter_end_to_end() {
    let vault = scope_vault();
    index(vault.path());

    assert_eq!(paths(&search_json(vault.path(), &["probe"])).len(), 4);
    assert_eq!(
        paths(&search_json(
            vault.path(),
            &["probe", "--scope", "org:acme"]
        )),
        std::collections::BTreeSet::from(["acme.md".to_owned(), "global.md".to_owned()])
    );
    assert_eq!(
        paths(&search_json(
            vault.path(),
            &["probe", "--scope", "org:beta"]
        )),
        std::collections::BTreeSet::from([
            "beta.md".to_owned(),
            "global.md".to_owned(),
            "shared.md".to_owned(),
        ])
    );
    assert_eq!(
        paths(&search_json(
            vault.path(),
            &["probe", "--scope", "shared:finance"]
        )),
        std::collections::BTreeSet::from(["global.md".to_owned(), "shared.md".to_owned()])
    );
    assert_eq!(
        paths(&search_json(
            vault.path(),
            &["probe", "--scope", "org:acme", "--scope-only"]
        )),
        std::collections::BTreeSet::from(["acme.md".to_owned()])
    );
}

#[test]
fn scope_only_without_scope_is_a_usage_error() {
    let vault = scope_vault();
    index(vault.path());
    let output = run(vault.path(), &["search", "probe", "--scope-only"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--scope-only"), "{stderr}");
    assert!(stderr.contains("--scope"), "{stderr}");

    // The same rule protects `facts`.
    let output = run(vault.path(), &["facts", "--scope-only"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn include_superseded_widens_current_and_nothing_else() {
    let vault = fixture_vault();
    index(vault.path());

    let default = search_json(vault.path(), &["cache", "--limit", "20"]);
    assert!(
        default["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["status"] != "superseded"),
        "{default}"
    );

    let flagged = search_json(
        vault.path(),
        &["cache", "--include-superseded", "--limit", "20"],
    );
    let superseded: Vec<&Value> = flagged["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|hit| hit["status"] == "superseded")
        .collect();
    assert_eq!(superseded.len(), 3, "{flagged}");
    for hit in &superseded {
        assert_eq!(hit["scores"]["status_factor"], 1.0, "{hit}");
    }

    // Same chunk, same span, same breadcrumb as the history view.
    let history = search_json(
        vault.path(),
        &["cache", "--intent", "history", "--limit", "20"],
    );
    let by_path: std::collections::BTreeMap<String, &Value> = history["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| (hit["rel_path"].as_str().unwrap().to_owned(), hit))
        .collect();
    for hit in &superseded {
        let reference = by_path[hit["rel_path"].as_str().unwrap()];
        assert_eq!(hit["chunk_id"], reference["chunk_id"], "{hit}");
        assert_eq!(hit["span"], reference["span"], "{hit}");
        assert_eq!(hit["breadcrumb"], reference["breadcrumb"], "{hit}");
    }

    // A search override must never widen the facts predicate.
    let facts = common::facts_json(vault.path(), &["--query", "cache"]);
    assert!(result_paths(&facts).is_empty(), "{facts}");
}

#[test]
fn a_decay_override_changes_the_score_but_not_a_timeless_type() {
    let vault = status_vault();
    index(vault.path());

    // `decision`/`spec`/`principle` are timeless in the shipped config, so a
    // half-life override changes their decay not at all (§9 gap 9: the flag
    // overrides the parameter, never the exemption). A `note` does decay, and
    // decays harder with a one-day half-life.
    let plain = search_json(vault.path(), &["quorum", "--intent", "current"]);
    let overridden = search_json(
        vault.path(),
        &["quorum", "--intent", "current", "--decay-half-life", "1"],
    );
    let decay = |report: &Value, path: &str| -> f64 {
        report["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["rel_path"] == path)
            .unwrap_or_else(|| panic!("no hit for {path}: {report}"))["scores"]["decay"]
            .as_f64()
            .unwrap()
    };
    let note_plain = decay(&plain, "active-note.md");
    let note_overridden = decay(&overridden, "active-note.md");
    assert!(note_plain < 1.0, "{plain}");
    assert!(note_overridden < note_plain, "{overridden}");
    for hit in overridden["results"].as_array().unwrap() {
        if hit["type"] != "note" {
            assert_eq!(hit["scores"]["decay"], 1.0, "{hit}");
        }
    }
}

#[test]
fn facts_excludes_draft_and_unknown_while_current_includes_them() {
    let vault = status_vault();
    index(vault.path());

    let current = paths(&search_json(vault.path(), &["quorum"]));
    assert_eq!(
        current,
        std::collections::BTreeSet::from([
            "active-decision.md".to_owned(),
            "active-note.md".to_owned(),
            "draft-decision.md".to_owned(),
            "frozen-spec.md".to_owned(),
        ])
    );

    let facts = paths(&common::facts_json(vault.path(), &["--query", "quorum"]));
    assert_eq!(
        facts,
        std::collections::BTreeSet::from(["active-decision.md".to_owned()])
    );
}

#[test]
fn facts_lists_active_decisions_specs_and_principles_without_a_query() {
    let vault = fixture_vault();
    index(vault.path());
    let report = common::facts_json(vault.path(), &[]);

    assert!(!result_paths(&report).is_empty(), "{report}");
    for hit in report["results"].as_array().unwrap() {
        assert_eq!(hit["status"], "active", "{hit}");
        assert!(
            ["decision", "spec", "principle"].contains(&hit["type"].as_str().unwrap()),
            "{hit}"
        );
        assert_eq!(hit["scores"]["decay"], 1.0, "{hit}");
        assert_eq!(hit["scores"]["status_factor"], 1.0, "{hit}");
    }
}

#[test]
fn facts_ignores_the_superseded_search_override() {
    let vault = fixture_vault();
    index(vault.path());

    // `--include-superseded` does not exist on `facts`; the predicate is fixed.
    let output = run(
        vault.path(),
        &["facts", "--query", "cache", "--include-superseded"],
    );
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(output.status.code(), Some(2));
}

/// A vault with one document per date source, plus one that has no date at all.
///
/// Every dated document carries `tags: [dated]`, so one search can include the
/// undated document and another can exclude it from the same index.
fn provenance_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"provenance\"\n",
    )
    .unwrap();

    // Ordinary documents: the date is in the frontmatter or nowhere.
    fs::write(
        root.join("frontmatter.md"),
        "---\ntitle: Authored\ndate: 2025-05-06\ntags: [dated]\n---\n\nprobe\n",
    )
    .unwrap();
    fs::write(
        root.join("undated.md"),
        "---\ntitle: Undated\n---\n\nprobe\n",
    )
    .unwrap();

    // File-name dates at all three precisions.
    for name in ["2025-04-03-day.md", "2025-04-month.md", "2025-year.md"] {
        fs::write(
            root.join(name),
            format!("---\ntitle: {name}\ntags: [dated]\n---\n\nprobe\n"),
        )
        .unwrap();
    }

    // Folder dates at all three precisions.
    for (folder, file) in [
        ("2025.07.08_folder", "day.md"),
        ("2025-09_folder", "month.md"),
        ("2025_folder", "year.md"),
    ] {
        fs::create_dir_all(root.join(folder)).unwrap();
        fs::write(
            root.join(folder).join(file),
            format!("---\ntitle: {folder}/{file}\ntags: [dated]\n---\n\nprobe\n"),
        )
        .unwrap();
    }
    dir
}

/// Reads `date_source` for every result, keyed by `rel_path`.
fn date_sources(report: &Value) -> std::collections::BTreeMap<String, String> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|hit| {
            (
                hit["rel_path"].as_str().expect("rel_path").to_owned(),
                hit["date_source"].as_str().expect("date_source").to_owned(),
            )
        })
        .collect()
}

#[test]
fn every_result_names_the_rung_that_produced_its_date() {
    let vault = provenance_vault();
    index(vault.path());
    let report = search_json(
        vault.path(),
        &["probe", "--intent", "history", "--limit", "20"],
    );

    // One entry per rung of the ladder, written by hand from the file tree.
    let expected = std::collections::BTreeMap::from([
        ("2025-04-03-day.md".to_owned(), "filename".to_owned()),
        ("2025-04-month.md".to_owned(), "filename_month".to_owned()),
        ("2025-year.md".to_owned(), "filename_year".to_owned()),
        ("2025.07.08_folder/day.md".to_owned(), "folder".to_owned()),
        (
            "2025-09_folder/month.md".to_owned(),
            "folder_month".to_owned(),
        ),
        ("2025_folder/year.md".to_owned(), "folder_year".to_owned()),
        ("frontmatter.md".to_owned(), "frontmatter".to_owned()),
        ("undated.md".to_owned(), "absent".to_owned()),
    ]);
    assert_eq!(date_sources(&report), expected, "{report}");

    // The field is additive: the envelope keeps its version.
    assert_eq!(report["api"], 1, "{report}");

    // Only the absent one is null: the source never outruns the date.
    for hit in report["results"].as_array().unwrap() {
        let absent = hit["date_source"] == "absent";
        assert_eq!(hit["date"].is_null(), absent, "{hit}");
    }
}

#[test]
fn the_undated_note_counts_only_returned_results() {
    let vault = provenance_vault();
    index(vault.path());

    let all = search_json(
        vault.path(),
        &["probe", "--intent", "current", "--limit", "20"],
    );
    let notes: Vec<&str> = all["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|note| note.as_str().expect("note"))
        .collect();
    assert!(
        notes.contains(&"1 result(s) have no date; decay uses the corpus median"),
        "{all}"
    );

    // The undated document is the only one without the tag, so this search
    // returns none of it and must not carry the line.
    let dated = search_json(
        vault.path(),
        &[
            "probe", "--intent", "history", "--tags", "dated", "--limit", "20",
        ],
    );
    assert_eq!(date_sources(&dated).len(), 7, "{dated}");
    assert!(
        !dated["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("no date")),
        "{dated}"
    );
}

#[test]
fn facts_carries_the_date_source_too() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"facts\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("decisions")).unwrap();
    fs::create_dir_all(root.join("specs")).unwrap();
    fs::write(
        root.join("decisions/adr.md"),
        "---\ntitle: Authored\ntype: decision\ndate: 2025-01-02\n---\n\nprobe\n",
    )
    .unwrap();
    fs::write(
        root.join("specs/2025-02-03-named.md"),
        "---\ntitle: Named\ntype: spec\n---\n\nprobe\n",
    )
    .unwrap();
    index(root);

    let report = common::facts_json(root, &[]);
    assert_eq!(
        date_sources(&report),
        std::collections::BTreeMap::from([
            ("decisions/adr.md".to_owned(), "frontmatter".to_owned()),
            (
                "specs/2025-02-03-named.md".to_owned(),
                "filename".to_owned()
            ),
        ]),
        "{report}"
    );
}

/// A vault whose documents are all `note`: it declares no fact type at all.
fn no_fact_types_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"cold-start\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();

    // Newest first is 2025-07-02, 2025-03-04, then the undated document.
    for (name, date) in [
        ("2025-03-04-older.md", "2025-03-04"),
        ("2025-07-02-newer.md", "2025-07-02"),
    ] {
        fs::write(
            root.join("notes").join(name),
            format!("---\ntitle: {name}\ndate: {date}\n---\n\nquorum probe\n"),
        )
        .unwrap();
    }
    fs::write(
        root.join("notes/undated.md"),
        "---\ntitle: Undated\n---\n\nquorum probe\n",
    )
    .unwrap();
    dir
}

#[test]
fn facts_explains_a_corpus_without_fact_types() {
    let vault = no_fact_types_vault();
    index(vault.path());
    let report = common::facts_json(vault.path(), &[]);

    let notes: Vec<&str> = report["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|note| note.as_str().expect("note"))
        .collect();
    assert!(
        notes.contains(
            &"no document declares type decision, spec or principle; \
              showing the newest documents instead"
        ),
        "{report}"
    );
    assert!(
        !notes
            .iter()
            .any(|note| note.contains("ordered by type authority")),
        "the fallback list is not authority-ordered: {report}"
    );

    // Newest of any type, then the undated document last.
    assert_eq!(
        result_paths(&report),
        vec![
            "notes/2025-07-02-newer.md".to_owned(),
            "notes/2025-03-04-older.md".to_owned(),
            "notes/undated.md".to_owned(),
        ],
        "{report}"
    );
    for hit in report["results"].as_array().unwrap() {
        assert_eq!(hit["type"], "note", "{hit}");
        // An undated document now decays by the corpus median (§3.13). The
        // fallback list is ordered by date, so its decay cannot move this order.
        assert!(hit["scores"]["decay"].as_f64().unwrap() <= 1.0, "{hit}");
    }
}

#[test]
fn facts_keeps_the_fact_list_when_a_fact_type_exists() {
    // The fixture has decisions, so the note must not appear there.
    let vault = fixture_vault();
    index(vault.path());
    let report = common::facts_json(vault.path(), &[]);
    assert!(
        !report["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("no document declares")),
        "{report}"
    );
}

// ──────────────────── validity windows, status and `--as-of` (M5 P3) ────────
//
// The graph fixture's dates are all ≤ 2022, so every expectation below is
// hand-derived from the frontmatter and none depends on the day the test runs.

/// The graph fixture with an index: these cases read metadata only.
fn graph_ready() -> tempfile::TempDir {
    let vault = common::graph_vault();
    index(vault.path());
    vault
}

/// One document's JSON hit, when the report returned it.
fn hit<'a>(report: &'a Value, rel_path: &str) -> Option<&'a Value> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|hit| hit["rel_path"] == rel_path)
}

fn notes(report: &Value) -> Vec<String> {
    report["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|note| note.as_str().expect("note text").to_owned())
        .collect()
}

#[test]
fn case_7_1_a_document_declared_active_but_superseded_is_absent_from_current() {
    // A temp vault isolates the status gate: the successor has no date at all,
    // so it closes no window (§3.6) and today is inside A's window. Only the
    // derived status can keep A out of `current`, so this fails on `main`.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"lies\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("old.md"),
        "---\ntitle: Old cache\nstatus: active\ndate: 2020-01-01\nsuperseded_by: new.md\n---\n\ncache rule\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("new.md"),
        "---\ntitle: New cache\nstatus: active\n---\n\nthe replacement, undated\n",
    )
    .unwrap();
    index(dir.path());

    let current = search_json(dir.path(), &["cache", "--intent", "current"]);
    assert_eq!(result_paths(&current), ["new.md"], "{current}");
    let lines = notes(&current);
    assert!(
        lines
            .iter()
            .any(|note| note.starts_with("1 document(s) hidden by the status gate")),
        "{current}"
    );
    assert!(
        !lines.iter().any(|note| note.contains("validity window")),
        "the window must not be the reason: {current}"
    );

    let history = search_json(dir.path(), &["cache", "--intent", "history"]);
    let lying = hit(&history, "old.md").expect("history returns it");
    assert_eq!(lying["status"], "active", "the declared status is reported");
    assert_eq!(lying["effective_status"], "superseded", "{lying}");

    // The committed fixture carries the same lie; its window is closed, so
    // the status gate is what keeps it out of `current` there too.
    let vault = graph_ready();
    let report = search_json(
        vault.path(),
        &["wins", "--intent", "history", "--no-embeddings"],
    );
    let lies = hit(&report, "bad/status-lies.md").expect("history returns it");
    assert_eq!(lies["effective_status"], "superseded", "{lies}");
}

#[test]
fn case_7_2_quiet_expiry_is_superseded_with_no_successor() {
    // `bad/quiet-expiry.md` declares `superseded` and nothing replaces it.
    let vault = graph_ready();
    let history = search_json(
        vault.path(),
        &["supersession", "--intent", "history", "--no-embeddings"],
    );
    let quiet = hit(&history, "bad/quiet-expiry.md").expect("history returns it");
    assert_eq!(quiet["effective_status"], "superseded", "{quiet}");
    assert_eq!(quiet["valid_to"], Value::Null, "{quiet}");
}

#[test]
fn case_7_3_expiry_is_a_window_property() {
    // `bad/expired.md`: active, valid from 2019-01-01 to 2020-01-01, so its
    // window is valid and simply over. It is absent from `current`, and it is
    // present in `history` with the declared status unchanged.
    let vault = graph_ready();
    let current = search_json(
        vault.path(),
        &["outside window", "--intent", "current", "--no-embeddings"],
    );
    assert!(result_paths(&current).is_empty(), "{current}");

    let history = search_json(
        vault.path(),
        &["outside window", "--intent", "history", "--no-embeddings"],
    );
    let expired = hit(&history, "bad/expired.md").expect("history returns it");
    assert_eq!(
        expired["effective_status"], "active",
        "no expired status exists"
    );
    assert_eq!(expired["valid_from"], "2019-01-01", "{expired}");
    assert_eq!(expired["valid_to"], "2020-01-01", "{expired}");
}

#[test]
fn case_7_4_include_superseded_admits_what_the_status_gate_hid() {
    // The flag admits `status-lies.md`, whose window is closed, so it also
    // proves that the flag cancels the window gate (M5-9).
    let vault = graph_ready();
    let flagged = search_json(
        vault.path(),
        &["wins", "--intent", "current", "--include-superseded"],
    );
    assert_eq!(result_paths(&flagged), ["bad/status-lies.md"], "{flagged}");
}

#[test]
fn case_7_5_the_chain_returns_only_its_last_link() {
    // A–D are declared superseded, so the status gate hides them. E is active
    // and its window has no end.
    let vault = graph_ready();
    let current = search_json(
        vault.path(),
        &["chain", "--intent", "current", "--no-embeddings"],
    );
    assert_eq!(
        result_paths(&current),
        ["chain/2022-01-01-cache-returns.md"],
        "{current}"
    );
}

#[test]
fn case_7_6_no_successor_and_no_end_is_current() {
    // The same claim one document at a time: E reports an open window and an
    // active effective status, and the report carries no window note.
    let vault = graph_ready();
    let current = search_json(
        vault.path(),
        &["chain", "--intent", "current", "--no-embeddings"],
    );
    let last = hit(&current, "chain/2022-01-01-cache-returns.md").expect("E");
    assert_eq!(last["effective_status"], "active", "{last}");
    assert_eq!(last["valid_from"], "2022-01-01", "{last}");
    assert_eq!(last["valid_to"], Value::Null, "{last}");
    // Ten documents of the fixture are excluded by their status (A–D, quiet
    // expiry, the status lie, the deprecated one, the global claim it replaces,
    // the shared-old pair and the one-sided link) and two are excluded by their
    // window (`bad/expired.md` and `bad/invalid-window.md`).
    assert_eq!(
        notes(&current),
        [
            "10 document(s) hidden by the status gate; use --intent history or --include-superseded",
            "2 document(s) hidden by the validity window; use --include-superseded or --intent history to see them",
        ],
        "{current}"
    );
}

#[test]
fn case_7_7_a_global_document_keeps_its_status_for_a_scope_that_does_not_match() {
    // A pair where the replaced document is declared active: `current` admits
    // G for `org:beta` because the successor does not match that scope, and
    // drops it for `org:acme`, where the successor does match.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"scopes\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("global-claim.md"),
        "---\ntitle: Global claim\nstatus: active\ndate: 2020-02-01\n---\n\ncache claim\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("acme-claim.md"),
        "---\ntitle: Acme claim\nstatus: active\ndate: 2020-03-01\nscope: org:acme\nsupersedes: global-claim.md\n---\n\ncache claim\n",
    )
    .unwrap();
    index(dir.path());

    let beta = search_json(dir.path(), &["cache", "--scope", "org:beta"]);
    assert_eq!(result_paths(&beta), ["global-claim.md"], "{beta}");

    let acme = search_json(dir.path(), &["cache", "--scope", "org:acme"]);
    assert_eq!(result_paths(&acme), ["acme-claim.md"], "{acme}");
}

#[test]
fn case_7_7b_scope_only_does_not_change_who_supersedes_whom() {
    // The mirror of the same pair: a global document replaces an owned one.
    // A scope-only query drops the global document from the result set
    // (§6.12 rule 4) but the global successor still supersedes, so the owned
    // document must not report itself current (§6.12 rule 7, M5-31).
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"only\"\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("a")).unwrap();
    fs::write(
        dir.path().join("a/acme.md"),
        "---\ntitle: Acme cache\nstatus: active\ndate: 2020-01-01\nscope: org:acme\n---\n\ncache rule\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("global.md"),
        "---\ntitle: Global cache\nstatus: active\ndate: 2020-06-01\nsupersedes: a/acme.md\n---\n\nthe cache is replaced\n",
    )
    .unwrap();
    index(dir.path());

    // Without `--scope-only` the global successor is returned and the owned
    // document is superseded: `cache` matches both bodies.
    let plain = search_json(dir.path(), &["cache", "--scope", "org:acme"]);
    assert_eq!(result_paths(&plain), ["global.md"], "{plain}");
    let history = search_json(dir.path(), &["cache", "--intent", "history"]);
    let owned = hit(&history, "a/acme.md").expect("history returns the owned document");
    assert_eq!(owned["effective_status"], "superseded", "{owned}");

    // With `--scope-only` the global document leaves the result set, and the
    // owned document is still superseded: the result set is empty.
    let only = search_json(
        dir.path(),
        &["cache", "--scope", "org:acme", "--scope-only"],
    );
    assert!(result_paths(&only).is_empty(), "{only}");
}

#[test]
fn case_7_8_an_undated_document_stays_in_current() {
    // §6.10 rule 9 and R-4: a missing `valid_from` passes the window gate, so
    // the fixture's one undated document still answers its eval query.
    let vault = fixture_vault();
    index(vault.path());
    let report = search_json(
        vault.path(),
        &["undated note declared id identity", "--intent", "current"],
    );
    // Rank 1 is not asserted: an undated document decays by the corpus median
    // (§3.13), so its place moves with the wall clock. Its presence under
    // `current`, with no window, is the §6.10 rule 9 contract.
    let found = report["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|hit| hit["rel_path"] == "notes/undated.md")
        .unwrap_or_else(|| panic!("the undated document left current: {report}"));
    assert_eq!(found["valid_from"], Value::Null);
}

#[test]
fn case_7_9_include_superseded_does_not_admit_deprecated() {
    let vault = graph_ready();
    let flagged = search_json(
        vault.path(),
        &[
            "deprecated",
            "--intent",
            "current",
            "--include-superseded",
            "--no-embeddings",
        ],
    );
    assert!(result_paths(&flagged).is_empty(), "{flagged}");

    let history = search_json(
        vault.path(),
        &["deprecated", "--intent", "history", "--no-embeddings"],
    );
    assert_eq!(result_paths(&history), ["bad/deprecated.md"], "{history}");
}

#[test]
fn case_8_1_as_of_2020_03_01_returns_the_first_link() {
    // A's window is [2020-01-01, 2020-07-01): B starts on 2020-07-01.
    let vault = graph_ready();
    let report = search_json(
        vault.path(),
        &["chain", "--as-of", "2020-03-01", "--no-embeddings"],
    );
    assert_eq!(
        result_paths(&report),
        ["chain/2020-01-01-adopt-postgres.md"],
        "{report}"
    );
    assert_eq!(report["results"][0]["valid_to"], "2020-07-01", "{report}");
    assert_eq!(report["as_of"], "2020-03-01", "{report}");
}

#[test]
fn case_8_2_as_of_2025_01_01_returns_the_last_link() {
    // Only E has a window that reaches 2025: the query term `chain` also keeps
    // the near-identical pair out, and neither is linked to the chain.
    let vault = graph_ready();
    let report = search_json(
        vault.path(),
        &["chain", "--as-of", "2025-01-01", "--no-embeddings"],
    );
    assert_eq!(
        result_paths(&report),
        ["chain/2022-01-01-cache-returns.md"],
        "{report}"
    );
    assert_eq!(report["results"][0]["valid_to"], Value::Null, "{report}");
    assert_ne!(
        result_paths(&report),
        result_paths(&search_json(
            vault.path(),
            &["chain", "--as-of", "2020-03-01"]
        )),
        "the two dates must answer with different documents"
    );
}

#[test]
fn case_8_3_the_same_query_without_the_flag_returns_the_last_link() {
    let vault = graph_ready();
    let report = search_json(
        vault.path(),
        &["chain", "--intent", "current", "--no-embeddings"],
    );
    assert_eq!(
        result_paths(&report),
        ["chain/2022-01-01-cache-returns.md"],
        "{report}"
    );
    assert!(report.get("as_of").is_none(), "{report}");
}

#[test]
fn case_8_4_a_year_expands_to_its_first_day() {
    // `2021` becomes 2021-01-01, which is C's first day and D's predecessor's
    // last day plus one: C's window is [2021-01-01, 2021-07-01).
    let vault = graph_ready();
    let report = search_json(
        vault.path(),
        &["chain", "--as-of", "2021", "--no-embeddings"],
    );
    assert_eq!(report["as_of"], "2021-01-01", "{report}");
    assert_eq!(
        result_paths(&report),
        ["chain/2021-01-01-cache-in-front.md"],
        "{report}"
    );
    assert_eq!(report["results"][0]["valid_to"], "2021-07-01", "{report}");
}

#[test]
fn case_8_5_as_of_admits_a_draft_and_does_not_decay() {
    // The draft is dated 2019-05-01, so it is inside its window at 2020-03-01
    // even though `current` would score it at 0.7 and decay it.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"draft\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("draft.md"),
        "---\ntitle: Cache draft\nstatus: draft\ndate: 2019-05-01\n---\n\ncache draft\n",
    )
    .unwrap();
    index(dir.path());

    let report = search_json(dir.path(), &["cache", "--as-of", "2020-03-01"]);
    let draft = hit(&report, "draft.md").expect("the draft is returned");
    assert_eq!(draft["status"], "draft", "{report}");
    assert_eq!(draft["effective_status"], "draft", "{report}");
    assert_eq!(draft["scores"]["decay"], 1.0, "{report}");
    assert_eq!(draft["scores"]["status_factor"], 1.0, "{report}");
}

#[test]
fn case_8_6_a_document_with_no_window_is_counted_and_excluded() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"undated\"\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("undated.md"),
        "---\ntitle: Cache undated\n---\n\ncache with no date\n",
    )
    .unwrap();
    index(dir.path());

    let report = search_json(dir.path(), &["cache", "--as-of", "2019-01-01"]);
    assert!(result_paths(&report).is_empty(), "{report}");
    assert_eq!(
        notes(&report),
        ["1 document(s) have no validity window"],
        "{report}"
    );
}

#[test]
fn case_8_7_an_unparseable_as_of_is_a_usage_error() {
    let vault = graph_ready();
    let output = run(vault.path(), &["search", "cache", "--as-of", "2025-13-40"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("2025-13-40"), "{stderr}");
}

#[test]
fn case_8_8_as_of_wins_over_intent_with_one_note() {
    // `history` would return A and B (both match `chain` and no window gate
    // applies), so the window alone is what makes this one result.
    let vault = graph_ready();
    let report = search_json(
        vault.path(),
        &[
            "chain",
            "--as-of",
            "2020-03-01",
            "--intent",
            "history",
            "--no-embeddings",
        ],
    );
    assert_eq!(
        result_paths(&report),
        ["chain/2020-01-01-adopt-postgres.md"],
        "{report}"
    );
    assert!(
        notes(&report).contains(&"as-of 2020-03-01 overrides intent history".to_owned()),
        "{report}"
    );
    assert_eq!(report["intent"], "history", "the row that ranked: {report}");
    assert_eq!(report["as_of"], "2020-03-01", "{report}");
}

#[test]
fn case_8_9_as_of_respects_the_scope_rule_on_the_global_pair() {
    // G is global and replaced by H of `org:acme` on 2020-03-01. For
    // `org:acme` the window ends there; for `org:beta` it stays open. Both
    // dates below are inside either window, and 2020-02-15 is before H starts.
    let vault = graph_ready();
    let acme = search_json(
        vault.path(),
        &[
            "global",
            "--as-of",
            "2020-02-15",
            "--scope",
            "org:acme",
            "--no-embeddings",
        ],
    );
    let for_acme = hit(&acme, "scopes/global-claim.md").expect("G for acme");
    assert_eq!(for_acme["valid_to"], "2020-03-01", "{acme}");

    let beta = search_json(
        vault.path(),
        &[
            "global",
            "--as-of",
            "2020-02-15",
            "--scope",
            "org:beta",
            "--no-embeddings",
        ],
    );
    let for_beta = hit(&beta, "scopes/global-claim.md").expect("G for beta");
    assert_eq!(for_beta["valid_to"], Value::Null, "{beta}");
}

#[test]
fn case_8_10_an_as_of_report_names_the_parameter_and_not_an_intent() {
    let vault = graph_ready();
    let report = search_json(
        vault.path(),
        &[
            "chain",
            "--as-of",
            "2020-03-01",
            "--intent",
            "exhaustive",
            "--no-embeddings",
        ],
    );
    assert_eq!(report["api"], 1, "{report}");
    assert_eq!(report["intent"], "history", "the row that ranked: {report}");
    assert_eq!(report["as_of"], "2020-03-01", "{report}");
    assert!(
        notes(&report).contains(&"as-of 2020-03-01 overrides intent exhaustive".to_owned()),
        "{report}"
    );
}
