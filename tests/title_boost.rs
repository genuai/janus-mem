//! The title boost end to end (M4C §3.1, SPEC §7.2).
//!
//! The reproduction corpus is the query shape that failed on a real corpus: ten
//! short notes that mention `q4` and `okrs` once, one long document titled
//! `Q4 plan draft` with eight sections, and twenty filler notes that carry the
//! common words but never `q4`. Without the boost the long document ranks 11.
//! With the default weight it ranks 1.
//!
//! The filler notes are not decoration. A term the index never saw leaves the
//! denominator, and a corpus where `q4` is the common word gives the boost a
//! weight it does not deserve. The filler sets the idf balance the real corpus
//! has: 152 of 2 119 documents mention `q4`, so the term is rare, but not unique.
//!
//! # The arithmetic, from the built index
//!
//! `df` counts chunks, not documents: the index scores chunks.
//!
//! | term | df | idf |
//! |---|---|---|
//! | `what` | 30 | 0.24583496251628056 |
//! | `is` | 30 | 0.24583496251628056 |
//! | `our` | 30 | 0.24583496251628056 |
//! | `okrs` | 30 | 0.24583496251628056 |
//! | `q4` | 18 | 0.74579091404536724 |
//!
//! There are 38 chunks. Every one of the five query terms is in the index, so
//! `total` uses all five: `0.74579091404536724 + 4 · 0.24583496251628056 =
//! 1.72913076411048938`. The plan's title carries `q4` only, so `hits` is
//! `0.74579091404536724`, `title_cov` is `0.43130972481946545`, and
//! `title_boost = 1 + 1.0 · cov = 1.43130972481946550`. The literal is below
//! 1.5 on purpose: a corpus that saturates the boost cannot pass this test.

mod common;

use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use janus_mem::clock::FixedClock;
use janus_mem::config::Config;
use janus_mem::index::LexicalIndex;
use janus_mem::index::store::{LoadOptions, load};
use janus_mem::search::{self, Options};
use janus_mem::tokenize::Tokenizer;

/// A pinned build date: nothing in a test may depend on the clock.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date")
}

/// The acceptance query: five terms, with `q4` alone in the title.
const QUERY: &str = "what is our q4 okrs";

/// The long document the reproduction is about.
const PLAN: &str = "plans/q4-plan-draft.md";

fn options(query: &str) -> Options {
    Options {
        query: query.to_owned(),
        intent: Some("history".to_owned()),
        scopes: Vec::new(),
        scope_only: false,
        limit: 40,
        types: Vec::new(),
        tags: Vec::new(),
        decay_half_life: None,
        include_superseded: false,
        no_rebuild: false,
        no_embeddings: true,
        embedding_reason: None,
        progress: false,
        json: false,
    }
}

fn write(root: &Path, rel_path: &str, body: &str) {
    let path = root.join(rel_path);
    fs::create_dir_all(path.parent().unwrap_or(root)).expect("create the document folder");
    fs::write(path, body).expect("write the document");
}

/// One short note: every one of the five query terms appears once.
fn mentioning_note(index: usize) -> String {
    format!(
        "---\ntitle: Weekly note {index}\ndate: 2025-01-01\nstatus: active\ntype: note\n---\n\n\
         # Weekly note\n\nwhat is our q4 okrs list for this week.\n"
    )
}

/// One filler note: the common words, but never `q4`.
fn filler_note(index: usize) -> String {
    format!(
        "---\ntitle: Admin note {index}\ndate: 2025-01-01\nstatus: active\ntype: note\n---\n\n\
         # Admin note\n\nwhat is our okrs schedule.\n"
    )
}

/// The long document: eight sections, all about `q4`, and no `okrs`.
fn plan_draft() -> String {
    let mut body = String::from(
        "---\ntitle: Q4 plan draft\ndate: 2025-01-02\nstatus: active\ntype: note\n---\n\n\
         ## Section 0\n\nq4 goals q4 timeline q4 owners.\n\n",
    );
    for section in 1..8 {
        body.push_str(&format!("## Section {section}\n\nq4 work continues.\n\n"));
    }
    body
}

/// The reproduction corpus, with extra config text, written fresh per run.
fn reproduction_vault(extra_config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        format!("index_dir = \".janus\"\ncorpus_id = \"title-boost\"\n{extra_config}"),
    )
    .expect("write config");
    write(root, PLAN, &plan_draft());
    for index in 1..=10 {
        write(
            root,
            &format!("notes/mention-{index:02}.md"),
            &mentioning_note(index),
        );
    }
    for index in 1..=20 {
        write(
            root,
            &format!("admin/admin-{index:02}.md"),
            &filler_note(index),
        );
    }
    dir
}

fn clock() -> FixedClock {
    FixedClock(NaiveDate::from_ymd_opt(2025, 4, 1).expect("valid date"))
}

/// Ranks the corpus and returns the hits, so a test can read a reported score.
fn hits(root: &Path, query: &str) -> Vec<(String, search::Hit)> {
    let config = Config::load(root).expect("load config");
    let outcome = search::query(root, &config, &clock(), None, &options(query))
        .expect("the reproduction corpus indexes and queries");
    outcome
        .hits
        .iter()
        .map(|hit| {
            (
                outcome.rows[outcome.selected[hit.doc_index]]
                    .rel_path
                    .clone(),
                *hit,
            )
        })
        .collect()
}

/// The `rel_path` of every hit, in rank order.
fn ranked(root: &Path, query: &str) -> Vec<String> {
    hits(root, query)
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

/// The lexical index a previous query built, for the arithmetic.
fn built_index(root: &Path) -> LexicalIndex {
    let config = Config::load(root).expect("load config");
    let load_options = LoadOptions::new(root, &config, today());
    let loaded = load(&load_options).expect("the corpus indexes");
    loaded
        .snapshot
        .to_index()
        .expect("the snapshot builds an index")
}

fn term_idf(index: &LexicalIndex, term: &str) -> f64 {
    let id = index.pool().lookup(term).expect("term is in the index");
    index.idf(index.terms()[id.as_usize()].df)
}

#[test]
fn the_document_about_q4_outranks_the_documents_that_mention_it() {
    let vault = reproduction_vault("");
    let ranked = ranked(vault.path(), QUERY);

    let first = ranked.first().map(String::as_str);
    assert_eq!(
        first,
        Some(PLAN),
        "the document about q4 must rank first: {ranked:?}"
    );
}

#[test]
fn the_boost_is_the_documented_literal() {
    let vault = reproduction_vault("");
    let index = built_index(vault.path());
    let tokenizer = Tokenizer::new();
    let tokens = tokenizer.tokenize(QUERY);
    let terms: Vec<&str> = tokens.iter().collect();

    // The five df counts from the module comment, over 38 chunks.
    assert_eq!(index.len(), 38, "the corpus has 38 chunks");
    for (term, df) in [
        ("what", 30),
        ("is", 30),
        ("our", 30),
        ("q4", 18),
        ("okrs", 30),
    ] {
        let id = index.pool().lookup(term).expect("term is in the index");
        assert_eq!(index.terms()[id.as_usize()].df, df, "df of {term}");
    }

    let total: f64 = terms.iter().map(|term| term_idf(&index, term)).sum();
    let matched: f64 = terms
        .iter()
        .filter(|term| **term == "q4")
        .map(|term| term_idf(&index, term))
        .sum();
    let coverage = matched / total;
    let boost = (1.0 + coverage).clamp(1.0, 2.0);

    assert!((term_idf(&index, "what") - 0.245_834_962_516_280_56).abs() < 1e-12);
    assert!((term_idf(&index, "q4") - 0.745_790_914_045_367_2).abs() < 1e-12);
    assert!((total - 1.729_130_764_110_489_4).abs() < 1e-12);
    assert!((matched - 0.745_790_914_045_367_2).abs() < 1e-12);
    assert!((coverage - 0.431_309_724_819_465_4).abs() < 1e-12);
    assert!((boost - 1.431_309_724_819_465_5).abs() < 1e-12);
    assert!(boost < 1.5, "the literal must not saturate: {boost}");

    // The report carries the same number, on the plan only: the notes' titles
    // hold no query term.
    for (path, hit) in hits(vault.path(), QUERY) {
        let expected = if path == PLAN { boost } else { 1.0 };
        assert!(
            (hit.breakdown.title_boost - expected).abs() < 1e-12,
            "{path}: boost {} != {expected}",
            hit.breakdown.title_boost
        );
    }
}

#[test]
fn a_zero_weight_reproduces_the_unboosted_ranking() {
    let vault = reproduction_vault("[scoring]\ntitle_boost = 0.0\n");
    let ranked = ranked(vault.path(), QUERY);

    let position = ranked.iter().position(|path| path == PLAN);
    assert_eq!(
        position,
        Some(10),
        "with w = 0 the plan stays 11th, behind the ten mentions: {ranked:?}"
    );
    for (path, hit) in hits(vault.path(), QUERY) {
        assert_eq!(hit.breakdown.title_boost, 1.0, "{path} is boosted: {hit:?}");
        // Multiplying by 1.0 is exact, so relevance is the blend itself.
        let blend = hit.breakdown.lexical;
        assert_eq!(hit.breakdown.relevance, blend, "{path}");
    }
}

#[test]
fn a_year_query_does_not_match_a_leading_date_in_the_title() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"dated-title\"\n",
    )
    .expect("write config");
    write(
        root,
        "decisions/2024-12-01-redis-cache.md",
        "---\ndate: 2024-12-01\nstatus: active\ntype: decision\n---\n\n# Cache\n\nredis cache notes.\n",
    );

    let found = hits(dir.path(), "2024");
    let (path, hit) = found.first().expect("2024 is in the title");
    assert_eq!(path, "decisions/2024-12-01-redis-cache.md");
    assert_eq!(
        hit.breakdown.title_boost, 1.0,
        "a leading date is not a topic"
    );
}

#[test]
fn the_fixture_tokenizes_each_title_once() {
    let vault = common::fixture_vault();
    common::index(vault.path());
    let config = Config::load(vault.path()).expect("load config");
    let outcome = search::query(vault.path(), &config, &clock(), None, &options("cache"))
        .expect("the fixture query succeeds");

    assert_eq!(
        outcome.title_tokenizations,
        common::FIXTURE_DOCUMENTS,
        "one title tokenization per selected document"
    );
}
