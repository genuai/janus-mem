//! `janus scan` end-to-end: discovery, resolved metadata, JSON output, exit codes.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::path::Path;

fn janus() -> Command {
    Command::cargo_bin("janus").unwrap()
}

/// Creates a vault with a config and a small, deliberately mixed tree.
fn sample_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\n\n[index]\nexclude = [\"archive/private/**\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".janus")).unwrap();
    fs::create_dir_all(root.join("decisions")).unwrap();
    fs::create_dir_all(root.join("archive/private")).unwrap();
    fs::create_dir_all(root.join("target")).unwrap();

    fs::write(root.join("notes.md"), "---\ntitle: Notes\n---\n\n# Notes\n").unwrap();
    fs::write(root.join("decisions/adr-1.md"), "# ADR 1\n").unwrap();
    fs::write(root.join("decisions/draft.txt"), "not markdown").unwrap();
    fs::write(root.join("archive/private/secret.md"), "# secret\n").unwrap();
    fs::write(root.join(".hidden.md"), "# hidden\n").unwrap();
    fs::write(root.join("target/build.md"), "# build\n").unwrap();

    dir
}

#[test]
fn scan_lists_markdown_and_attributes_skips() {
    let vault = sample_vault();

    janus()
        .arg("--vault")
        .arg(vault.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("decisions/adr-1.md"))
        .stdout(predicate::str::contains("notes.md"))
        .stdout(predicate::str::contains("2 document(s)"))
        .stdout(predicate::str::contains("archive/private/secret.md").not())
        .stdout(predicate::str::contains(".hidden.md").not())
        .stdout(predicate::str::contains("config_exclude"))
        .stdout(predicate::str::contains("not_markdown"));
}

#[test]
fn scan_finds_the_corpus_from_a_subdirectory() {
    let vault = sample_vault();
    let nested = vault.path().join("decisions");

    janus()
        .current_dir(&nested)
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("notes.md"));
}

#[test]
fn scan_reports_a_corpus_without_a_config() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".janus")).unwrap();

    // `.janus/` makes the directory discoverable, and the missing config is then
    // reported by name rather than as a mystery.
    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("scan")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("janus.toml"));
}

#[test]
fn scan_fails_clearly_when_no_corpus_can_be_found() {
    let dir = tempfile::tempdir().unwrap();

    janus()
        .current_dir(dir.path())
        .arg("scan")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("no corpus found"))
        .stderr(predicate::str::contains("janus init"));
}

#[test]
fn scan_refuses_a_vault_path_that_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    fs::write(&file, "# notes\n").unwrap();

    janus()
        .arg("--vault")
        .arg(&file)
        .arg("scan")
        .assert()
        .code(4);
}

#[test]
fn scan_works_on_an_empty_corpus() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("janus.toml"), "index_dir = \".janus\"\n").unwrap();

    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("0 document(s)"));
}

#[test]
fn scan_surfaces_config_warnings_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\n\n[authority]\nweird = 0.0\n",
    )
    .unwrap();

    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("scan")
        .assert()
        .success()
        // Diagnostics go to stderr so `--json` stdout always parses (§8.2).
        .stderr(predicate::str::contains("warning:"))
        .stderr(predicate::str::contains("weird"));
}

#[test]
fn scan_never_writes_to_the_vault() {
    let vault = sample_vault();
    let before = snapshot(vault.path());

    janus()
        .arg("--vault")
        .arg(vault.path())
        .arg("scan")
        .assert()
        .success();

    assert_eq!(
        before,
        snapshot(vault.path()),
        "scan must not mutate the vault"
    );
}

/// A cheap recursive listing: every path plus its length.
fn snapshot(root: &Path) -> Vec<(String, u64)> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.push((rel, entry.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
    }

    let mut out = Vec::new();
    visit(root, root, &mut out);
    out.sort();
    out
}

// ─────────────────────────────── JSON output ───────────────────────────────

/// A vault whose single document declares every metadata key the report emits.
fn metadata_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"main\"\n",
    )
    .unwrap();
    fs::write(
        root.join("2024-11-02-postgres.md"),
        "---\n\
         type: adr\n\
         status: draft\n\
         title: Postgres cache\n\
         author: claude-opus\n\
         doc_version: \"2.0\"\n\
         schema_version: 1\n\
         scope: ORG:Acme\n\
         shared_with: [shared:legal, shared:legal, shared:finance]\n\
         tags: [db, rust]\n\
         supersedes: decisions/old.md\n\
         effective_from: 2026-11-01\n\
         ---\n\
         CONFIDENTIAL BODY TEXT\n",
    )
    .unwrap();

    dir
}

/// Runs `scan --json` and returns the parsed object.
fn scan_json(root: &Path, extra: &[&str]) -> Value {
    let mut command = janus();
    command.arg("--vault").arg(root).arg("scan").arg("--json");
    command.args(extra);

    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn scan_json_is_a_versioned_report_with_one_entry_per_document() {
    let vault = sample_vault();
    let report = scan_json(vault.path(), &[]);

    assert_eq!(report["api"], 1);
    assert_eq!(report["corpus_id"], "main");
    assert!(
        report["vault"].as_str().unwrap().starts_with('/'),
        "vault must be absolute: {}",
        report["vault"]
    );
    assert!(report["config"].as_str().unwrap().ends_with("janus.toml"));

    let docs = report["docs"].as_array().unwrap();
    assert_eq!(docs.len(), 2, "{report}");
    let paths: Vec<&str> = docs
        .iter()
        .map(|doc| doc["rel_path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["decisions/adr-1.md", "notes.md"],
        "sorted by rel_path"
    );

    for doc in docs {
        assert!(doc["date_source"].is_string(), "{doc}");
        assert!(doc["date"].is_string(), "{doc}");
        assert!(doc["type"].is_string(), "{doc}");
        assert!(doc["status"].is_string(), "{doc}");
        assert!(doc["content_hash"].as_str().unwrap().len() == 64, "{doc}");
    }

    // Every skip rule is present even at zero, so consumers never have to guess.
    let skipped = report["skipped"].as_object().unwrap();
    assert_eq!(skipped.len(), 7);
    assert_eq!(skipped["config_exclude"], 1, "archive/private/secret.md");
    assert_eq!(skipped["hidden"], 1, ".hidden.md");
    // janus.toml and decisions/draft.txt are the two non-Markdown files.
    assert_eq!(skipped["not_markdown"], 2);
}

#[test]
fn scan_json_reports_resolved_metadata_not_raw_frontmatter() {
    let vault = metadata_vault();
    let report = scan_json(vault.path(), &[]);
    let doc = &report["docs"][0];

    assert_eq!(doc["type"], "decision", "alias resolved");
    assert_eq!(doc["status"], "draft");
    assert_eq!(doc["date"], "2024-11-02");
    assert_eq!(doc["date_source"], "filename");
    assert_eq!(doc["title"], "Postgres cache");
    assert_eq!(doc["scope"], "org:acme", "normalised");
    assert_eq!(
        doc["shared_with"],
        serde_json::json!(["shared:legal", "shared:finance"])
    );
    assert_eq!(doc["tags"], serde_json::json!(["db", "rust"]));
    assert_eq!(doc["author"], "claude-opus");
    assert_eq!(doc["doc_version"], "2.0");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["effective_from"], "2026-11-01");
    assert_eq!(doc["effective_to"], Value::Null);
    assert_eq!(doc["supersedes"], serde_json::json!(["decisions/old.md"]));
    assert_eq!(doc["timeless"], true, "decision is timeless by default");
    assert_eq!(doc["has_frontmatter"], true);
    assert!(doc["mtime"].as_str().unwrap().ends_with('Z'), "{doc}");
}

#[test]
fn scan_json_never_contains_document_content() {
    let vault = metadata_vault();
    let report = scan_json(vault.path(), &[]);
    let doc = &report["docs"][0];

    assert!(doc.get("body").is_none(), "{doc}");
    assert!(doc.get("raw_frontmatter").is_none(), "{doc}");

    let raw = serde_json::to_string(&report).unwrap();
    assert!(!raw.contains("CONFIDENTIAL BODY TEXT"), "{raw}");
}

#[test]
fn scan_json_verbose_adds_the_per_entry_skip_list() {
    let vault = sample_vault();

    let terse = scan_json(vault.path(), &[]);
    assert!(terse.get("skipped_entries").is_none(), "{terse}");

    let verbose = scan_json(vault.path(), &["--verbose"]);
    let entries = verbose["skipped_entries"].as_array().unwrap();
    let reasons: Vec<&str> = entries
        .iter()
        .map(|entry| entry["reason"].as_str().unwrap())
        .collect();

    assert!(reasons.contains(&"config_exclude"), "{verbose}");
    assert!(reasons.contains(&"not_markdown"), "{verbose}");
    assert!(
        entries
            .iter()
            .any(|entry| entry["rel_path"] == "archive/private/secret.md"),
        "{verbose}"
    );
}

#[test]
fn scan_json_is_deterministic_between_runs() {
    let vault = sample_vault();

    let mut first = scan_json(vault.path(), &[]);
    let mut second = scan_json(vault.path(), &[]);

    // The only field that may differ is the wall clock.
    first.as_object_mut().unwrap().remove("taken_ms");
    second.as_object_mut().unwrap().remove("taken_ms");

    assert_eq!(first, second);
}

#[test]
fn scan_json_reports_a_non_utf8_file_as_a_read_skip() {
    let vault = sample_vault();
    fs::write(vault.path().join("bad.md"), [0xff, 0xfe, 0x00]).unwrap();

    let report = scan_json(vault.path(), &[]);

    let skips = report["read_skips"].as_array().unwrap();
    assert_eq!(skips.len(), 1, "{report}");
    assert_eq!(skips[0]["rel_path"], "bad.md");
    assert_eq!(skips[0]["reason"], "non_utf8_content");
    assert!(skips[0]["message"].is_string());

    // It is not silently counted as a document either.
    assert_eq!(report["docs"].as_array().unwrap().len(), 2);

    // And the human report says so in one line.
    janus()
        .arg("--vault")
        .arg(vault.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("unreadable: 1 non_utf8_content"));
}

#[test]
fn scan_json_stdout_stays_parseable_when_config_warns() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\n\n[authority]\nweird = 0.0\n",
    )
    .unwrap();

    // Parsing happens inside `scan_json`, so a stray warning line would panic.
    let report = scan_json(dir.path(), &[]);
    assert_eq!(report["docs"].as_array().unwrap().len(), 0);

    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("scan")
        .arg("--json")
        .assert()
        .success()
        .stderr(predicate::str::contains("warning:"));
}

#[test]
fn scan_json_errors_stay_on_stderr_with_empty_stdout() {
    // The CLI's error contract is `error: …` on stderr and nothing on stdout,
    // so a consumer can tell "no output" from a report with zero documents.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".janus")).unwrap();

    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("scan")
        .arg("--json")
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("janus.toml"));
}

#[test]
fn scan_human_output_shows_the_resolved_metadata_table() {
    let vault = metadata_vault();

    janus()
        .arg("--vault")
        .arg(vault.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("date (source)"))
        .stdout(predicate::str::contains("decision"))
        .stdout(predicate::str::contains("draft"))
        .stdout(predicate::str::contains("2024-11-02 (filename)"));
}

#[test]
fn scan_rejects_an_unknown_flag_with_the_usage_exit_code() {
    let vault = sample_vault();

    janus()
        .arg("--vault")
        .arg(vault.path())
        .arg("scan")
        .arg("--nope")
        .assert()
        .code(2);
}

#[test]
fn scan_json_reports_a_derived_identity_per_document() {
    let vault = metadata_vault();
    let report = scan_json(vault.path(), &[]);
    let doc = &report["docs"][0];

    // Unique title + filename date, no declared id: the third rung.
    assert_eq!(doc["id_source"], "title_date");
    assert_eq!(doc["id"].as_str().unwrap().len(), 32, "{doc}");

    let sample = sample_vault();
    let report = scan_json(sample.path(), &[]);
    let docs = report["docs"].as_array().unwrap();
    let ids: Vec<&str> = docs.iter().map(|doc| doc["id"].as_str().unwrap()).collect();

    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "distinct documents, distinct ids");
    for doc in docs {
        let source = doc["id_source"].as_str().unwrap();
        assert!(
            [
                "declared_id",
                "content_hash",
                "title_date",
                "title_version",
                "title",
                "rel_path"
            ]
            .contains(&source),
            "unexpected id_source: {source}"
        );
        assert_eq!(doc["id"].as_str().unwrap().len(), 32);
    }
}
