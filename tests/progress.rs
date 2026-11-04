//! Progress during a long build (M4D §3.6, D-6, D-7, D-8).
//!
//! Two kinds of assertion, and neither looks at the estimate. The library test
//! counts the values the sink reports, so a change to the cadence or the total
//! fails on a number. The command tests read stderr, because the printer is the
//! only thing that decides what a person sees.
//!
//! No test asserts a duration or an estimate (D-8): both depend on the machine.
//! No test reads the terminal: `common::run` captures stderr, so `auto` resolves
//! to "off" in every one of them, on any machine.

mod common;

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use common::{fixture_vault, index, run};
use janus_mem::config::Config;
use janus_mem::index::build::{self, BuildPlan, EmbedSink};
use janus_mem::index::embed::HashEmbedder;
use janus_mem::vault::{self, WalkOptions, identity, resolve};
use serde_json::Value;

/// The most progress lines one build may print: one per 5 % step.
const BOUNDARIES: usize = 20;

/// Plans the fixture with an embedder, reporting every embedded chunk to `sink`.
fn plan(root: &Path, previous: Option<&build::Previous>, sink: EmbedSink<'_>) -> BuildPlan {
    let config = Config::load(root).expect("the fixture has a config");
    let walk = vault::walk(root, &config, WalkOptions::default()).expect("walk the fixture");
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, &config);
    identity::derive_identity(&mut documents, &config);
    let embedder = HashEmbedder::new(64).expect("64 is a valid dimension");
    build::plan(&config, &documents, previous, Some(&embedder), sink).expect("the fixture builds")
}

/// Records every `(done, total)` pair a build reports, in call order.
#[derive(Clone, Default)]
struct Recorder(Rc<RefCell<Vec<(u64, u64)>>>);

impl Recorder {
    /// The sink, as a closure the build can call.
    fn sink(&self) -> impl Fn(u64, u64) {
        let log = Rc::clone(&self.0);
        move |done, total| log.borrow_mut().push((done, total))
    }

    fn reports(&self) -> Vec<(u64, u64)> {
        self.0.borrow().clone()
    }

    fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

// ───────────────────────────── the sink's values ──────────────────────────

#[test]
fn the_sink_reports_every_chunk_once_and_ends_at_the_total() {
    let vault = fixture_vault();
    let recorder = Recorder::default();
    let built = plan(vault.path(), None, &recorder.sink());
    let reports = recorder.reports();

    assert!(
        reports.len() > 1_000,
        "the fixture embeds more than a thousand chunks: {}",
        reports.len()
    );
    assert_eq!(
        reports.len(),
        built.stats.embedded_chunks,
        "the sink fires once per embedded chunk"
    );
    for pair in reports.windows(2) {
        assert!(pair[1].0 > pair[0].0, "done must increase: {pair:?}");
    }
    let (done, total) = *reports
        .last()
        .expect("the fixture embeds at least one chunk");
    assert_eq!(done, total, "the last report completes the build");
    assert_eq!(total as usize, built.stats.embedded_chunks);
}

#[test]
fn the_reported_steps_stay_within_the_twenty_boundaries() {
    let vault = fixture_vault();
    let recorder = Recorder::default();
    plan(vault.path(), None, &recorder.sink());

    let steps: BTreeSet<u64> = recorder
        .reports()
        .iter()
        .map(|(done, total)| done * 20 / total)
        // A step below 1 is not printed, so it is not a boundary (§3.6).
        .filter(|step| *step >= 1)
        .collect();

    assert!(
        steps.len() <= BOUNDARIES,
        "a 5 % step cannot exceed {BOUNDARIES} boundaries: {steps:?}"
    );
    assert_eq!(steps.iter().max(), Some(&20), "the build reaches its end");
}

#[test]
fn a_build_that_reuses_every_row_reports_nothing() {
    let vault = fixture_vault();
    let recorder = Recorder::default();
    let first = plan(vault.path(), None, &recorder.sink());
    let rows = first.vectors.as_ref().expect("the fixture embeds");
    let previous = build::Previous::with_vectors(
        first.snapshot.clone(),
        rows.id().clone(),
        rows.rows().clone(),
    );

    recorder.clear();
    let again = plan(vault.path(), Some(&previous), &recorder.sink());

    assert_eq!(again.stats.embedded_chunks, 0, "every row is reused");
    assert!(
        recorder.reports().is_empty(),
        "nothing is embedded, so nothing reports"
    );
}

// ─────────────────────────────── the gate ─────────────────────────────────

#[test]
fn a_captured_stderr_prints_no_progress() {
    let vault = fixture_vault();
    let output = run(vault.path(), &["index"]);

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "",
        "auto is off when stderr is not a terminal"
    );
}

#[test]
fn never_prints_no_progress() {
    let vault = fixture_vault();
    let output = run(vault.path(), &["index", "--progress=never"]);

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        ""
    );
}

#[test]
fn a_second_build_embeds_nothing_and_prints_no_summary() {
    let vault = fixture_vault();
    index(vault.path());
    let output = run(vault.path(), &["index", "--progress=always"]);

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "",
        "a full reuse embeds nothing, so it reports nothing"
    );
}

// ────────────────────────── the lines a person sees ───────────────────────

#[test]
fn always_prints_at_most_twenty_progress_lines_and_one_summary() {
    let vault = fixture_vault();
    let output = run(vault.path(), &["index", "--json", "--progress=always"]);
    assert!(
        output.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // stdout is unchanged: the receipt parses with progress on (§3.6).
    let report: Value = serde_json::from_slice(&output.stdout).expect("stdout is valid JSON");
    let embedded = report["embedded"]
        .as_u64()
        .expect("a numeric embedded count");
    assert!(embedded > 1_000, "the fixture embeds its chunks: {report}");

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    let lines: Vec<&str> = stderr.lines().collect();
    let progress: Vec<&&str> = lines
        .iter()
        .filter(|line| line.contains(" chunks ("))
        .collect();
    let summary: Vec<&&str> = lines
        .iter()
        .filter(|line| !line.contains('(') && line.starts_with("embedded "))
        .collect();

    assert!(!progress.is_empty(), "progress is on: {stderr}");
    assert!(
        progress.len() <= BOUNDARIES,
        "{} progress lines: {stderr}",
        progress.len()
    );
    for line in &progress {
        assert!(line.starts_with("embedded "), "{line}");
    }
    assert_eq!(summary.len(), 1, "exactly one summary line: {stderr}");
    assert_eq!(*summary[0], format!("embedded {embedded} chunks"));
}

#[test]
fn a_read_that_builds_an_absent_index_reports_progress() {
    let vault = fixture_vault();
    let output = run(
        vault.path(),
        &["search", "cache", "--json", "--progress=always"],
    );
    assert!(
        output.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _: Value = serde_json::from_slice(&output.stdout).expect("stdout is valid JSON");

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("refresh: no index yet"), "{stderr}");
    assert!(
        stderr.lines().any(|line| line.starts_with("embedded ")),
        "a read that builds reports too: {stderr}"
    );
}

#[test]
fn a_refresh_under_a_captured_stderr_prints_only_its_line() {
    let vault = fixture_vault();
    index(vault.path());
    let path = vault.path().join("notes/undated.md");
    let mut body = fs::read_to_string(&path).expect("read the fixture document");
    body.push_str("\nan appended line makes the corpus stale\n");
    fs::write(&path, body).expect("write the fixture document");

    let output = run(vault.path(), &["search", "cache", "--json"]);
    assert!(
        output.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(stderr.contains("refresh: corpus changed"), "{stderr}");
    assert!(
        !stderr.contains("embedded "),
        "the gate is off in a captured stderr: {stderr}"
    );
}
