//! `insta` goldens over `search --json` on the committed fixture vault (§10).
//!
//! Two snapshots, because one file cannot be both "unchanged from M3" and
//! "regenerated for hybrid" (§3.3, review finding 6):
//!
//! * `search_no_embeddings` is the α = 0 reference, taken with
//!   `--no-embeddings`. Its `results` array must stay exactly the M3 snapshot;
//!   the top-level `alpha` and `degraded` fields are the only additions.
//! * `search_hybrid` is the default α = 0.65 path, where `scores.semantic` is
//!   non-zero and `relevance == α·semantic + (1−α)·lexical` is not degenerate.
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

use common::{fixture_vault, index, run_ok};
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
        assert_eq!(hit["scores"]["semantic"], json!(0.0), "{hit}");
        assert_eq!(
            hit["scores"]["relevance"], hit["scores"]["lexical"],
            "α = 0 must leave relevance equal to lexical: {hit}"
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
    assert_eq!(report["alpha"], json!(0.65), "{report}");
    assert_eq!(report["degraded"], json!([]), "{report}");

    let mut semantic_seen = false;
    for hit in report["results"].as_array().expect("results") {
        let alpha = report["alpha"].as_f64().expect("alpha");
        let semantic = hit["scores"]["semantic"].as_f64().expect("semantic");
        let lexical = hit["scores"]["lexical"].as_f64().expect("lexical");
        let relevance = hit["scores"]["relevance"].as_f64().expect("relevance");
        semantic_seen |= semantic > 0.0;
        let expected = alpha * semantic + (1.0 - alpha) * lexical;
        assert!(
            (relevance - expected).abs() < 1e-12,
            "relevance {relevance} != {alpha}·{semantic} + {}·{lexical} = {expected}",
            1.0 - alpha
        );
    }
    assert!(
        semantic_seen,
        "no hybrid hit carried a semantic signal: {report}"
    );

    insta::assert_snapshot!("search_hybrid", snapshot_json(&text));
}
