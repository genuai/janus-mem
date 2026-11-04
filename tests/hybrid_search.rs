//! Hybrid ranking end to end: semantic-only admission, α = 0, and the
//! `--no-embeddings` consequences (§3.3, §3.6, V-9, V-14, V-15).

mod common;

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::NaiveDate;
use common::{facts_json, fixture_vault, index, result_paths, search_json};
use janus_mem::clock::FixedClock;
use janus_mem::config::Config;
use janus_mem::index::embed::{EmbedError, Embedder, EmbedderId, HashEmbedder};
use janus_mem::index::squash;
use janus_mem::index::store::{self, LoadOptions, Policy};
use janus_mem::index::vectors::dot;
use janus_mem::search::{self, Options};
use janus_mem::tokenize::Tokenizer;
use serde_json::Value;

/// Options with every optional filter off, so a test names only what it needs.
fn options(query: &str) -> Options {
    Options {
        query: query.to_owned(),
        intent: Some("history".to_owned()),
        scopes: Vec::new(),
        scope_only: false,
        limit: 10,
        types: Vec::new(),
        tags: Vec::new(),
        decay_half_life: None,
        include_superseded: false,
        no_rebuild: false,
        no_embeddings: false,
        embedding_reason: None,
        progress: false,
        json: false,
    }
}

/// A vault whose only document is built from a token that hashes to the same
/// bucket as `zebra` but shares no lexical term with it.
fn semantic_only_vault(query: &str) -> tempfile::TempDir {
    let embedder = HashEmbedder::new(64).expect("64 is a valid dimension");
    let query_row = embedder
        .embed(&[query])
        .expect("the hash double never fails")[0]
        .clone();
    let term = (0..20_000)
        .map(|index| format!("semantic{index}"))
        .find(|term| {
            let row = &embedder
                .embed(&[term.as_str()])
                .expect("the hash double never fails")[0];
            dot(&query_row, row) > 0.99
        })
        .expect("a 64-bucket hash always has a collision within 20 000 terms");

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"semantic-only\"\n\n[embedding]\nmodel = \"hash-64\"\n",
    )
    .expect("write config");
    fs::create_dir_all(root.join("notes")).expect("create notes");
    fs::write(
        root.join("notes/semantic.md"),
        format!("---\ntitle: {term}\ndate: 2025-03-01\n---\n\n# {term}\n\n{term} {term}\n"),
    )
    .expect("write document");
    dir
}

#[test]
fn a_semantic_only_document_enters_at_the_default_alpha_and_not_at_alpha_zero() {
    let vault = semantic_only_vault("zebra");
    index(vault.path());

    let hybrid = search_json(vault.path(), &["zebra", "--no-rebuild"]);
    assert_eq!(hybrid["alpha"], 0.5, "{hybrid}");
    assert!(
        result_paths(&hybrid).contains(&"notes/semantic.md".to_owned()),
        "a semantic-only match must be admitted: {hybrid}"
    );
    let hit = hybrid["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|hit| hit["rel_path"] == "notes/semantic.md")
        .expect("the semantic-only document");
    assert_eq!(hit["scores"]["lexical"], 0.0, "{hit}");
    assert!(hit["scores"]["semantic"].as_f64().unwrap() > 0.0, "{hit}");

    let lexical = search_json(vault.path(), &["zebra", "--no-embeddings", "--no-rebuild"]);
    assert_eq!(lexical["alpha"], 0.0, "{lexical}");
    assert!(
        result_paths(&lexical).is_empty(),
        "without vectors the document has no signal at all: {lexical}"
    );
}

#[test]
fn facts_reports_a_zero_alpha_and_stays_authority_ordered() {
    let vault = fixture_vault();
    index(vault.path());

    let report = facts_json(vault.path(), &[]);
    assert_eq!(report["alpha"], 0.0, "{report}");
    assert_eq!(
        report["degraded"],
        serde_json::json!(["facts: lexical only (V-15)"]),
        "{report}"
    );

    let results = report["results"].as_array().expect("results");
    assert!(!results.is_empty(), "{report}");
    let mut last = f64::INFINITY;
    for hit in results {
        let authority = hit["scores"]["type_authority"].as_f64().expect("authority");
        assert!(authority <= last, "authority order moved: {report}");
        last = authority;
        assert_eq!(hit["scores"]["semantic"], 0.0, "{hit}");
        assert_eq!(
            hit["scores"]["title_boost"],
            serde_json::json!(1.0),
            "facts passes no title boost: {hit}"
        );
        assert_eq!(
            hit["scores"]["relevance"], hit["scores"]["lexical"],
            "facts is lexical even with an embedder available: {hit}"
        );
    }
}

/// Patches only the manifest's corpus id, forcing a stale generation without
/// changing any document's identity (which a config edit would do).
fn patch_manifest_corpus(root: &std::path::Path, corpus_id: &str) {
    let path = root.join(".janus/manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&path).expect("read manifest")).expect("manifest JSON");
    manifest["corpus_id"] = serde_json::json!(corpus_id);
    fs::write(
        path,
        serde_json::to_vec_pretty(&manifest).expect("encode manifest"),
    )
    .expect("patch manifest");
}

#[test]
fn a_no_embeddings_index_carries_unchanged_rows_and_drops_changed_ones() {
    let vault = fixture_vault();
    index(vault.path());
    let vectors = vault.path().join(".janus/vectors.f32");
    let before = fs::read(&vectors).expect("the fixture's hash model writes vectors");

    // M4D P2: a read may no longer rebuild a `corpus_id` mismatch (D-3), so the
    // degraded rebuild this test pins is the command's job now.
    patch_manifest_corpus(vault.path(), "renamed");
    let report = common::json_ok(vault.path(), &["index", "--no-embeddings", "--json"]);
    assert_eq!(report["embedded"], 0, "{report}");
    assert_eq!(report["reused_vectors"], report["chunks"], "{report}");
    assert!(
        vectors.exists(),
        "nothing changed, so a degraded rebuild carries the rows"
    );
    assert_eq!(
        fs::read(&vectors).expect("read carried vectors"),
        before,
        "a carried payload is byte-identical"
    );
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(vault.path().join(".janus/manifest.json")).expect("manifest"),
    )
    .expect("manifest is JSON");
    assert_eq!(
        manifest["embedder"]["name"], "hash-64",
        "the manifest describes the file that is still there"
    );
    assert!(manifest["vectors_sha"].is_string(), "{manifest}");

    // One body edit changes a row, and this build cannot produce a replacement.
    let note = vault.path().join("2025-01-05-corpus-notes.md");
    let text = fs::read_to_string(&note).expect("read a fixture document");
    fs::write(&note, format!("{text}\n\nchanged body\n")).expect("append to the document");
    patch_manifest_corpus(vault.path(), "renamed-again");

    let report = common::json_ok(vault.path(), &["index", "--no-embeddings", "--json"]);
    assert_eq!(report["dim"], Value::Null, "{report}");
    assert!(
        !vectors.exists(),
        "one changed document makes the rows uncarryable, so they go"
    );
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(vault.path().join(".janus/manifest.json")).expect("manifest"),
    )
    .expect("manifest is JSON");
    assert!(manifest["embedder"].is_null(), "{manifest}");
    assert!(manifest["vectors_sha"].is_null(), "{manifest}");
    assert!(manifest["vectors_len"].is_null(), "{manifest}");
}

#[test]
fn a_disabled_embedder_degrades_to_lexical_with_the_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"disabled\"\n\n[embedding]\nenabled = false\n",
    )
    .expect("write config");
    fs::write(
        dir.path().join("note.md"),
        "---\ntitle: Disabled\ndate: 2025-03-01\n---\n\n# Disabled\n\nalpha beta\n",
    )
    .expect("write document");

    let report = search_json(dir.path(), &["alpha"]);
    assert_eq!(report["alpha"], 0.0, "{report}");
    assert_eq!(
        report["degraded"],
        serde_json::json!(["embeddings disabled in janus.toml; lexical only"]),
        "{report}"
    );
    for hit in report["results"].as_array().expect("results") {
        assert_eq!(hit["scores"]["semantic"], 0.0, "{hit}");
    }
}

#[test]
fn a_configured_alpha_is_reported_verbatim() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"alpha\"\n\n[embedding]\nmodel = \"hash-64\"\nalpha = 0.35\n",
    )
    .expect("write config");
    fs::write(
        dir.path().join("note.md"),
        "---\ntitle: Alpha\ndate: 2025-03-01\n---\n\n# Alpha\n\nalpha beta\n",
    )
    .expect("write document");

    let report = search_json(dir.path(), &["alpha"]);
    assert_eq!(report["alpha"], 0.35, "{report}");
    let hit = &report["results"][0];
    let semantic = hit["scores"]["semantic"].as_f64().expect("semantic");
    let lexical = hit["scores"]["lexical"].as_f64().expect("lexical");
    let relevance = hit["scores"]["relevance"].as_f64().expect("relevance");
    let boost = hit["scores"]["title_boost"].as_f64().expect("title_boost");
    // The title is `Alpha` and the query is `alpha`, so the boost is real, and
    // the blend identity must hold with it (M4C §3.2).
    assert!(boost > 1.0, "a matching title must boost: {hit}");
    assert!(
        (relevance - (0.35 * semantic + 0.65 * lexical) * boost).abs() < 1e-12,
        "relevance {relevance} must be the blend times {boost}: {hit}"
    );
}

/// Wraps the hash double and counts calls, so "no embedder call" is a fact.
struct CountingEmbedder {
    inner: HashEmbedder,
    calls: AtomicUsize,
}

impl CountingEmbedder {
    fn new(dim: u32) -> Self {
        Self {
            inner: HashEmbedder::new(dim).expect("a positive dimension"),
            calls: AtomicUsize::new(0),
        }
    }
}

impl Embedder for CountingEmbedder {
    fn id(&self) -> &EmbedderId {
        self.inner.id()
    }

    fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.embed(batch)
    }
}

#[test]
fn an_empty_query_calls_no_embedder_and_a_real_query_calls_it_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"empty-query\"\n\n[embedding]\nmodel = \"hash-64\"\n",
    )
    .expect("write config");
    fs::write(
        dir.path().join("note.md"),
        "---\ntitle: Query\ndate: 2025-03-01\n---\n\n# Query\n\nalpha beta\n",
    )
    .expect("write document");
    index(dir.path());

    let config = Config::load(dir.path()).expect("load config");
    let clock = FixedClock(NaiveDate::from_ymd_opt(2025, 4, 1).expect("valid date"));
    let embedder = CountingEmbedder::new(64);

    let empty = search::query(dir.path(), &config, &clock, Some(&embedder), &options(""))
        .expect("an empty query still resolves the corpus");
    assert_eq!(embedder.calls.load(Ordering::Relaxed), 0);
    assert!(
        empty.hits.is_empty(),
        "an empty query has no lexical signal"
    );
    assert_eq!(empty.alpha, 0.5);

    let filled = search::query(
        dir.path(),
        &config,
        &clock,
        Some(&embedder),
        &options("alpha"),
    )
    .expect("a real query succeeds");
    assert_eq!(embedder.calls.load(Ordering::Relaxed), 1);
    assert!(!filled.hits.is_empty());
    for hit in &filled.hits {
        assert!(hit.breakdown.semantic > 0.0, "{}", hit.breakdown.semantic);
    }
}

#[test]
#[ignore = "measurement: prints the costs and distribution, never asserts them (§3.3)"]
fn report_hybrid_cost_and_semantic_shape() {
    let vault = fixture_vault();
    index(vault.path());

    let config = Config::load(vault.path()).expect("load config");
    let clock = FixedClock(NaiveDate::from_ymd_opt(2025, 4, 1).expect("valid date"));
    let embedder = HashEmbedder::new(64).expect("64 is a valid dimension");
    let mut opts = options("cache");
    opts.limit = usize::MAX;

    let started = std::time::Instant::now();
    let outcome = search::query(vault.path(), &config, &clock, Some(&embedder), &opts)
        .expect("the fixture query succeeds");
    let elapsed = started.elapsed();

    let mut semantic: Vec<f64> = outcome
        .hits
        .iter()
        .take(10)
        .map(|hit| hit.breakdown.semantic)
        .collect();
    semantic.sort_by(f64::total_cmp);
    let median = if semantic.is_empty() {
        f64::NAN
    } else {
        semantic[semantic.len() / 2]
    };
    let min = semantic.first().copied().unwrap_or(f64::NAN);
    let max = semantic.last().copied().unwrap_or(f64::NAN);
    let semantic_only_docs = outcome
        .hits
        .iter()
        .filter(|hit| hit.breakdown.lexical == 0.0 && hit.breakdown.semantic > 0.0)
        .count();

    // Chunk-level admission, from the same generation the query read.
    let mut load_options = LoadOptions::new(vault.path(), &config, "2025-04-01T00:00:00Z");
    load_options.policy = Policy::Refuse;
    load_options.want_vectors = true;
    load_options.embedder = Some(&embedder);
    let load = store::load(&load_options).expect("the indexed generation loads");
    let index = load
        .snapshot
        .to_index()
        .expect("the snapshot builds an index");
    let tokens = Tokenizer::new().tokenize("cache");
    let terms: Vec<&str> = tokens.iter().collect();
    let (raw, reference) = index.score_query(&terms);
    let query_row = embedder
        .embed(&["cache"])
        .expect("the hash double never fails")
        .remove(0);
    let rows = load.vectors.as_ref().expect("the fixture has vectors");
    let semantic_only_chunks = (0..rows.len())
        .filter(|index| {
            let lexical = squash(raw.get(*index).copied().unwrap_or(0.0), reference);
            let row = rows.row(*index).expect("row in range");
            let semantic = f64::from(dot(&query_row, row).clamp(0.0, 1.0));
            lexical == 0.0 && semantic > 0.0
        })
        .count();

    println!(
        "hybrid fixture: query() = {elapsed:?}; alpha = {}; matched {} of {} documents; \
         top-10 semantic min/median/max = {min:.4}/{median:.4}/{max:.4}; \
         semantic-only document winners = {semantic_only_docs}; \
         semantic-only chunk admissions = {semantic_only_chunks} of {} chunks",
        outcome.alpha,
        outcome.hits.len(),
        outcome.rows.len(),
        rows.len(),
    );
}
