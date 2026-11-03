//! Retrieval quality: `active top-1`, `nDCG@10`, and the α sweep (§3.5, §3.8, S-15).
//!
//! This is a **report, not a gate**. It is `#[ignore]`d so `cargo test
//! --all-targets` never fails on a ranking experiment; `scripts/eval.sh` runs it
//! and the `eval` CI job publishes the numbers with `continue-on-error: true`.
//! There is deliberately no quality floor in code: milestone acceptance
//! (`active top-1 ≥ 0.80`) is a human step recorded in the PR that changes the
//! ranking.
//!
//! The clock is injected (`FixedClock(2025-04-01)`), so decay is deterministic
//! and a ranking cannot move because the suite ran in a different month.
//!
//! `active top-1` denominator: queries with `intent: current` whose expectation
//! names at least one **active** document. History/evolution/exhaustive queries
//! legitimately rank inactive documents and are excluded from that metric.
//!
//! # The α sweep
//!
//! With no environment variable this is M3's lexical report at α = 0, which is
//! what CI runs. `JANUS_EVAL_SWEEP=1` measures every α §3.5 pre-declares and
//! prints, per α:
//!
//! 1. `active top-1` and `nDCG@10` with their denominators;
//! 2. `rescued` / `broken` against α = 0 — the queries whose rank-1 hit becomes
//!    correct or wrong. This, not "the number went up", is the honest statement
//!    that the semantic layer helped;
//! 3. the semantic distribution (min/median/max) of each query's returned
//!    top-10, so a saturating cosine is visible rather than inferred;
//! 4. elapsed time, printed and never asserted.
//!
//! Every knob is read **here**, at the test edge, never in the library:
//!
//! | Variable | Effect |
//! |---|---|
//! | `JANUS_EVAL_SWEEP=1` | measure all six α values instead of one |
//! | `JANUS_EVAL_ALPHA=0.5` | measure exactly this α (plus the α = 0 reference) |
//! | `JANUS_EVAL_TITLE_BOOST=0.0` | set the title weight; the default is 1.0 |
//! | `JANUS_EVAL_MODEL=AllMiniLML6V2` | override `[embedding] model`; the fixture ships `hash-64` |
//! | `JANUS_MODEL_DIR=<dir>` | where the real model files live |
//!
//! `HashEmbedder`'s numbers are never an input to the α decision (§3.2): the
//! fixture's `hash-64` makes the sweep a smoke test that proves the instrument
//! runs and the numbers move. The decision is taken with a real model, and §14 of
//! `docs/m4-breakdown.md` records which corpus produced the numbers.

mod common;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use chrono::NaiveDate;
use janus_mem::clock::FixedClock;
use janus_mem::config::Config;
use janus_mem::document::Status;
use janus_mem::index::embed::{Embedder, ModelDirs, Resolved, resolve};
use janus_mem::search::{self, Options};

/// One eval query: a graded expectation, most relevant first.
#[derive(serde::Deserialize)]
struct Query {
    query: String,
    intent: String,
    expect: Vec<String>,
}

/// `nDCG@10`: the depth the metric is reported at.
const DEPTH: usize = 10;

/// The α values §3.5 pre-declares, so the sweep is not chosen after the fact.
const ALPHAS: [f64; 6] = [0.0, 0.35, 0.5, 0.65, 0.8, 1.0];

fn options(query: &str, intent: &str) -> Options {
    Options {
        query: query.to_owned(),
        intent: Some(intent.to_owned()),
        scopes: Vec::new(),
        scope_only: false,
        limit: DEPTH,
        types: Vec::new(),
        tags: Vec::new(),
        decay_half_life: None,
        include_superseded: false,
        no_rebuild: false,
        no_embeddings: false,
        embedding_reason: None,
        json: false,
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Discounted gain at `rank` (1-based), for a linear grade.
fn discount(gain: f64, rank: usize) -> f64 {
    gain / ((rank as f64) + 1.0).log2()
}

/// `min`, `median`, `max` of a set of semantic scores; `(0, 0, 0)` when empty.
fn shape(mut values: Vec<f64>) -> (f64, f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let median = if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    };
    (values[0], median, values[values.len() - 1])
}

/// Everything one α measures. Owned so the printer can compare two of them.
struct Measurement {
    top1: usize,
    eligible: usize,
    ndcg_sum: f64,
    graded: usize,
    /// Rank-1 correctness per query, in query order; `None` for a query the
    /// metric excludes, so `correct[i]` and `winners[i]` always describe the
    /// same query.
    correct: Vec<Option<bool>>,
    /// The rank-1 hit of every query, so a change can be named, not counted.
    winners: Vec<Option<String>>,
    /// Semantic score of every returned hit, for the per-α aggregate.
    semantics: Vec<f64>,
    /// Per query, `(min, median, max)` over its returned top-10.
    shapes: Vec<(f64, f64, f64)>,
    misses: Vec<String>,
    elapsed_ms: u128,
}

impl Measurement {
    fn ndcg(&self) -> f64 {
        self.ndcg_sum / self.graded.max(1) as f64
    }
}

/// Measures one α: one pass over every query, grouped to documents.
fn measure(
    root: &std::path::Path,
    config: &Config,
    clock: &FixedClock,
    embedder: Option<&dyn Embedder>,
    queries: &[Query],
    status: &BTreeMap<&str, Status>,
    alpha: f64,
) -> Measurement {
    let started = Instant::now();
    // α is a config value, so injecting it needs no CLI flag and no new
    // constructor: the harness sets the field the scorer already reads.
    let mut config = config.clone();
    config.embedding.alpha = alpha;

    let (mut eligible, mut top1) = (0_usize, 0_usize);
    let (mut ndcg_sum, mut graded) = (0.0_f64, 0_usize);
    let mut correct = Vec::new();
    let mut winners = Vec::new();
    let mut semantics = Vec::new();
    let mut shapes = Vec::new();
    let mut misses = Vec::new();

    for query in queries {
        let outcome = search::query(
            root,
            &config,
            clock,
            embedder,
            &options(&query.query, &query.intent),
        )
        .unwrap_or_else(|err| panic!("query {:?} failed: {err}", query.query));
        let ranked: Vec<&str> = outcome
            .hits
            .iter()
            .map(|hit| {
                outcome.rows[outcome.selected[hit.doc_index]]
                    .rel_path
                    .as_str()
            })
            .collect();
        semantics.extend(outcome.hits.iter().map(|hit| hit.breakdown.semantic));
        shapes.push(shape(
            outcome
                .hits
                .iter()
                .map(|hit| hit.breakdown.semantic)
                .collect(),
        ));

        winners.push(ranked.first().map(|path| (*path).to_owned()));

        let active: Vec<&String> = query
            .expect
            .iter()
            .filter(|path| status.get(path.as_str()) == Some(&Status::Active))
            .collect();
        if query.intent == "current" && !active.is_empty() {
            eligible += 1;
            let winner = ranked.first().copied();
            let hit = winner.is_some_and(|first| active.iter().any(|path| path.as_str() == first));
            correct.push(Some(hit));
            if hit {
                top1 += 1;
            } else {
                misses.push(format!(
                    "{:?} -> {:?}, expected one of {:?}",
                    query.query, winner, query.expect
                ));
            }
        } else {
            correct.push(None);
        }

        let grades: Vec<f64> = (0..query.expect.len())
            .map(|index| (query.expect.len() - index) as f64)
            .collect();
        let dcg: f64 = ranked
            .iter()
            .take(DEPTH)
            .enumerate()
            .filter_map(|(rank, path)| {
                query
                    .expect
                    .iter()
                    .position(|expected| expected == path)
                    .map(|position| discount(grades[position], rank + 1))
            })
            .sum();
        let mut ideal = grades.clone();
        ideal.sort_by(|a, b| b.total_cmp(a));
        let idcg: f64 = ideal
            .iter()
            .take(DEPTH)
            .enumerate()
            .map(|(rank, gain)| discount(*gain, rank + 1))
            .sum();
        if idcg > 0.0 {
            ndcg_sum += dcg / idcg;
            graded += 1;
        }
    }

    Measurement {
        top1,
        eligible,
        ndcg_sum,
        graded,
        correct,
        winners,
        semantics,
        shapes,
        misses,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

/// Resolves the embedder the sweep needs, or says why there is none.
///
/// The cache candidate cannot exist, so a model left in `~/.cache/fastembed` by
/// some other tool cannot make a run that forgot `JANUS_MODEL_DIR` look like a
/// success (the same discipline `tests/real_model.rs` follows).
fn embedder(config: &Config) -> (Option<Box<dyn Embedder>>, String) {
    let dirs = ModelDirs {
        config: config.embedding.model_dir.clone(),
        env: env::var_os("JANUS_MODEL_DIR").map(PathBuf::from),
        cache: env::temp_dir().join("janus-eval-no-model-cache"),
    };
    match resolve(config, &dirs) {
        Resolved::Ready(embedder) => {
            let id = embedder.id();
            let note = if id.config_warning().is_some() {
                format!("{} (test double: never a quality number)", id.name)
            } else {
                format!("{} (dim {})", id.name, id.dim)
            };
            (Some(embedder), note)
        }
        Resolved::Degraded(reason, detail) => (None, format!("none: {reason} ({detail})")),
    }
}

/// Parses a `JANUS_EVAL_ALPHA` value, failing loudly rather than measuring the
/// wrong weight.
/// Parses `JANUS_EVAL_TITLE_BOOST` as a title weight in `[0, 1]`.
///
/// The weight is a config value, so the harness sets the field the scorer
/// already reads, exactly as it does for alpha. The default is the corpus
/// value, which the fixture leaves at `1.0`.
fn requested_title_boost() -> f64 {
    let Ok(text) = env::var("JANUS_EVAL_TITLE_BOOST") else {
        return 1.0;
    };
    let value: f64 = text
        .trim()
        .parse()
        .unwrap_or_else(|err| panic!("JANUS_EVAL_TITLE_BOOST={text:?} is not a number: {err}"));
    assert!(
        (0.0..=1.0).contains(&value),
        "JANUS_EVAL_TITLE_BOOST={value} is outside [0, 1]"
    );
    value
}

fn requested_alpha() -> Option<f64> {
    let text = env::var("JANUS_EVAL_ALPHA").ok()?;
    let value: f64 = text
        .trim()
        .parse()
        .unwrap_or_else(|err| panic!("JANUS_EVAL_ALPHA={text:?} is not a number: {err}"));
    assert!(
        (0.0..=1.0).contains(&value),
        "JANUS_EVAL_ALPHA={value} is outside [0, 1]"
    );
    Some(value)
}

#[test]
#[ignore = "quality report, not a gate (§3.5, §3.8, S-15)"]
fn report_active_top1_and_ndcg() {
    let started = Instant::now();
    let vault = common::fixture_vault();
    let root = vault.path();
    let mut config = Config::load(root).expect("the fixture config parses");
    if let Some(model) = env::var("JANUS_EVAL_MODEL").ok().filter(|m| !m.is_empty()) {
        config.embedding.model = model;
    }
    config.scoring.title_boost = requested_title_boost();
    let clock = FixedClock(NaiveDate::from_ymd_opt(2025, 4, 1).expect("valid eval date"));
    let (embedder, embedder_note) = embedder(&config);

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval/queries.jsonl");
    let text = fs::read_to_string(path).expect("read tests/eval/queries.jsonl");
    let queries: Vec<Query> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("each line is a query"))
        .collect();

    let sweep = env::var_os("JANUS_EVAL_SWEEP").is_some_and(|value| value != "0");
    let mut alphas: Vec<f64> = if sweep {
        ALPHAS.to_vec()
    } else {
        vec![requested_alpha().unwrap_or(0.0)]
    };
    if alphas.iter().any(|alpha| *alpha > 0.0) && embedder.is_none() {
        println!("eval: α > 0 needs an embedder; {embedder_note}");
        println!("eval: measuring α = 0 only (set JANUS_EVAL_MODEL and JANUS_MODEL_DIR)");
        alphas = vec![0.0];
    }

    // One exhaustive pass with an empty query: no hits, every row and status.
    let all = search::query(
        root,
        &config,
        &clock,
        embedder.as_deref(),
        &options("", "exhaustive"),
    )
    .expect("the eval corpus indexes");
    let status: BTreeMap<&str, Status> = all
        .rows
        .iter()
        .map(|row| (row.rel_path.as_str(), row.status))
        .collect();

    println!("eval: {} queries over {}", queries.len(), root.display());
    println!("eval: embedder {embedder_note}");
    println!(
        "eval: title weight {}; {} title tokenization(s) per query over {} document(s) \
         and {} chunk(s)",
        config.scoring.title_boost,
        all.title_tokenizations,
        all.snapshot.docs.len(),
        all.snapshot.chunks.len(),
    );

    // α = 0 is the reference every other α is compared against, so it is always
    // measured even when only one other α was asked for.
    let reference = measure(
        root,
        &config,
        &clock,
        embedder.as_deref(),
        &queries,
        &status,
        0.0,
    );
    print_row(0.0, &reference, None, &queries);
    for miss in &reference.misses {
        println!("miss: {miss}");
    }

    for alpha in alphas.iter().copied().filter(|alpha| *alpha > 0.0) {
        let measurement = measure(
            root,
            &config,
            &clock,
            embedder.as_deref(),
            &queries,
            &status,
            alpha,
        );
        print_row(alpha, &measurement, Some(&reference), &queries);
    }

    println!("elapsed {} ms", started.elapsed().as_millis());
}

/// One line per α, then the diagnostics §3.5 asks for.
fn print_row(
    alpha: f64,
    measurement: &Measurement,
    reference: Option<&Measurement>,
    queries: &[Query],
) {
    let (low, median, high) = shape(measurement.semantics.clone());
    println!(
        "alpha {alpha:.2}  active-top-1 {:.2} ({}/{})  nDCG@10 {:.4} ({} queries)  \
         semantic {low:.3}/{median:.3}/{high:.3}  {} ms",
        ratio(measurement.top1, measurement.eligible),
        measurement.top1,
        measurement.eligible,
        measurement.ndcg(),
        measurement.graded,
        measurement.elapsed_ms,
    );

    if let Some(reference) = reference {
        let rescued: Vec<usize> = (0..reference.correct.len())
            .filter(|index| {
                reference.correct[*index] == Some(false)
                    && measurement.correct[*index] == Some(true)
            })
            .collect();
        let broken: Vec<usize> = (0..reference.correct.len())
            .filter(|index| {
                reference.correct[*index] == Some(true)
                    && measurement.correct[*index] == Some(false)
            })
            .collect();
        println!(
            "  rescued {} broken {} (against alpha 0.00)",
            rescued.len(),
            broken.len()
        );
        for index in rescued {
            println!(
                "  rescued {:?} -> {:?}",
                queries[index].query, measurement.winners[index]
            );
        }
        for index in broken {
            println!(
                "  broken  {:?} -> {:?} (was {:?})",
                queries[index].query, measurement.winners[index], reference.winners[index]
            );
        }
        println!("  semantic distribution of each query's returned top-10:");
        for (index, (low, median, high)) in measurement.shapes.iter().enumerate() {
            println!(
                "    {low:.3}/{median:.3}/{high:.3}  {:?}",
                queries[index].query
            );
        }
    }
}
