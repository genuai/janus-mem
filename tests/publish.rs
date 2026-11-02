//! Publication, locking and reader retry (§3.5, S-7, S-17).
//!
//! Every case here is deterministic. No test sleeps to create a race: the
//! publisher is never paused (a paused publisher holds the lock a recovering
//! reader waits for), readers are spawned from `publish_with_hook` and joined
//! after the publish returns, and the post-crash pair is built directly because
//! that is the durable state a crash leaves.
//!
//! Writes go through the public API only. The "clean build" a recovery is
//! compared against is computed here from the vault, independently of whatever
//! `store::load` did.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use janus_mem::config::{Config, resolve_index_dir};
use janus_mem::error::JanusError;
use janus_mem::index::Snapshot;
use janus_mem::index::build::{self, BuildPlan, BuildStats};
use janus_mem::index::store::{
    self, INDEX_FILE, Load, LoadOptions, Lock, MANIFEST_FILE, Policy, Publication, Stage,
};
use janus_mem::vault::{self, WalkOptions, identity, resolve};

/// A pinned build time, so no manifest in this file depends on the clock.
const BUILT_AT: &str = "2025-04-01T00:00:00Z";

// ─────────────────────────────── the vault ────────────────────────────────

fn temp_vault(docs: &[(&str, &str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"publish\"\n",
    )
    .expect("write config");
    for (rel_path, title, body) in docs {
        write_doc(dir.path(), rel_path, title, body);
    }
    dir
}

/// A document with a fixed date: an mtime-derived date would make the snapshot
/// depend on when the test ran.
fn write_doc(root: &Path, rel_path: &str, title: &str, body: &str) {
    let path = root.join(rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create document directory");
    }
    let text = format!(
        "---\ntitle: {title}\ndate: 2025-01-01\ntype: note\nstatus: active\n---\n\n{body}\n"
    );
    fs::write(path, text).expect("write document");
}

fn config(root: &Path) -> Config {
    Config::load(root).expect("the test vault has a config")
}

fn index_dir(root: &Path) -> PathBuf {
    resolve_index_dir(root, &config(root).index_dir)
}

/// A clean build from the vault: the independent oracle a recovery is compared
/// against, not the code under test.
fn clean_plan(root: &Path) -> BuildPlan {
    let config = config(root);
    let walk = vault::walk(root, &config, WalkOptions::default()).expect("walk the vault");
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, &config);
    identity::derive_identity(&mut documents, &config);
    build::plan(&config, &documents, None)
}

fn publish(root: &Path, plan: &BuildPlan) {
    publish_hooked(root, plan, &mut |_stage| {});
}

fn publish_hooked(root: &Path, plan: &BuildPlan, hook: &mut dyn FnMut(Stage)) {
    let dir = index_dir(root);
    let publication = Publication {
        dir: &dir,
        root,
        snapshot: &plan.snapshot,
        chunker: plan.chunker,
        built_at: BUILT_AT,
    };
    store::publish_with_hook(&publication, hook).expect("publish");
}

fn load(root: &Path, config: &Config) -> Load {
    store::load(&LoadOptions::new(root, config, BUILT_AT)).expect("load a validated generation")
}

// ─────────────────────────────── the reader ───────────────────────────────

/// What one reader outside the test thread observed.
struct ReadResult {
    rebuilt: bool,
    stats: BuildStats,
    hits: Vec<String>,
    index_sha: String,
}

/// What a reader process does: load, then prove the pair it got is one
/// generation. The assertions live here so every case inherits them.
fn read(root: &Path, term: &str) -> ReadResult {
    let config = config(root);
    let load = load(root, &config);
    let bytes = encoded(&load.snapshot);
    assert_eq!(
        blake3::hash(&bytes).to_hex().to_string(),
        load.manifest.index_sha,
        "the bytes the reader loaded must hash to the manifest it read"
    );
    assert_eq!(bytes.len() as u64, load.manifest.index_len);
    ReadResult {
        rebuilt: load.rebuilt,
        stats: load.stats,
        hits: hits(&load.snapshot, term),
        index_sha: load.manifest.index_sha,
    }
}

/// Document paths with a non-zero score for `term`, deduplicated and sorted.
fn hits(snapshot: &Snapshot, term: &str) -> Vec<String> {
    let index = snapshot.to_index().expect("a loaded snapshot is valid");
    let (scores, _reference) = index.score_query(&[term]);
    let mut paths: Vec<String> = scores
        .iter()
        .enumerate()
        .filter(|(_, score)| **score > 0.0)
        .filter_map(|(chunk, _)| snapshot.chunks.get(chunk))
        .filter_map(|chunk| snapshot.docs.get(chunk.doc as usize))
        .filter_map(|doc| snapshot.strings.get(doc.rel_path))
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

fn encoded(snapshot: &Snapshot) -> Vec<u8> {
    snapshot.to_bytes().expect("a valid snapshot encodes")
}

/// Every file in `.janus/`, so "wrote nothing" can be asserted byte-for-byte.
fn dir_snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("the index directory exists") {
        let entry = entry.expect("index directory entry");
        files.insert(
            entry.file_name().to_string_lossy().into_owned(),
            fs::read(entry.path()).expect("read an index directory file"),
        );
    }
    files
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

/// One corruption applied to an index file: a name and the mutation.
type Corruption = (&'static str, fn(&mut Vec<u8>));

// ────────────────────────────── acceptances ───────────────────────────────

#[test]
fn a_reader_at_every_publication_stage_sees_one_generation() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let pre = clean_plan(&root);
    publish(&root, &pre);

    // The second generation adds a document, so the two hit sets differ.
    write_doc(&root, "b.md", "Beta", "alpha store body");
    let post = clean_plan(&root);
    let pre_hits = hits(&pre.snapshot, "alpha");
    let post_hits = hits(&post.snapshot, "alpha");
    assert_ne!(pre_hits, post_hits, "the generations must be distinct");

    let mut readers = Vec::new();
    {
        // One reader per stage, spawned and joined *after* the publish: the
        // publisher never pauses, so no reader can be waiting on a lock that
        // will not be released until the hook returns.
        let mut hook = |_stage: Stage| {
            let root = root.clone();
            readers.push(thread::spawn(move || read(&root, "alpha")));
        };
        publish_hooked(&root, &post, &mut hook);
    }
    assert_eq!(readers.len(), 6, "the hook fires once per stage");

    for reader in readers {
        let result = reader.join().expect("reader thread");
        assert!(
            !result.rebuilt,
            "a reader met the completed generation and adopted it"
        );
        assert!(
            result.hits == pre_hits || result.hits == post_hits,
            "a reader saw a mix of generations: {:?}",
            result.hits
        );
    }
}

#[test]
fn two_simultaneous_auto_builds_cost_one_build() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache")]);
    let root = vault.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(2));

    let handles: Vec<_> = (0..2)
        .map(|_| {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                read(&root, "cache")
            })
        })
        .collect();
    let results: Vec<ReadResult> = handles
        .into_iter()
        .map(|handle| handle.join().expect("builder thread"))
        .collect();

    assert_eq!(
        results.iter().filter(|result| result.rebuilt).count(),
        1,
        "exactly one auto-build builds"
    );
    assert_eq!(
        results.iter().filter(|result| !result.rebuilt).count(),
        1,
        "the other adopts the generation it waited for"
    );
    let adopter = results
        .iter()
        .find(|result| !result.rebuilt)
        .expect("one adopter");
    assert_eq!(
        adopter.stats,
        BuildStats::default(),
        "adoption re-tokenizes nothing"
    );
    assert_eq!(results[0].hits, results[1].hits);
    assert_eq!(results[0].index_sha, results[1].index_sha);
}

#[test]
fn a_held_lock_times_out_without_writing() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache")]);
    let root = vault.path().to_path_buf();
    publish(&root, &clean_plan(&root));

    // The pair a crash between the two renames leaves: new index bytes, old
    // manifest. This is the only kind of read that needs the writer lock.
    write_doc(&root, "b.md", "Beta", "beta store");
    let next = clean_plan(&root);
    let dir = index_dir(&root);
    fs::write(dir.join(INDEX_FILE), encoded(&next.snapshot)).expect("interrupt the pair");

    let before = dir_snapshot(&dir);
    let held = Lock::acquire(&dir, Duration::from_secs(10)).expect("hold the writer lock");
    let config = config(&root);
    let mut options = LoadOptions::new(&root, &config, BUILT_AT);
    options.lock_timeout = Duration::from_millis(10);
    let error = store::load(&options).expect_err("a held lock must time out");

    assert!(
        matches!(error, JanusError::LockTimeout { .. }),
        "got {error:?}"
    );
    assert_eq!(error.exit_code(), 1);
    assert!(
        error
            .to_string()
            .contains(&path_text(&dir.join(store::LOCK_FILE))),
        "the message must name the lock file: {error}"
    );
    assert_eq!(
        dir_snapshot(&dir),
        before,
        "a timed-out load starts no publish, so no temp file appears"
    );

    drop(held);
    let recovered = load(&root, &config);
    assert!(recovered.rebuilt, "the released lock lets the rebuild run");
    assert_eq!(encoded(&recovered.snapshot), encoded(&next.snapshot));
}

#[test]
fn an_interrupted_publish_is_recovered_by_a_rebuild_and_refused_without_one() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let first = clean_plan(&root);
    publish(&root, &first);
    let config = config(&root);

    // A publish that failed before the index rename leaves the previous
    // generation untouched: it loads with no rebuild and nothing to report.
    let untouched = load(&root, &config);
    assert!(!untouched.rebuilt);
    assert!(untouched.stale.is_none(), "the first read is not stale");
    assert_eq!(encoded(&untouched.snapshot), encoded(&first.snapshot));

    // The durable state a crash after the index rename leaves, built directly:
    // the new index bytes with the old manifest, and no publisher running.
    write_doc(&root, "b.md", "Beta", "alpha store body");
    let next = clean_plan(&root);
    let dir = index_dir(&root);
    fs::write(dir.join(INDEX_FILE), encoded(&next.snapshot)).expect("interrupt the pair");

    // `--no-rebuild`: refuse, name the index, write nothing at all.
    let before = dir_snapshot(&dir);
    let mut refuse = LoadOptions::new(&root, &config, BUILT_AT);
    refuse.policy = Policy::Refuse;
    let error = store::load(&refuse).expect_err("refusal must be an error");
    assert!(
        matches!(error, JanusError::IndexUnstable { .. }),
        "got {error:?}"
    );
    assert_eq!(error.exit_code(), 1);
    assert!(
        error
            .to_string()
            .contains(&path_text(&dir.join(INDEX_FILE))),
        "the message must name the index: {error}"
    );
    assert_eq!(
        dir_snapshot(&dir),
        before,
        "a refusal must leave .janus/ byte-identical"
    );

    // The same read with a rebuild allowed recovers from the vault.
    let recovered = load(&root, &config);
    assert!(recovered.rebuilt);
    assert_eq!(
        encoded(&recovered.snapshot),
        encoded(&next.snapshot),
        "recovery must equal a clean build"
    );
    let stale = recovered.stale.expect("the cause is reported");
    assert!(
        stale.contains(INDEX_FILE) && stale.contains("manifest.index_sha"),
        "the cause must name the mismatch: {stale}"
    );
}

#[test]
fn corrupt_bytes_rebuild_instead_of_panicking() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let clean = clean_plan(&root);
    publish(&root, &clean);
    let dir = index_dir(&root);
    let config = config(&root);

    let corruptions: [Corruption; 3] = [
        ("flip one byte", |bytes| {
            let middle = bytes.len() / 2;
            bytes[middle] ^= 1;
        }),
        ("truncate", |bytes| bytes.truncate(bytes.len() / 2)),
        ("zero the file", |bytes| bytes.fill(0)),
    ];
    for (name, corrupt) in corruptions {
        let mut bytes = fs::read(dir.join(INDEX_FILE)).expect("read the index");
        corrupt(&mut bytes);
        fs::write(dir.join(INDEX_FILE), &bytes).expect("corrupt the index");

        let recovered = load(&root, &config);
        assert!(recovered.rebuilt, "{name}: corruption must rebuild");
        assert_eq!(
            encoded(&recovered.snapshot),
            encoded(&clean.snapshot),
            "{name}: a rebuild equals a clean build"
        );
        assert!(recovered.stale.is_some(), "{name}: the cause is reported");
    }
}

#[test]
fn a_reader_storm_during_one_publish_never_sees_a_mix() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let pre = clean_plan(&root);
    publish(&root, &pre);
    write_doc(&root, "b.md", "Beta", "alpha store body");
    let post = clean_plan(&root);
    let pre_hits = hits(&pre.snapshot, "alpha");
    let post_hits = hits(&post.snapshot, "alpha");

    let mut readers = Vec::new();
    {
        let mut hook = |stage: Stage| {
            if stage == Stage::IndexRenamed {
                for _ in 0..8 {
                    let root = root.clone();
                    readers.push(thread::spawn(move || read(&root, "alpha")));
                }
            }
        };
        publish_hooked(&root, &post, &mut hook);
    }
    assert_eq!(readers.len(), 8);

    for reader in readers {
        let result = reader.join().expect("storm reader");
        assert!(!result.rebuilt, "a storm reader adopts, never rebuilds");
        assert!(
            result.hits == pre_hits || result.hits == post_hits,
            "a storm reader saw a mix: {:?}",
            result.hits
        );
    }
}

#[test]
fn the_index_rename_precedes_the_manifest_rename() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let pre = clean_plan(&root);
    publish(&root, &pre);
    write_doc(&root, "b.md", "Beta", "alpha store body");
    let post = clean_plan(&root);

    let dir = index_dir(&root);
    let post_sha = blake3::hash(&encoded(&post.snapshot)).to_hex().to_string();
    let old_manifest = fs::read(dir.join(MANIFEST_FILE)).expect("read the old manifest");
    let mut stages = Vec::new();
    {
        let mut hook = |stage: Stage| {
            stages.push(stage);
            if stage == Stage::IndexRenamed {
                // The new index is visible while the old manifest still names
                // the previous generation: the only mismatch a reader can meet.
                let on_disk = fs::read(dir.join(INDEX_FILE)).expect("read the new index");
                assert_eq!(blake3::hash(&on_disk).to_hex().to_string(), post_sha);
                assert_eq!(
                    fs::read(dir.join(MANIFEST_FILE)).expect("read the manifest"),
                    old_manifest
                );
            }
        };
        publish_hooked(&root, &post, &mut hook);
    }

    assert_eq!(
        stages,
        [
            Stage::IndexWritten,
            Stage::IndexRenamed,
            Stage::IndexDirSynced,
            Stage::ManifestWritten,
            Stage::ManifestRenamed,
            Stage::ManifestDirSynced,
        ]
    );
    let index_rename = stages
        .iter()
        .position(|stage| *stage == Stage::IndexRenamed)
        .expect("index rename");
    let manifest_rename = stages
        .iter()
        .position(|stage| *stage == Stage::ManifestRenamed)
        .expect("manifest rename");
    assert!(
        index_rename < manifest_rename,
        "the manifest must be renamed last"
    );
}
