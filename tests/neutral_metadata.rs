//! An unknown type and an unknown status are neutral, not fail-low
//! (M4C §3.3, C-6).
//!
//! The two documents carry the same body and titles of equal length, so every
//! scoring input except authority is equal. Only the metadata can separate them.

mod common;

use std::fs;

use common::{fixture_vault, index, result_paths, scan_json, search_json};
use janus_mem::config::UNKNOWN_TYPE_AUTHORITY;

/// The document with a type and status the config does not know.
const UNKNOWN: &str = "notes/unknown-metadata.md";

/// A plain note: known type `note` at 0.90, known status `active` at 1.00.
const PLAIN: &str = "notes/plain-note.md";

/// A vault with one unknown-metadata document and one plain note.
fn metadata_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"metadata\"\n",
    )
    .expect("write config");
    fs::create_dir_all(root.join("notes")).expect("create notes directory");

    let docs = [
        (
            UNKNOWN,
            "title: Quorum alpha\ntype: personal-operating-plan\nstatus: unknown\ndate: 2024-06-01",
        ),
        (
            PLAIN,
            "title: Quorum beta\ntype: note\nstatus: active\ndate: 2024-06-01",
        ),
    ];
    for (path, front) in docs {
        fs::write(
            root.join(path),
            format!("---\n{front}\n---\n\n# Quorum\n\nquorum quorum quorum.\n"),
        )
        .expect("write document");
    }
    dir
}

/// Finds one scored hit by its path.
fn hit<'a>(report: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|hit| hit["rel_path"] == path)
        .unwrap_or_else(|| panic!("{path} is absent from {report}"))
}

#[test]
fn an_unknown_type_and_status_rank_as_their_relevance_says() {
    let vault = metadata_vault();
    index(vault.path());

    let report = search_json(vault.path(), &["quorum"]);
    let unknown = hit(&report, UNKNOWN);
    let plain = hit(&report, PLAIN);

    // Same body, equal title length, same date: relevance, decay and the title
    // boost cancel. Only authority is left.
    let relevance =
        |hit: &serde_json::Value| hit["scores"]["relevance"].as_f64().expect("relevance");
    assert!(
        (relevance(unknown) - relevance(plain)).abs() < 1e-12,
        "the scores must differ only by metadata: {report}"
    );
    assert_eq!(
        unknown["scores"]["decay"], plain["scores"]["decay"],
        "the dates and half-lives are equal: {report}"
    );

    // Unknown type and status are neutral: 1.00 × 1.00 = 1.00.
    // A plain note is 0.90 × 1.00 = 0.90.
    assert_eq!(
        unknown["scores"]["type_authority"],
        serde_json::json!(UNKNOWN_TYPE_AUTHORITY)
    );
    assert_eq!(unknown["scores"]["status_factor"], serde_json::json!(1.0));
    assert_eq!(plain["scores"]["type_authority"], serde_json::json!(0.90));
    assert_eq!(plain["scores"]["status_factor"], serde_json::json!(1.0));

    // The unknown document therefore wins by 1.00 / 0.90 = 1.111…, and must
    // rank first. Before the change the factor was 0.80 × 0.80 = 0.64.
    assert_eq!(result_paths(&report), vec![UNKNOWN, PLAIN]);

    let final_ratio = unknown["scores"]["final"].as_f64().expect("final")
        / plain["scores"]["final"].as_f64().expect("final");
    assert!(
        (final_ratio - 1.0 / 0.90).abs() < 1e-12,
        "final ratio {final_ratio} must be the authority ratio: {report}"
    );

    for hit in [unknown, plain] {
        let scores = &hit["scores"];
        let expected = scores["relevance"].as_f64().expect("relevance")
            * scores["authority"].as_f64().expect("authority")
            * scores["decay"].as_f64().expect("decay");
        assert!(
            (scores["final"].as_f64().expect("final") - expected).abs() < 1e-12,
            "final = relevance × authority × decay: {hit}"
        );
    }
}

#[test]
fn both_unknown_metadata_notes_state_the_rule_the_code_uses() {
    let vault = metadata_vault();
    let report = scan_json(vault.path(), &[]);
    let document = report["docs"]
        .as_array()
        .expect("docs")
        .iter()
        .find(|doc| doc["rel_path"] == UNKNOWN)
        .expect("the unknown-metadata document");

    let notes = document["notes"].as_array().expect("notes");
    let message = |code: &str| {
        notes
            .iter()
            .find(|note| note["code"] == code)
            .unwrap_or_else(|| panic!("no {code} note in {document}"))["message"]
            .as_str()
            .expect("message")
            .to_owned()
    };

    let type_note = message("unknown_type");
    assert!(
        type_note.contains(&format!("{UNKNOWN_TYPE_AUTHORITY:.2}")),
        "the type note must print the configured weight: {type_note}"
    );

    // The status factor is a table cell in `score.rs`, which `document.rs`
    // cannot import. The note states the rule instead of a number.
    let status_note = message("unknown_status");
    assert!(
        status_note.contains("treated as active for inclusion"),
        "the status note must state the inclusion rule: {status_note}"
    );
    assert!(
        !status_note.contains("0.8"),
        "the status note must not repeat the old penalty: {status_note}"
    );
}

/// The fixture vault keeps both notes, so its goldens move with this change.
#[test]
fn the_fixture_unknown_document_still_reports_both_notes() {
    let vault = fixture_vault();
    let report = scan_json(vault.path(), &[]);
    let document = report["docs"]
        .as_array()
        .expect("docs")
        .iter()
        .find(|doc| doc["rel_path"] == "notes/2025-03-05-unknown-type.md")
        .expect("the fixture document");

    let codes: Vec<&str> = document["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|note| note["code"].as_str().expect("code"))
        .collect();
    assert_eq!(codes, ["unknown_type", "unknown_status"]);
}
