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
            "notes/bom-note.md",
            "notes/undated.md",
            "notes/yaml-comments.md",
            "notes/überlegungen zu cache.md",
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

    // The one deliberate mtime-sourced document. Its declared id keeps identity
    // stable even though its date is the checkout time.
    let undated = doc(&report, "notes/undated.md");
    assert_eq!(undated["date_source"], "mtime", "{undated}");
    assert_eq!(undated["id_source"], "declared_id", "{undated}");
    assert!(undated["date"].is_string(), "{undated}");

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
    let skipped = report["skipped"].as_object().expect("skipped map");
    assert_eq!(skipped["always_excluded"], 1, "{report}");
    assert_eq!(skipped["hidden"], 3, "{report}");
    assert_eq!(skipped["config_exclude"], 2, "{report}");
    assert_eq!(skipped["not_markdown"], 1, "{report}");
    assert_eq!(
        report["ignored_by_git"], 1,
        "scratch/ignored.md is gitignored:\n{report}"
    );
    assert_eq!(report["errors"], json!([]), "{report}");
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

    assert_eq!(documents.len(), 13, "{report}");
    assert_eq!(
        sources.iter().filter(|source| **source == "mtime").count(),
        1,
        "exactly one document deliberately falls back to mtime: {sources:?}"
    );
    assert!(
        sources
            .iter()
            .all(|source| ["frontmatter", "filename", "mtime"].contains(source)),
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
        report["skipped"]["always_excluded"], 1,
        "the .janus/ sidecar is never vault content (§8.6):\n{report}"
    );
}
