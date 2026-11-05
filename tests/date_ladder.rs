//! The date ladder must not read the disk (§3.1, §3.3, §7).
//!
//! The ranking is a property of the vault, not of the machine that copied it.
//! The two-copy test proves that: two identical vaults, one with every mtime
//! moved, must return the same order and the same scores.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{copy_tree, fixture_vault, index, result_paths, scan_json, search_json};
use serde_json::Value;

/// Moves every file's mtime under `root` to `when`, recursively.
fn shift_mtimes(root: &Path, when: SystemTime) {
    for entry in fs::read_dir(root).expect("read directory") {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if entry.file_type().expect("file type").is_dir() {
            shift_mtimes(&path, when);
        } else {
            fs::File::options()
                .write(true)
                .open(&path)
                .expect("open file to change its mtime")
                .set_modified(when)
                .expect("set the mtime");
        }
    }
}

/// The rank order and each hit's final score, in one comparable value.
fn outcome(report: &Value) -> Vec<(String, f64)> {
    let paths = result_paths(report);
    let results = report["results"].as_array().expect("results array");
    assert_eq!(paths.len(), results.len());

    results
        .iter()
        .map(|hit| {
            (
                hit["rel_path"].as_str().expect("rel_path").to_owned(),
                hit["scores"]["final"].as_f64().expect("final score"),
            )
        })
        .collect()
}

#[test]
fn shifting_every_mtime_changes_no_order_and_no_score() {
    let first = fixture_vault();
    let second = fixture_vault();

    // 2001-01-01. Far enough from the fixture's authored dates that a
    // disk-derived date changes both the decay and the order.
    let moved = SystemTime::UNIX_EPOCH + Duration::from_secs(978_307_200);
    shift_mtimes(second.path(), moved);

    index(first.path());
    index(second.path());

    let args = ["frontmatter", "--limit", "50"];
    let first_report = search_json(first.path(), &args);
    let second_report = search_json(second.path(), &args);

    let first_outcome = outcome(&first_report);
    assert!(
        !first_outcome.is_empty(),
        "the query must match the undated fixture: {first_report}"
    );
    assert_eq!(
        first_outcome,
        outcome(&second_report),
        "a shifted mtime changed the result order or a score"
    );
}

/// The fixture copy must never carry a built index (§5 Phase 1, P1-6).
#[test]
fn the_fixture_copy_skips_a_built_index() {
    let source = tempfile::tempdir().expect("source tempdir");
    fs::write(source.path().join("note.md"), "x").expect("write a vault file");
    fs::create_dir(source.path().join(".janus")).expect("create a built index");
    fs::write(source.path().join(".janus/index.bin"), "stale").expect("write a stale index");

    let target = tempfile::tempdir().expect("target tempdir");
    copy_tree(source.path(), target.path()).expect("copy the vault");
    assert!(target.path().join("note.md").exists());
    assert!(!target.path().join(".janus").exists());

    // The real fixture copy behaves the same way.
    let fixture = fixture_vault();
    assert!(!fixture.path().join(".janus").exists());
}

/// Every fixture document keeps the identity it had before the date ladder moved
/// (§3.4). These ids were taken from the scan before this change.
#[test]
fn no_doc_id_changes_over_the_fixture_vault() {
    let vault = fixture_vault();
    let report = scan_json(vault.path(), &[]);
    let mut actual: Vec<(String, String)> = report["docs"]
        .as_array()
        .expect("docs array")
        .iter()
        .map(|document| {
            (
                document["rel_path"].as_str().expect("rel_path").to_owned(),
                document["id"].as_str().expect("id").to_owned(),
            )
        })
        .collect();
    actual.sort();

    let mut expected = vec![
        (
            "2025-01-05-corpus-notes.md",
            "2a4fe14a1122d5d726c6d12e5656e935",
        ),
        (
            "big/2025-03-06-large.md",
            "09d01188d4ad18caca871caaeb0618d2",
        ),
        (
            "decisions/2024-11-02-postgres-cache.md",
            "2e85ad7f373d6a46959b68701c2fba40",
        ),
        (
            "decisions/2024-12-01-redis-cache.md",
            "d97e653f4a2dd60ea5d426b27d260e63",
        ),
        (
            "decisions/2025-02-01-cache-revisited.md",
            "c64418894b25cfc133c800f916942b66",
        ),
        (
            "decisions/2025-03-01-broken-link.md",
            "820044991ccc5b759670c9a4ac7b0e34",
        ),
        (
            "notes/2025-03-05-unknown-type.md",
            "58348ba891d48e66b6266adb712e8e2b",
        ),
        (
            "notes/2025-03-07-no-frontmatter.md",
            "8280bfc9a69199ba7f2274ab53ad6085",
        ),
        ("notes/anchor-alias.md", "50c490e90f2f022c79837fbe24dcefc8"),
        (
            "notes/block-sequence.md",
            "58d5215272ba7e4c0896e918205167e3",
        ),
        ("notes/bom-note.md", "8ba81b1ad4f14148351aa6c5f08941e9"),
        ("notes/flow-mapping.md", "016a6dfc23b4ee2be1ff8b0f0b3a8921"),
        (
            "notes/multiline-scalar.md",
            "c9be92b7587d9a23fcbcb86efae8c3bd",
        ),
        (
            "notes/quoted-scalars.md",
            "5d134c0ff5ec1f4e5053ceaced41adea",
        ),
        ("notes/undated.md", "46a3c52228dd60c4e334e1f8ac92480b"),
        ("notes/yaml-comments.md", "7a24b1ff1a4963776b0f300fde68d4a8"),
        (
            "notes/überlegungen zu cache.md",
            "c880c35f324b0c0dd5f242f086ac77d3",
        ),
        (
            "specs/body-starts-with-rule.md",
            "52329f303e3430055fabcd6613dfa40e",
        ),
        ("specs/crlf-spec.md", "fe4582ebaf16653b9d6c5f91c214cadc"),
    ]
    .into_iter()
    .map(|(path, id)| (path.to_owned(), id.to_owned()))
    .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(actual, expected);
}

// ───────────────────── M5 P8: quarter dates and undated decay ─────────────────

use chrono::NaiveDate;
use janus_mem::clock::FixedClock;
use janus_mem::config::Config;
use janus_mem::search::{self, Options};

/// A vault written from literal files, so a P8 case owns its whole corpus.
fn written(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"p8\"\n[decay]\ntimeless_types = []\n",
    )
    .expect("write config");
    for (rel, text) in files {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create folder");
        }
        fs::write(path, text).expect("write document");
    }
    dir
}

/// The P8 clock: all P8 ages are measured from this date.
fn p8_clock() -> FixedClock {
    FixedClock(NaiveDate::from_ymd_opt(2025, 4, 1).expect("valid date"))
}

/// A `current` query over the whole corpus, with no embeddings.
fn p8_options(query: &str) -> Options {
    Options {
        query: query.to_owned(),
        intent: Some("current".to_owned()),
        scopes: Vec::new(),
        scope_only: false,
        limit: 20,
        types: Vec::new(),
        tags: Vec::new(),
        decay_half_life: None,
        include_superseded: false,
        as_of: None,
        no_rebuild: false,
        no_embeddings: true,
        embedding_reason: None,
        progress: false,
        json: false,
    }
}

/// Runs one query and returns `(rel_path, decay)` for every hit.
fn decays(root: &Path, options: &Options) -> Vec<(String, f64)> {
    let config = Config::load(root).expect("load config");
    let outcome = search::query(root, &config, &p8_clock(), None, options)
        .expect("the P8 corpus indexes and queries");
    outcome
        .hits
        .iter()
        .map(|hit| {
            (
                outcome.rows[outcome.selected[hit.doc_index]]
                    .rel_path
                    .clone(),
                hit.breakdown.decay,
            )
        })
        .collect()
}

/// The decay the undated document received.
fn undated_decay(root: &Path, options: &Options) -> f64 {
    decays(root, options)
        .into_iter()
        .find(|(path, _)| path == "undated-decision.md")
        .map(|(_, decay)| decay)
        .expect("the undated document matches the query")
}

/// Three dated documents plus one undated: ages 10, 20 and 30 days.
fn median_corpus() -> tempfile::TempDir {
    written(&[
        (
            "ten-decision.md",
            "---\ntype: decision\nstatus: active\ndate: 2025-03-22\n---\ncache\n",
        ),
        (
            "twenty-note.md",
            "---\ntype: note\nstatus: active\ndate: 2025-03-12\nscope: org:acme\n---\ncache\n",
        ),
        (
            "thirty-decision.md",
            "---\ntype: decision\nstatus: active\ndate: 2025-03-02\n---\ncache\n",
        ),
        (
            "undated-decision.md",
            "---\ntype: decision\nstatus: active\n---\ncache\n",
        ),
    ])
}

#[test]
fn case_13_1_a_quarter_in_a_file_name_is_a_filename_quarter() {
    let vault = written(&[("notes/2026-q4-plan.md", "x")]);
    let report = scan_json(vault.path(), &[]);
    let document = &report["docs"].as_array().expect("docs")[0];

    assert_eq!(document["date"].as_str(), Some("2026-10-01"), "{report}");
    assert_eq!(
        document["date_source"].as_str(),
        Some("filename_quarter"),
        "{report}"
    );
}

#[test]
fn case_13_2_every_quarter_separator_parses_to_the_first_day() {
    let vault = written(&[
        ("q2-2026.md", "x"),
        ("2026.q3.md", "x"),
        ("2026_q1.md", "x"),
        ("q4_2026.md", "x"),
        ("2026q2.md", "x"),
        ("q3.2026.md", "x"),
        ("2026-q4.md", "x"),
        ("q1-2026.md", "x"),
    ]);
    let report = scan_json(vault.path(), &[]);
    let mut actual: Vec<(String, String, String)> = report["docs"]
        .as_array()
        .expect("docs")
        .iter()
        .map(|document| {
            (
                document["rel_path"].as_str().expect("path").to_owned(),
                document["date"].as_str().expect("date").to_owned(),
                document["date_source"].as_str().expect("source").to_owned(),
            )
        })
        .collect();
    actual.sort();

    let expected: Vec<(String, String, String)> = [
        ("q2-2026.md", "2026-04-01", "filename_quarter"),
        ("2026.q3.md", "2026-07-01", "filename_quarter"),
        ("2026_q1.md", "2026-01-01", "filename_quarter"),
        ("q4_2026.md", "2026-10-01", "filename_quarter"),
        ("2026q2.md", "2026-04-01", "filename_quarter"),
        ("q3.2026.md", "2026-07-01", "filename_quarter"),
        ("2026-q4.md", "2026-10-01", "filename_quarter"),
        ("q1-2026.md", "2026-01-01", "filename_quarter"),
    ]
    .into_iter()
    .map(|(path, date, source)| (path.to_owned(), date.to_owned(), source.to_owned()))
    .collect();
    let mut expected = expected;
    expected.sort();
    assert_eq!(actual, expected, "{report}");
}

#[test]
fn case_13_3_an_impossible_quarter_stays_a_near_miss() {
    let vault = written(&[
        ("2026-q5.md", "x"),
        ("2026-q0.md", "x"),
        ("2026-q04.md", "x"),
        ("q4.md", "x"),
    ]);
    let report = scan_json(vault.path(), &[]);
    assert_eq!(
        report["date_near_misses"],
        serde_json::json!(["2026-q0.md", "2026-q04.md", "2026-q5.md", "q4.md"]),
        "{report}"
    );
    for document in report["docs"].as_array().expect("docs") {
        assert_eq!(document["date"], Value::Null, "{document}");
        assert_eq!(document["date_source"], "absent", "{document}");
    }
}

#[test]
fn case_13_4_an_undated_document_decays_by_the_median_age() {
    // Dated ages 10, 20, 30 days. Sorted, the middle is 20.
    // 2025-04-01 is `today`; 2025-03-22 is 10 days ago, 2025-03-12 is 20,
    // 2025-03-02 is 30. A decision half-life is 180 days:
    // exp(-ln2 · 20 / 180) = exp(-0.077016) = 0.9258747 → 0.9259 at 4 dp.
    let vault = median_corpus();
    index(vault.path());
    let expected = (-std::f64::consts::LN_2 * 20.0 / 180.0).exp();
    assert!(
        (expected - 0.9258747).abs() < 1e-7,
        "the constant is 0.9259"
    );

    let actual = undated_decay(vault.path(), &p8_options("cache"));
    assert!(
        (actual - expected).abs() < 1e-12,
        "undated decay {actual} != exp(-ln2·20/180) = {expected}"
    );
}

#[test]
fn case_13_5_an_even_count_takes_the_lower_middle() {
    // Dated ages 10 and 30 days. The lower middle of [10, 30] is 10:
    // exp(-ln2 · 10 / 180) = exp(-0.0385082) = 0.9622238 → 0.9622 at 4 dp.
    let vault = written(&[
        (
            "ten-decision.md",
            "---\ntype: decision\nstatus: active\ndate: 2025-03-22\n---\ncache\n",
        ),
        (
            "thirty-decision.md",
            "---\ntype: decision\nstatus: active\ndate: 2025-03-02\n---\ncache\n",
        ),
        (
            "undated-decision.md",
            "---\ntype: decision\nstatus: active\n---\ncache\n",
        ),
    ]);
    index(vault.path());
    let expected = (-std::f64::consts::LN_2 * 10.0 / 180.0).exp();
    assert!(
        (expected - 0.9622238).abs() < 1e-7,
        "the constant is 0.9622"
    );

    let actual = undated_decay(vault.path(), &p8_options("cache"));
    assert!(
        (actual - expected).abs() < 1e-12,
        "lower-middle decay {actual} != exp(-ln2·10/180) = {expected}"
    );
}

#[test]
fn case_13_6_a_corpus_with_no_dated_document_decays_by_one_and_says_so() {
    let vault = written(&[(
        "undated-decision.md",
        "---\ntype: decision\nstatus: active\n---\ncache\n",
    )]);
    index(vault.path());

    let config = Config::load(vault.path()).expect("load config");
    let outcome = search::query(
        vault.path(),
        &config,
        &p8_clock(),
        None,
        &p8_options("cache"),
    )
    .expect("the corpus queries");
    let hit = outcome.hits.first().expect("the undated document matches");
    assert_eq!(hit.breakdown.decay, 1.0, "no dated document means no age");
    assert_eq!(
        outcome
            .notes
            .iter()
            .filter(|note| note.contains("no dated document"))
            .count(),
        1,
        "one note explains the missing median: {:?}",
        outcome.notes
    );
}

#[test]
fn case_13_7_a_type_filter_does_not_move_the_median() {
    // The corpus is case 13.4. `--types decision` drops the 20-day note, so a
    // median built after the filter would be the lower middle of [10, 30] = 10
    // and decay 0.9622. The generation median stays 20 and decay stays 0.9259.
    let vault = median_corpus();
    index(vault.path());
    let mut options = p8_options("cache");
    options.types = vec!["decision".to_owned()];
    let filtered = undated_decay(vault.path(), &options);

    let expected = (-std::f64::consts::LN_2 * 20.0 / 180.0).exp();
    assert!(
        (filtered - expected).abs() < 1e-12,
        "--types decision moved the undated decay to {filtered}, expected {expected}"
    );
}

#[test]
fn case_13_7b_a_scope_filter_does_not_move_the_median() {
    // The 20-day note is `org:acme`; `--scope org:beta` drops it from the rows.
    // A median built after the filter would be 10. The generation median is 20.
    let vault = median_corpus();
    index(vault.path());
    let mut options = p8_options("cache");
    options.scopes = vec!["org:beta".to_owned()];
    let filtered = undated_decay(vault.path(), &options);

    let expected = (-std::f64::consts::LN_2 * 20.0 / 180.0).exp();
    assert!(
        (filtered - expected).abs() < 1e-12,
        "--scope moved the undated decay to {filtered}, expected {expected}"
    );
}
