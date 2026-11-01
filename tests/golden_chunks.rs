//! `insta` golden over `chunks --json` on the committed fixture vault (§10).
//!
//! A stronger golden than `scan`'s: chunk ids are content-addressed, spans are
//! relative to the document body, documents are in `rel_path` order and the DTO
//! carries no clock and no metadata, so **nothing** in this report depends on the
//! machine, the checkout or the run. The one exception is the absolute corpus
//! root, which the fixture copy makes unpredictable; that single field is
//! redacted, deliberately nothing wider.
//!
//! `--limit 3` keeps the snapshot readable: the generated 2 MB document is
//! chunked in full (that is the point of it) and then reduced to three rows with
//! `truncated: true`.

mod common;

use common::{chunks_json, fixture_vault};
use serde_json::json;

#[test]
fn chunks_json_matches_the_golden() {
    let vault = fixture_vault();
    let mut report = chunks_json(vault.path(), &["--limit", "3"]);

    report["vault"] = json!("<vault>");

    let json = serde_json::to_string_pretty(&report).expect("the report serializes");
    insta::assert_snapshot!("chunks", json);
}
