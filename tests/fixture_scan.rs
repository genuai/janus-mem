//! The committed, deliberately ugly fixture vault must parse into a known set of
//! documents, with every §10 case visible in `scan --json`.

mod common;

use common::{LARGE_REL, NON_UTF8_REL, fixture_vault, janus, scan_json};
use serde_json::{Value, json};

/// The document with this vault-relative path.
fn doc<'a>(report: &'a Value, rel_path: &str) -> &'a Value {
    report["docs"]
        .as_array()
        .expect("docs is an array")
        .iter()
        .find(|document| document["rel_path"] == rel_path)
        .unwrap_or_else(|| panic!("no document at {rel_path}:\n{report}"))
}

fn rel_paths(report: &Value) -> Vec<&str> {
    report["docs"]
        .as_array()
        .expect("docs is an array")
        .iter()
        .map(|document| document["rel_path"].as_str().expect("rel_path is a string"))
        .collect()
}

#[test]
fn fixture_parses_every_ugly_case_into_the_expected_documents() {
    let vault = fixture_vault();
    let report = scan_json(vault.path(), &[]);

    assert_eq!(
        rel_paths(&report),
        [
            "2025-01-05-corpus-notes.md",
            "big/2025-03-06-large.md",
            "decisions/2024-11-02-postgres-cache.md",
            "decisions/2024-12-01-redis-cache.md",
            "decisions/2025-02-01-cache-revisited.md",
            "decisions/2025-03-01-broken-link.md",
            "notes/2025-03-05-unknown-type.md",
            "notes/2025-03-07-no-frontmatter.md",
            "notes/anchor-alias.md",
            "notes/block-sequence.md",
            "notes/bom-note.md",
            "notes/flow-mapping.md",
            "notes/multiline-scalar.md",
            "notes/quoted-scalars.md",
            "notes/undated.md",
            "notes/yaml-comments.md",
            "notes/überlegungen zu cache.md",
            "specs/body-starts-with-rule.md",
            "specs/crlf-spec.md",
        ],
        "sorted by rel_path; generated large/non-UTF-8 files included:\n{report}"
    );

    // A BOM must not leak into the title or the frontmatter fence detection.
    let bom = doc(&report, "notes/bom-note.md");
    assert_eq!(bom["has_frontmatter"], true, "{bom}");
    assert_eq!(bom["title"], "BOM note", "{bom}");
    assert_eq!(bom["date"], "2025-03-02", "{bom}");
    assert_eq!(bom["date_source"], "frontmatter", "{bom}");

    // CRLF bytes survive, aliased keys resolve, and the specs/ path hint types
    // the document because it declares no type.
    let crlf = doc(&report, "specs/crlf-spec.md");
    assert_eq!(crlf["title"], "CRLF line endings", "name alias: {crlf}");
    assert_eq!(crlf["type"], "spec", "specs/ path hint: {crlf}");
    assert_eq!(crlf["date"], "2025-02-14", "created alias: {crlf}");
    assert_eq!(crlf["date_source"], "frontmatter", "{crlf}");
    assert_eq!(crlf["tags"], json!(["line-endings", "fixtures"]), "{crlf}");
    assert_eq!(crlf["author"], "fixture-generator", "{crlf}");

    // No frontmatter at all: the filename carries the date.
    let bare = doc(&report, "notes/2025-03-07-no-frontmatter.md");
    assert_eq!(bare["has_frontmatter"], false, "{bare}");
    assert_eq!(bare["date"], "2025-03-07", "{bare}");
    assert_eq!(bare["date_source"], "filename", "{bare}");
    assert_eq!(bare["title"], "2025-03-07-no-frontmatter", "{bare}");

    // The one deliberate undated document. Its declared id keeps identity
    // stable, and no checkout time is invented as its date.
    let undated = doc(&report, "notes/undated.md");
    assert_eq!(undated["date_source"], "absent", "{undated}");
    assert_eq!(undated["id_source"], "declared_id", "{undated}");
    assert!(undated["date"].is_null(), "{undated}");

    // Comments in YAML are ignored, not treated as part of the values.
    let commented = doc(&report, "notes/yaml-comments.md");
    assert_eq!(
        commented["tags"],
        json!(["yaml", "comments"]),
        "{commented}"
    );
    assert_eq!(commented["date"], "2025-03-03", "{commented}");
    assert_eq!(commented["status"], "active", "{commented}");

    // Space + non-ASCII filename; title falls back to the filename stem.
    let unicode = doc(&report, "notes/überlegungen zu cache.md");
    assert_eq!(unicode["title"], "überlegungen zu cache", "{unicode}");
    assert_eq!(unicode["date"], "2025-03-04", "{unicode}");
    assert_eq!(unicode["date_source"], "frontmatter", "{unicode}");

    // An open-registry type keeps its name; an unknown status becomes `unknown`.
    let unknown = doc(&report, "notes/2025-03-05-unknown-type.md");
    assert_eq!(unknown["type"], "speculative", "{unknown}");
    assert_eq!(unknown["status"], "unknown", "{unknown}");
    let note_codes: Vec<&str> = unknown["notes"]
        .as_array()
        .expect("notes is an array")
        .iter()
        .map(|note| note["code"].as_str().expect("note code"))
        .collect();
    assert!(note_codes.contains(&"unknown_type"), "{unknown}");
    assert!(note_codes.contains(&"unknown_status"), "{unknown}");

    // The supersession cycle A -> B -> C -> A is parsed and echoed, never
    // validated in M1.
    let cycle_a = doc(&report, "decisions/2024-11-02-postgres-cache.md");
    let cycle_b = doc(&report, "decisions/2024-12-01-redis-cache.md");
    let cycle_c = doc(&report, "decisions/2025-02-01-cache-revisited.md");
    assert_eq!(
        cycle_a["supersedes"],
        json!(["decisions/2024-12-01-redis-cache.md"]),
        "{cycle_a}"
    );
    assert_eq!(
        cycle_b["supersedes"],
        json!(["decisions/2025-02-01-cache-revisited.md"]),
        "{cycle_b}"
    );
    assert_eq!(
        cycle_c["supersedes"],
        json!(["decisions/2024-11-02-postgres-cache.md"]),
        "{cycle_c}"
    );
    assert_eq!(cycle_c["type"], "decision", "adr alias: {cycle_c}");

    // A broken link is echoed verbatim; link validation is M5.
    let broken = doc(&report, "decisions/2025-03-01-broken-link.md");
    assert_eq!(
        broken["supersedes"],
        json!(["decisions/2025-04-01-never-written.md"]),
        "{broken}"
    );

    // The generated document is really present and really large.
    let large = doc(&report, LARGE_REL);
    let size = large["size"].as_u64().expect("size is a number");
    assert!(size > 2_000_000, "large fixture is {size} bytes");
    assert_eq!(large["date"], "2025-03-06", "{large}");

    // Excluded and hidden files never become documents.
    for excluded in [
        "archive/private/secret.md",
        "notes/plan.draft.md",
        "target/build.md",
        ".scratch.md",
        "scratch/ignored.md",
        NON_UTF8_REL,
    ] {
        assert!(
            !rel_paths(&report).contains(&excluded),
            "{excluded} must not be a document:\n{report}"
        );
    }

    // The non-UTF-8 file is admitted by the walk and skipped by the reader, with
    // its own vocabulary rather than being folded into a walk rule (J-4).
    let read_skips = report["read_skips"].as_array().expect("read_skips array");
    assert_eq!(read_skips.len(), 1, "{report}");
    assert_eq!(read_skips[0]["rel_path"], NON_UTF8_REL, "{report}");
    assert_eq!(read_skips[0]["reason"], "non_utf8_content", "{report}");

    // Every skip rule is visible: target/ (always), the three dot-files
    // (hidden), the two config exclusions, and janus.toml (not markdown).
    assert_eq!(
        skip_row(&report, "always_excluded")["entries"],
        1,
        "{report}"
    );
    assert_eq!(skip_row(&report, "hidden")["entries"], 3, "{report}");
    assert_eq!(
        skip_row(&report, "config_exclude")["entries"],
        2,
        "{report}"
    );
    assert_eq!(skip_row(&report, "not_markdown")["entries"], 1, "{report}");
    assert_eq!(
        report["ignored_by_git"], 1,
        "scratch/ignored.md is gitignored:\n{report}"
    );
    assert_eq!(report["errors"], json!([]), "{report}");
}

#[test]
fn every_skip_rule_is_reported_in_a_fixed_order() {
    let vault = fixture_vault();
    let report = scan_json(vault.path(), &[]);

    let reasons: Vec<&str> = report["walk"]["per_reason"]
        .as_array()
        .expect("per_reason list")
        .iter()
        .map(|row| row["reason"].as_str().expect("reason"))
        .collect();

    assert_eq!(
        reasons,
        [
            "always_excluded",
            "hidden",
            "config_exclude",
            "not_markdown",
            "symlinked_directory",
            "symlink_escape",
            "non_utf8_path",
        ],
        "all seven reasons, always, in report order:\n{report}"
    );
}

#[test]
fn the_skip_report_reconciles_with_the_files_on_disk() {
    let vault = fixture_vault();
    let report = scan_json(vault.path(), &[]);

    let files = report["walk"]["files"].as_u64().expect("walk.files");
    let rows = report["walk"]["per_reason"]
        .as_array()
        .expect("per_reason list");
    let sum = |field: &str| -> u64 {
        rows.iter()
            .map(|row| row[field].as_u64().expect("a count"))
            .sum()
    };

    // Hand-derived: 20 markdown files are read (19 documents plus the one
    // non-UTF-8 file) and 7 paths are refused: `node_modules/` as one pruned
    // directory, 3 dot-entries (two of them not markdown), 2 config exclusions,
    // and `janus.toml`.
    assert_eq!(files, 20, "{report}");
    assert_eq!(sum("entries"), 7, "{report}");
    assert_eq!(sum("pruned_directories"), 1, "{report}");
    assert_eq!(report["walk"]["entries"], 7, "{report}");
    assert_eq!(report["walk"]["directories_pruned"], 1, "{report}");
    // Every path the walk yielded is admitted once or skipped once.
    assert_eq!(files + sum("entries"), 27, "{report}");

    // The admitted files become documents or read skips; nothing else.
    let documents = report["docs"].as_array().expect("docs array").len() as u64;
    let read_skips = report["read_skips"].as_array().expect("read_skips").len() as u64;
    assert_eq!(documents, common::FIXTURE_DOCUMENTS as u64, "{report}");
    assert_eq!(files, documents + read_skips, "{report}");

    // Outside the invariant, by rule: `node_modules/generated.md` sits under a
    // pruned directory and `scratch/ignored.md` is hidden by `.gitignore`.
    // Neither is visited, and neither is counted as a walk entry.
    assert_eq!(report["ignored_by_git"], 1, "{report}");
}

#[test]
fn the_skip_map_keeps_the_published_shape() {
    let vault = fixture_vault();
    let report = scan_json(vault.path(), &[]);

    // A consumer of v0.1.0 reads `report.skipped.hidden` as a number. Keep that
    // promise: same type, same keys, same values. The new counts live under
    // `walk`, so `api` stays 1.
    let skipped = report["skipped"].as_object().expect("skipped map");
    assert_eq!(skipped.len(), 7, "{report}");
    assert_eq!(skipped["always_excluded"], 1, "{report}");
    assert_eq!(skipped["hidden"], 3, "{report}");
    assert_eq!(skipped["config_exclude"], 2, "{report}");
    assert_eq!(skipped["not_markdown"], 1, "{report}");
    for zero in ["symlinked_directory", "symlink_escape", "non_utf8_path"] {
        assert_eq!(skipped[zero], 0, "{zero}: {report}");
    }
    assert_eq!(report["api"], 1, "{report}");

    // The map and the new rows cannot disagree.
    for row in report["walk"]["per_reason"]
        .as_array()
        .expect("per_reason list")
    {
        let reason = row["reason"].as_str().expect("reason");
        assert_eq!(skipped[reason], row["entries"], "{reason}: {report}");
    }
}

/// One rule's row in `walk.per_reason`.
fn skip_row<'a>(report: &'a Value, reason: &str) -> &'a Value {
    report["walk"]["per_reason"]
        .as_array()
        .expect("per_reason list")
        .iter()
        .find(|row| row["reason"] == reason)
        .unwrap_or_else(|| panic!("no {reason} row:\n{report}"))
}

#[test]
fn every_document_reports_a_date_source() {
    let vault = fixture_vault();
    let report = scan_json(vault.path(), &[]);
    let documents = report["docs"].as_array().expect("docs array");

    let sources: Vec<&str> = documents
        .iter()
        .map(|document| {
            document["date_source"]
                .as_str()
                .expect("every document reports date_source")
        })
        .collect();

    assert_eq!(
        documents.len(),
        common::FIXTURE_DOCUMENTS,
        "fixture document count changed"
    );
    assert_eq!(
        sources.iter().filter(|source| **source == "absent").count(),
        1,
        "exactly one document deliberately has no date: {sources:?}"
    );
    assert!(
        sources.iter().all(|source| [
            "frontmatter",
            "filename",
            "filename_month",
            "filename_year",
            "folder",
            "folder_month",
            "folder_year",
            "absent",
        ]
        .contains(source)),
        "unexpected date source: {sources:?}"
    );

    for document in documents {
        let id = document["id"].as_str().expect("id is a string");
        assert_eq!(id.len(), 32, "id is 16 bytes as hex: {document}");
        assert!(document["id_source"].is_string(), "{document}");
    }
}

#[test]
fn two_scans_of_the_fixture_differ_only_in_taken_ms() {
    let vault = fixture_vault();

    let mut first = scan_json(vault.path(), &[]);
    let mut second = scan_json(vault.path(), &[]);

    first
        .as_object_mut()
        .expect("report object")
        .remove("taken_ms");
    second
        .as_object_mut()
        .expect("report object")
        .remove("taken_ms");

    assert_eq!(first, second);
}

#[test]
fn init_then_scan_works_in_a_fresh_directory() {
    let dir = tempfile::tempdir().expect("tempdir");

    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("init")
        .assert()
        .success();

    assert!(dir.path().join("janus.toml").is_file());
    assert!(dir.path().join(".janus").is_dir());

    let report = scan_json(dir.path(), &[]);
    assert_eq!(report["api"], 1, "{report}");
    assert_eq!(report["docs"], json!([]), "{report}");
    assert_eq!(
        skip_row(&report, "always_excluded")["entries"],
        1,
        "the .janus/ sidecar is never vault content (§8.6):\n{report}"
    );
    assert_eq!(
        report["walk"]["directories_pruned"], 1,
        "the sidecar is pruned, not enumerated:\n{report}"
    );
}

#[test]
fn case_3_1_the_metadata_counts_reconcile_on_the_fixture() {
    let vault = fixture_vault();
    let report = scan_json(vault.path(), &[]);
    let metadata = &report["metadata"];

    // Hand-derived from the fixture's 19 documents: 12 take a frontmatter date,
    // 6 take one from the file name, and `notes/undated.md` has none. The
    // default type is `note`, so 12 notes are not typed and the 4 decisions, 2
    // path-typed specs and 1 `speculative` are.
    assert_eq!(metadata["typed"], 7, "{metadata}");
    assert_eq!(metadata["dated"], 18, "{metadata}");
    assert_eq!(metadata["undated"], 1, "{metadata}");
    assert_eq!(metadata["by_source"]["frontmatter"], 12, "{metadata}");
    assert_eq!(metadata["by_source"]["filename"], 6, "{metadata}");
    assert_eq!(metadata["by_source"]["absent"], 1, "{metadata}");

    let documents = u64::try_from(report["docs"].as_array().expect("docs").len()).expect("small");
    assert_eq!(
        metadata["dated"].as_u64().expect("dated") + metadata["undated"].as_u64().expect("undated"),
        documents
    );
}
