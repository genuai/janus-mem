//! Cases 4.1–4.7 and 7.2b of the M6 plan: the engine and its cache (M6 §3.4, §3.7).
//!
//! Every expected value here is written by hand from the files the test writes.
//! No test reads the wall clock, sleeps, binds a port, or captures its own
//! stderr: the `refresh:` line belongs to the CLI, and case 10.1 pins it there.
//! The one stderr line case 4.7 names — `refresh: skipped, index busy` — is
//! asserted for the CLI by `tests/cli_search.rs`, because this process cannot
//! capture its own stderr without a new dependency.

mod common;

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{advance_mtime, engine, engine_paths, write_document};
use janus_mem::api::{Call, Tool};
use janus_mem::clock::Clock;
use janus_mem::config::Config;
use janus_mem::engine::{CacheStats, Engine, Response};
use janus_mem::index::store::Lock;
use serde_json::{Value, json};

const ALPHA: &str = "---\ntitle: Alpha\ndate: 2024-01-01\n---\n\nredis cache\n";
const BETA: &str = "---\ntitle: Beta\ndate: 2024-02-01\n---\n\nunrelated prose\n";

/// A corpus of two documents: one about the cache, one about something else.
fn vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"engine\"\n",
    )
    .expect("write the config");
    write_document(dir.path(), "notes/alpha.md", ALPHA);
    write_document(dir.path(), "notes/beta.md", BETA);
    dir
}

/// One `search` call with the parameters a case needs.
fn search(params: Value) -> Call {
    Call::new(Tool::Search, params)
}

/// The `rel_path` of every result, asserting the call answered.
fn paths(response: &Response) -> Vec<String> {
    assert!(response.ok, "the call failed: {}", response.json);
    engine_paths(response)
}

/// Every file under `.janus/` with its modification time, sorted by path.
fn index_state(dir: &Path) -> Vec<(String, std::time::SystemTime)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, std::time::SystemTime)>) {
        for entry in fs::read_dir(dir).expect("read the index directory") {
            let entry = entry.expect("directory entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                walk(&path, root, out);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .expect("a path under the index")
                .display()
                .to_string();
            let modified = entry
                .metadata()
                .expect("metadata")
                .modified()
                .expect("mtime");
            out.push((rel, modified));
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// A clock a test can move, so case 4.4 can show the cache holds no "today".
#[derive(Clone)]
struct MovableClock(Arc<Mutex<chrono::NaiveDate>>);

impl MovableClock {
    fn new(date: chrono::NaiveDate) -> Self {
        Self(Arc::new(Mutex::new(date)))
    }

    fn set(&self, date: chrono::NaiveDate) {
        *self.0.lock().expect("the test clock") = date;
    }
}

impl Clock for MovableClock {
    fn today(&self) -> chrono::NaiveDate {
        *self.0.lock().expect("the test clock")
    }
}

fn date(year: i32, month: u32, day: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(year, month, day).expect("a real date")
}

// ──────────────────────────────── the cache ────────────────────────────────

#[test]
fn case_4_1_two_calls_on_one_corpus_are_one_load() {
    let dir = vault();
    let engine = engine(dir.path());
    let first = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    let second = engine.call(&search(json!({"query": "redis", "intent": "history"})));

    assert_eq!(paths(&first), vec!["notes/alpha.md"]);
    // The second answer is the first one: only `taken_ms` may differ, and it is
    // not part of the results.
    assert_eq!(first.json["results"], second.json["results"]);
    assert_eq!(engine.cache(), CacheStats { hits: 1, misses: 1 });
}

#[test]
fn case_4_2_an_edited_document_is_a_miss_and_moves_the_answer() {
    let dir = vault();
    let engine = engine(dir.path());
    let before = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    assert_eq!(paths(&before), vec!["notes/alpha.md"]);

    // The signature reads path, size and mtime, so the mtime is moved on
    // purpose; a write alone could land in the same clock tick.
    write_document(
        dir.path(),
        "notes/alpha.md",
        "---\ntitle: Alpha\ndate: 2024-01-01\n---\n\nunrelated prose\n",
    );
    write_document(
        dir.path(),
        "notes/beta.md",
        "---\ntitle: Beta\ndate: 2024-02-01\n---\n\nredis cache\n",
    );
    advance_mtime(&dir.path().join("notes/alpha.md"));
    advance_mtime(&dir.path().join("notes/beta.md"));

    let after = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    assert_eq!(paths(&after), vec!["notes/beta.md"]);
    assert_eq!(engine.cache(), CacheStats { hits: 0, misses: 2 });
}

#[test]
fn case_4_2b_an_edit_that_keeps_size_and_mtime_is_invisible_until_a_rebuild() {
    let dir = vault();
    let engine = engine(dir.path());
    let before = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    assert_eq!(paths(&before), vec!["notes/alpha.md"]);

    // Same path, same size, same mtime, one byte different: `redir`, not
    // `redis`. Freshness cannot see it and the cache key cannot either (§8.3).
    let path = dir.path().join("notes/alpha.md");
    let mtime = fs::metadata(&path)
        .expect("metadata")
        .modified()
        .expect("mtime");
    let edited = ALPHA.replace("redis", "redir");
    assert_eq!(edited.len(), ALPHA.len(), "the edit keeps the file size");
    fs::write(&path, edited).expect("write the edited document");
    fs::File::options()
        .write(true)
        .open(&path)
        .expect("open the document")
        .set_modified(mtime)
        .expect("restore the mtime");

    let stale = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    assert_eq!(
        paths(&stale),
        vec!["notes/alpha.md"],
        "an edit that keeps path, size and mtime is invisible"
    );
    assert_eq!(engine.cache(), CacheStats { hits: 1, misses: 1 });

    // A rebuild re-reads the bytes, and the next call follows the new generation.
    common::run_ok(dir.path(), &["index", "--rebuild"]);
    let gone = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    assert!(
        paths(&gone).is_empty(),
        "the word is gone: {:?}",
        paths(&gone)
    );
    let found = engine.call(&search(json!({"query": "redir", "intent": "history"})));
    assert_eq!(paths(&found), vec!["notes/alpha.md"]);
}

#[test]
fn case_4_3_two_threads_on_one_engine_agree_with_one_thread() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Engine>();

    let dir = vault();
    let engine = engine(dir.path());
    let single = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    let expected = single.json["results"].clone();

    // No sleeps: the threads are joined and both answers are compared.
    let (left, right) = std::thread::scope(|scope| {
        let left =
            scope.spawn(|| engine.call(&search(json!({"query": "redis", "intent": "history"}))));
        let right =
            scope.spawn(|| engine.call(&search(json!({"query": "redis", "intent": "history"}))));
        (
            left.join().expect("the first thread"),
            right.join().expect("the second thread"),
        )
    });

    assert_eq!(left.json["results"], expected);
    assert_eq!(right.json["results"], expected);
}

#[test]
fn case_4_4_the_cache_holds_no_today() {
    let dir = vault();
    let clock = MovableClock::new(date(2026, 1, 1));
    let engine = Engine::new(
        dir.path().to_path_buf(),
        Config::load(dir.path()).expect("the corpus config"),
        Box::new(clock.clone()),
        None,
    );
    let params = json!({
        "query": "redis",
        "intent": "current",
        "decay_half_life_days": 365
    });
    let before = engine.call(&search(params.clone()));
    clock.set(date(2027, 1, 1));
    let after = engine.call(&search(params));

    // The generation was reused and the answer still moved: the decay is a
    // function of this call's clock, never of the cache.
    assert_eq!(engine.cache(), CacheStats { hits: 1, misses: 1 });
    let first = before.json["results"][0]["scores"]["decay"]
        .as_f64()
        .expect("a decay factor");
    let second = after.json["results"][0]["scores"]["decay"]
        .as_f64()
        .expect("a decay factor");
    // A 365-day half-life and a one-year gap: the factor halves exactly.
    assert!(
        (second - first / 2.0).abs() < 1e-12,
        "a year older must halve the decay: {first} -> {second}"
    );
}

#[test]
fn case_4_5_a_cache_hit_writes_nothing() {
    let dir = vault();
    let engine = engine(dir.path());
    // The first call builds the index; the state is taken after it.
    let built = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    assert!(built.ok, "{}", built.json);
    let before = index_state(&dir.path().join(".janus"));

    let hit = engine.call(&search(json!({"query": "redis", "intent": "history"})));
    assert!(hit.ok, "{}", hit.json);
    assert_eq!(engine.cache(), CacheStats { hits: 1, misses: 1 });
    assert_eq!(
        index_state(&dir.path().join(".janus")),
        before,
        "a cache hit must not write to the index"
    );
}

#[test]
fn case_4_7_a_busy_lock_is_a_skip_and_the_next_call_refreshes() {
    let dir = vault();
    // The generation exists before the lock is taken: a read that must build
    // would wait for the writer, and that is not what this case measures.
    common::index(dir.path());
    // Edit first, then take the lock, so the retry is what refreshes.
    write_document(
        dir.path(),
        "notes/gamma.md",
        "---\ntitle: Gamma\ndate: 2024-03-01\n---\n\nquokka marker\n",
    );
    advance_mtime(&dir.path().join("notes/gamma.md"));
    let lock = Lock::acquire(&dir.path().join(".janus"), Duration::from_secs(1))
        .expect("hold the writer lock");

    let engine = engine(dir.path());
    let skipped = engine.call(&search(json!({"query": "quokka", "intent": "history"})));
    assert!(skipped.ok, "a busy writer must not fail a read");
    assert_eq!(skipped.json["index"]["stale"], json!(true));
    assert!(
        paths(&skipped).is_empty(),
        "the loaded generation cannot know the new document"
    );

    drop(lock);
    // No further edit: the digest still differs from `corpus_sig`, so this call
    // is the one that refreshes.
    let refreshed = engine.call(&search(json!({"query": "quokka", "intent": "history"})));
    assert_eq!(refreshed.json["index"]["stale"], json!(false));
    assert_eq!(paths(&refreshed), vec!["notes/gamma.md"]);
    assert_eq!(engine.cache(), CacheStats { hits: 0, misses: 2 });
}

// ──────────────────────────────── the types filter ─────────────────────────

#[test]
fn case_7_2b_types_returns_only_the_named_types() {
    // One document of each fact type, and one of each other kind of type that
    // the filter must leave alone.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("janus.toml"), "corpus_id = \"types\"\n").expect("write the config");
    write_document(
        root,
        "decisions/one.md",
        "---\ntitle: One\ntype: decision\nstatus: active\ndate: 2024-01-01\n---\n\nthe cache\n",
    );
    write_document(
        root,
        "principles/two.md",
        "---\ntitle: Two\ntype: principle\nstatus: active\ndate: 2024-01-01\n---\n\nthe cache\n",
    );
    write_document(
        root,
        "specs/three.md",
        "---\ntitle: Three\ntype: spec\nstatus: active\ndate: 2024-01-01\n---\n\nthe cache\n",
    );

    let engine = engine(root);
    let facts = engine.call(&Call::new(
        Tool::AuthoritativeFacts,
        json!({"types": ["decision", "principle"]}),
    ));
    let mut got = paths(&facts);
    got.sort();
    assert_eq!(got, vec!["decisions/one.md", "principles/two.md"]);

    // The CLI flag answers with the same set: one predicate, two surfaces.
    let cli = common::facts_json(root, &["--types", "decision,principle", "--no-embeddings"]);
    let mut cli_paths = common::result_paths(&cli);
    cli_paths.sort();
    assert_eq!(cli_paths, got);

    // Without the filter, every fact type is returned, newest first by authority.
    let all = engine.call(&Call::new(Tool::AuthoritativeFacts, json!({})));
    assert_eq!(paths(&all).len(), 3, "{}", all.json);
}
