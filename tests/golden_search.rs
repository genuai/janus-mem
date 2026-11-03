//! `insta` goldens over `search --json` on the committed fixture vault (§10).
//!
//! Three snapshots, because one file cannot be "unchanged from M3", "regenerated
//! for hybrid" and "the ranking before the title boost" at once (§3.3, review
//! finding 6):
//!
//! * `search_no_embeddings` is the α = 0 reference, taken with
//!   `--no-embeddings`. The title boost multiplies `relevance` and `final`, so
//!   this file moved in M4C: the top-level `alpha` and `degraded` fields are the
//!   M4 additions, and `scores.title_boost` is the M4C addition.
//! * `search_hybrid` is the default α = 0.5 path (V-27), where `scores.semantic` is
//!   non-zero and `relevance == (α·semantic + (1−α)·lexical) · title_boost` is not
//!   degenerate.
//! * `search_pre_title_boost` is the same query with `[scoring] title_boost = 0.0`.
//!   It is the ranking before M4C, stored as a separate file, and it proves the
//!   boost reorders nothing at weight zero.
//!
//! `--intent history` is part of the golden's contract, not an accident: it
//! disables decay, so every score is a function of the corpus alone. With the
//! default intent the numbers would move with the calendar, and a golden that
//! has to redact the scores is not a golden.
//!
//! Redaction is the corpus root (the fixture is copied to a temporary
//! directory) and `taken_ms`; deliberately nothing wider. Spans, chunk ids,
//! breadcrumbs, dates and the whole `Breakdown` stay in the snapshot.

mod common;

use common::{fixture_vault, index, result_paths, run_ok};
use serde_json::{Value, json};

/// Redacts only the corpus root and the wall-clock measurement, then serializes.
fn snapshot_json(text: &str) -> String {
    let mut report: Value = serde_json::from_str(text).expect("stdout is JSON");

    report["taken_ms"] = json!(0);
    report["vault"] = json!("<vault>");
    for hit in report["results"].as_array_mut().expect("results") {
        hit["path"] = json!(format!("<vault>/{}", hit["rel_path"].as_str().unwrap()));
    }

    serde_json::to_string_pretty(&report).expect("the report serializes")
}

/// Removes the M4C key, so a weight-zero report equals the stored pre-boost one.
fn strip_title_boost(text: String) -> String {
    let mut report: Value = serde_json::from_str(&text).expect("the snapshot is JSON");
    for hit in report["results"].as_array_mut().expect("results") {
        hit["scores"]
            .as_object_mut()
            .expect("scores is an object")
            .remove("title_boost");
    }
    serde_json::to_string_pretty(&report).expect("the report serializes")
}

#[test]
fn search_without_embeddings_matches_the_m3_golden() {
    let vault = fixture_vault();
    index(vault.path());

    let text = run_ok(
        vault.path(),
        &[
            "search",
            "cache",
            "--intent",
            "history",
            "--limit",
            "3",
            "--no-embeddings",
            "--json",
        ],
    );
    let report: Value = serde_json::from_str(&text).expect("stdout is JSON");
    assert_eq!(report["alpha"], json!(0.0), "{report}");
    assert_eq!(
        report["degraded"],
        json!(["--no-embeddings: lexical only"]),
        "{report}"
    );
    for hit in report["results"].as_array().expect("results") {
        let lexical = hit["scores"]["lexical"].as_f64().expect("lexical");
        let relevance = hit["scores"]["relevance"].as_f64().expect("relevance");
        let boost = hit["scores"]["title_boost"].as_f64().expect("title_boost");
        assert_eq!(hit["scores"]["semantic"], json!(0.0), "{hit}");
        assert!(
            (relevance - lexical * boost).abs() < 1e-12,
            "α = 0 must give relevance = lexical · title_boost: {hit}"
        );
    }

    insta::assert_snapshot!("search_no_embeddings", snapshot_json(&text));
}

#[test]
fn search_hybrid_matches_the_golden_and_the_blend_identity() {
    let vault = fixture_vault();
    index(vault.path());

    let text = run_ok(
        vault.path(),
        &[
            "search", "cache", "--intent", "history", "--limit", "3", "--json",
        ],
    );
    let report: Value = serde_json::from_str(&text).expect("stdout is JSON");
    assert_eq!(report["alpha"], json!(0.5), "{report}");
    assert_eq!(report["degraded"], json!([]), "{report}");

    let mut semantic_seen = false;
    for hit in report["results"].as_array().expect("results") {
        let alpha = report["alpha"].as_f64().expect("alpha");
        let semantic = hit["scores"]["semantic"].as_f64().expect("semantic");
        let lexical = hit["scores"]["lexical"].as_f64().expect("lexical");
        let relevance = hit["scores"]["relevance"].as_f64().expect("relevance");
        let boost = hit["scores"]["title_boost"].as_f64().expect("title_boost");
        semantic_seen |= semantic > 0.0;
        let expected = (alpha * semantic + (1.0 - alpha) * lexical) * boost;
        assert!(
            (relevance - expected).abs() < 1e-12,
            "relevance {relevance} != ({alpha}·{semantic} + {}·{lexical}) · {boost} = {expected}",
            1.0 - alpha
        );
    }
    assert!(
        semantic_seen,
        "no hybrid hit carried a semantic signal: {report}"
    );

    insta::assert_snapshot!("search_hybrid", snapshot_json(&text));
}

#[test]
fn a_zero_title_boost_reproduces_the_pre_boost_snapshot() {
    let vault = fixture_vault();
    let config_path = vault.path().join("janus.toml");
    let base = std::fs::read_to_string(&config_path).expect("read the fixture config");
    std::fs::write(
        &config_path,
        format!("{base}\n[scoring]\ntitle_boost = 0.0\n"),
    )
    .expect("write the fixture config");
    index(vault.path());

    let args = [
        "search",
        "cache",
        "--intent",
        "history",
        "--limit",
        "3",
        "--no-embeddings",
        "--json",
    ];
    let unboosted = run_ok(vault.path(), &args);
    insta::assert_snapshot!(
        "search_pre_title_boost",
        strip_title_boost(snapshot_json(&unboosted))
    );

    // The order is a separate claim from the scores: weight zero must not move
    // a single result, and the boosted default must not either.
    std::fs::write(
        &config_path,
        format!("{base}\n[scoring]\ntitle_boost = 1.0\n"),
    )
    .expect("restore the weight");
    let boosted = run_ok(vault.path(), &args);
    assert_eq!(
        result_paths(&serde_json::from_str(&unboosted).expect("unboosted JSON")),
        result_paths(&serde_json::from_str(&boosted).expect("boosted JSON")),
        "the title boost must not reorder these three results"
    );
}
