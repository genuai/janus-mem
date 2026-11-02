//! `insta` golden over `search --json` on the committed fixture vault (§10).
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

#[test]
fn search_json_matches_the_golden() {
    let vault = fixture_vault();
    index(vault.path());

    let text = run_ok(
        vault.path(),
        &[
            "search", "cache", "--intent", "history", "--limit", "3", "--json",
        ],
    );
    let mut report: Value = serde_json::from_str(&text).expect("stdout is JSON");

    report["taken_ms"] = json!(0);
    report["vault"] = json!("<vault>");
    for hit in report["results"].as_array_mut().expect("results") {
        hit["path"] = json!(format!("<vault>/{}", hit["rel_path"].as_str().unwrap()));
    }

    let json = serde_json::to_string_pretty(&report).expect("the report serializes");
    insta::assert_snapshot!("search", json);
}
