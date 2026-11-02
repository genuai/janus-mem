//! Retrieval quality: `active top-1` and `nDCG@10` (§3.8, S-15).
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

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::time::Instant;

use chrono::NaiveDate;
use janus_mem::clock::FixedClock;
use janus_mem::config::Config;
use janus_mem::document::Status;
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

#[test]
#[ignore = "quality report, not a gate (§3.8, S-15)"]
fn report_active_top1_and_ndcg() {
    let started = Instant::now();
    let vault = common::fixture_vault();
    let root = vault.path();
    let config = Config::load(root).expect("the fixture config parses");
    let clock = FixedClock(NaiveDate::from_ymd_opt(2025, 4, 1).expect("valid eval date"));

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/eval/queries.jsonl");
    let text = fs::read_to_string(path).expect("read tests/eval/queries.jsonl");
    let queries: Vec<Query> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("each line is a query"))
        .collect();

    // One exhaustive pass with an empty query: no hits, every row and status.
    // Lexical only: the report is the M3 baseline, not a hybrid measurement.
    let all = search::query(root, &config, &clock, None, &options("", "exhaustive"))
        .expect("the eval corpus indexes");
    let status: BTreeMap<&str, Status> = all
        .rows
        .iter()
        .map(|row| (row.rel_path.as_str(), row.status))
        .collect();

    let (mut eligible, mut top1) = (0_usize, 0_usize);
    let (mut ndcg_sum, mut graded) = (0.0_f64, 0_usize);
    let mut misses = Vec::new();

    for query in &queries {
        let outcome = search::query(
            root,
            &config,
            &clock,
            None,
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

        let active: Vec<&String> = query
            .expect
            .iter()
            .filter(|path| status.get(path.as_str()) == Some(&Status::Active))
            .collect();
        if query.intent == "current" && !active.is_empty() {
            eligible += 1;
            let winner = ranked.first().copied();
            if winner.is_some_and(|first| active.iter().any(|path| path.as_str() == first)) {
                top1 += 1;
            } else {
                misses.push(format!(
                    "{:?} -> {:?}, expected one of {:?}",
                    query.query, winner, query.expect
                ));
            }
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

    println!("eval: {} queries over {}", queries.len(), root.display());
    println!(
        "active-top-1 {:.2} ({}/{})",
        ratio(top1, eligible),
        top1,
        eligible
    );
    println!(
        "nDCG@10 {:.2} ({graded} queries)",
        ndcg_sum / graded.max(1) as f64
    );
    println!("elapsed {} ms", started.elapsed().as_millis());
    for miss in &misses {
        println!("miss: {miss}");
    }
}
