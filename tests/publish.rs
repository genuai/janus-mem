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
use janus_mem::index::embed::{Embedder, HashEmbedder};
use janus_mem::index::store::{
    self, INDEX_FILE, Load, LoadOptions, Lock, MANIFEST_FILE, Policy, Publication, Stage,
};
use janus_mem::index::vectors::{VECTORS_FILE, Vectors};
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
    plan_with(root, None)
}

/// The same, embedding every chunk through `embedder` when there is one.
fn plan_with(root: &Path, embedder: Option<&dyn Embedder>) -> BuildPlan {
    let config = config(root);
    let walk = vault::walk(root, &config, WalkOptions::default()).expect("walk the vault");
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, &config);
    identity::derive_identity(&mut documents, &config);
    build::plan(&config, &documents, None, embedder).expect("the fixture documents build")
}

fn publish(root: &Path, plan: &BuildPlan) -> store::Manifest {
    publish_hooked(root, plan, &mut |_stage| {})
}

fn publish_hooked(root: &Path, plan: &BuildPlan, hook: &mut dyn FnMut(Stage)) -> store::Manifest {
    let dir = index_dir(root);
    let publication = Publication {
        dir: &dir,
        root,
        snapshot: &plan.snapshot,
        chunker: plan.chunker,
        vectors: plan.vectors.as_ref(),
        vectors_dropped: plan.vectors_dropped,
        built_at: BUILT_AT,
        corpus_sig: None,
    };
    store::publish_with_hook(&publication, hook).expect("publish")
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

// ──────────────────────── the two-file generation (§3.1) ────────────────────

/// The deterministic test double, constructed directly: these cases are about
/// the store protocol, and `resolve` is covered by its own unit tests.
fn embedder() -> HashEmbedder {
    HashEmbedder::new(64).expect("64 is a valid dimension")
}

fn read_manifest(root: &Path) -> store::Manifest {
    let path = index_dir(root).join(MANIFEST_FILE);
    serde_json::from_slice(&fs::read(&path).expect("read the manifest"))
        .expect("the manifest is JSON")
}

/// Rewrites `manifest.json` in place, so a corrupt generation can be built
/// directly instead of staged by a race.
fn patch_manifest(root: &Path, patch: impl FnOnce(&mut serde_json::Value)) {
    let path = index_dir(root).join(MANIFEST_FILE);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read the manifest"))
            .expect("manifest JSON");
    patch(&mut manifest);
    fs::write(
        &path,
        serde_json::to_vec_pretty(&manifest).expect("encode the manifest"),
    )
    .expect("patch the manifest");
}

fn vectors_path(root: &Path) -> PathBuf {
    index_dir(root).join(VECTORS_FILE)
}

/// One read, with the two §3.1 switches a caller controls.
fn load_with(
    root: &Path,
    want_vectors: bool,
    embedder: Option<&dyn Embedder>,
) -> Result<Load, JanusError> {
    let config = config(root);
    let mut options = LoadOptions::new(root, &config, BUILT_AT);
    options.want_vectors = want_vectors;
    options.embedder = embedder;
    store::load(&options)
}

/// Writes only the new payload: the durable state a crash between the vectors
/// rename and the index rename leaves (§3.1 row 2).
fn crash_after_vectors_rename(root: &Path, next: &BuildPlan) {
    let rows = next.vectors.as_ref().expect("the plan has vectors").rows();
    fs::write(vectors_path(root), rows.encode()).expect("write new vectors");
}

/// Writes the new payload *and* the new index: the state a crash between the
/// index rename and the manifest rename leaves (§3.1 row 3).
fn crash_after_index_rename(root: &Path, next: &BuildPlan) {
    crash_after_vectors_rename(root, next);
    fs::write(index_dir(root).join(INDEX_FILE), encoded(&next.snapshot)).expect("write new index");
}

#[test]
fn publish_writes_the_payload_its_manifest_describes() {
    let vault = temp_vault(&[
        ("a.md", "Alpha", "alpha cache body"),
        ("b.md", "Beta", "beta store body"),
    ]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    let plan = plan_with(&root, Some(&embedder));
    assert_eq!(
        plan.stats.embedded_chunks,
        plan.snapshot.chunks.len(),
        "P1b embeds every chunk on every build"
    );
    let manifest = publish(&root, &plan);

    // 2 documents × 1 chunk × 64 dims × 4 bytes, derived by hand.
    let bytes = fs::read(vectors_path(&root)).expect("the payload exists");
    assert_eq!(bytes.len(), 2 * 64 * 4);
    assert_eq!(manifest.vectors_len, Some(bytes.len() as u64));
    assert_eq!(manifest.chunks, 2);
    assert_eq!(
        manifest.vectors_sha,
        Some(blake3::hash(&bytes).to_hex().to_string()),
        "vectors_sha names the bytes actually written"
    );
    assert_eq!(manifest.embedder.as_ref(), Some(embedder.id()));

    let rows = plan
        .vectors
        .as_ref()
        .expect("a plan with an embedder")
        .rows();
    let decoded = Vectors::decode(&bytes, 64, plan.snapshot.chunks.len())
        .expect("the published payload validates against the manifest's shape");
    assert_eq!(&decoded, rows, "row i belongs to chunk i");
}

#[test]
fn the_vectors_rename_precedes_the_index_and_manifest_renames() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));

    write_doc(&root, "b.md", "Beta", "alpha store body");
    let post = plan_with(&root, Some(&embedder));
    let dir = index_dir(&root);
    let old_index = fs::read(dir.join(INDEX_FILE)).expect("read the old index");
    let old_manifest = fs::read(dir.join(MANIFEST_FILE)).expect("read the old manifest");
    let post_sha = post
        .vectors
        .as_ref()
        .map(|set| blake3::hash(&set.rows().encode()).to_hex().to_string())
        .expect("vectors");

    let mut stages = Vec::new();
    {
        let mut hook = |stage: Stage| {
            stages.push(stage);
            if stage == Stage::VectorsRenamed {
                // The new payload is visible while the pair the manifest names
                // is still the previous generation: row 2 of §3.1's table.
                let on_disk = fs::read(dir.join(VECTORS_FILE)).expect("read new vectors");
                assert_eq!(blake3::hash(&on_disk).to_hex().to_string(), post_sha);
                assert_eq!(
                    fs::read(dir.join(INDEX_FILE)).expect("read index"),
                    old_index
                );
                assert_eq!(
                    fs::read(dir.join(MANIFEST_FILE)).expect("read manifest"),
                    old_manifest
                );
            }
        };
        publish_hooked(&root, &post, &mut hook);
    }

    assert_eq!(
        stages,
        [
            Stage::VectorsWritten,
            Stage::VectorsRenamed,
            Stage::VectorsDirSynced,
            Stage::IndexWritten,
            Stage::IndexRenamed,
            Stage::IndexDirSynced,
            Stage::ManifestWritten,
            Stage::ManifestRenamed,
            Stage::ManifestDirSynced,
        ],
        "vectors first, index next, manifest (the commit record) last"
    );
}

#[test]
fn a_complete_generation_loads_in_both_read_modes() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    let first = plan_with(&root, Some(&embedder));
    publish(&root, &first);
    let dir = index_dir(&root);

    // Row 4: the new triple loads.
    let before = dir_snapshot(&dir);
    let lexical = load_with(&root, false, Some(&embedder)).expect("load");
    assert!(!lexical.rebuilt);
    assert!(lexical.stale.is_none());
    assert!(lexical.vectors.is_none(), "a lexical read gets no rows");

    let wanting = load_with(&root, true, Some(&embedder)).expect("load");
    assert!(!wanting.rebuilt);
    assert_eq!(
        wanting.vectors.as_ref(),
        first.vectors.as_ref().map(|set| set.rows()),
        "the reader gets the rows the writer wrote"
    );
    assert_eq!(dir_snapshot(&dir), before, "reading writes nothing");

    // Row 6: no embedder resolved, vectors-bearing generation. It loads, keeps
    // the payload, and reports no staleness: strictly more information than
    // this process reads is never a reason to destroy it (V-21).
    let degraded = load_with(&root, false, None).expect("load");
    assert!(!degraded.rebuilt);
    assert!(degraded.stale.is_none());
    assert_eq!(degraded.manifest.embedder.as_ref(), Some(embedder.id()));
    assert_eq!(dir_snapshot(&dir), before, "row 6 leaves .janus/ identical");

    // Row 1: the previous triple is also a complete one. A second publish then a
    // read sees a complete generation again.
    write_doc(&root, "b.md", "Beta", "alpha store body");
    let second = plan_with(&root, Some(&embedder));
    publish(&root, &second);
    let after = dir_snapshot(&dir);
    let lexical = load_with(&root, false, None).expect("load");
    assert!(!lexical.rebuilt);
    assert!(lexical.stale.is_none());
    let wanting = load_with(&root, true, Some(&embedder)).expect("load");
    assert!(!wanting.rebuilt);
    assert_eq!(wanting.manifest.chunks, 2);
    assert_eq!(dir_snapshot(&dir), after);
}

#[test]
fn a_new_vectors_old_index_pair_rebuilds_only_for_a_reader_that_wants_vectors() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));

    write_doc(&root, "b.md", "Beta", "alpha store body");
    let next = plan_with(&root, Some(&embedder));
    let dir = index_dir(&root);
    crash_after_vectors_rename(&root, &next);
    let crashed = dir_snapshot(&dir);

    // The same read, lexical only: the pair it validated agrees with itself, so
    // it loads and never looks at the payload it declined to read (V-4).
    let lexical = load_with(&root, false, Some(&embedder)).expect("load");
    assert!(!lexical.rebuilt, "row 2's lexical path must not rebuild");
    assert!(lexical.stale.is_none());
    assert!(lexical.vectors.is_none());
    assert_eq!(dir_snapshot(&dir), crashed, "a lexical read writes nothing");

    // `--no-rebuild` on the wanting path refuses and names the payload.
    let config = config(&root);
    let mut refuse = LoadOptions::new(&root, &config, BUILT_AT);
    refuse.want_vectors = true;
    refuse.embedder = Some(&embedder);
    refuse.policy = Policy::Refuse;
    let error = store::load(&refuse).expect_err("a mismatch must refuse");
    assert!(
        matches!(error, JanusError::IndexUnstable { .. }),
        "got {error:?}"
    );
    assert!(
        error
            .to_string()
            .contains(&path_text(&dir.join(VECTORS_FILE))),
        "the refusal must name the payload: {error}"
    );
    assert_eq!(
        dir_snapshot(&dir),
        crashed,
        "a refusal leaves .janus/ byte-identical"
    );

    // The wanting read rebuilds and publishes a complete pair.
    let rebuilt = load_with(&root, true, Some(&embedder)).expect("load");
    assert!(rebuilt.rebuilt);
    let stale = rebuilt.stale.expect("the cause is reported");
    assert!(stale.contains(VECTORS_FILE), "{stale}");
    assert!(stale.contains("manifest.vectors_sha"), "{stale}");
    assert_eq!(
        rebuilt.snapshot.chunks.len(),
        next.snapshot.chunks.len(),
        "the rebuild saw the new document"
    );
    assert_eq!(
        fs::read(vectors_path(&root)).expect("payload"),
        next.vectors.as_ref().expect("vectors").rows().encode(),
        "the rebuilt payload equals a clean build's"
    );
}

#[test]
fn a_new_index_old_manifest_pair_rebuilds_in_both_read_modes() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));
    let dir = index_dir(&root);
    let old_manifest = fs::read(dir.join(MANIFEST_FILE)).expect("read the old manifest");

    write_doc(&root, "b.md", "Beta", "alpha store body");
    let next = plan_with(&root, Some(&embedder));
    crash_after_index_rename(&root, &next);
    let crashed = dir_snapshot(&dir);

    let config = config(&root);
    for want in [false, true] {
        let mut options = LoadOptions::new(&root, &config, BUILT_AT);
        options.want_vectors = want;
        options.embedder = Some(&embedder);
        options.policy = Policy::Refuse;
        let error = store::load(&options).expect_err("a mismatch must refuse");
        assert!(
            error
                .to_string()
                .contains(&path_text(&dir.join(INDEX_FILE))),
            "want_vectors={want}: the refusal must name the index: {error}"
        );
        assert_eq!(dir_snapshot(&dir), crashed);
    }

    for want in [false, true] {
        // Each mode starts from the crashed state, because the first rebuild
        // publishes a generation the second read would rightly adopt.
        crash_after_index_rename(&root, &next);
        fs::write(dir.join(MANIFEST_FILE), &old_manifest).expect("restore the old manifest");
        let rebuilt = load_with(&root, want, Some(&embedder)).expect("load");
        assert!(rebuilt.rebuilt, "want_vectors={want}: row 3 rebuilds");
        let stale = rebuilt.stale.expect("the cause is reported");
        assert!(stale.contains(INDEX_FILE), "{stale}");
        assert!(stale.contains("manifest.index_sha"), "{stale}");
        assert_eq!(
            rebuilt.vectors.is_some(),
            want,
            "want_vectors={want}: rows come back only when asked for"
        );
    }
}

#[test]
fn a_lexical_read_never_opens_the_vectors_file() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));

    // The manifest still names a payload that is gone: harmless to a read that
    // will not use one, fatal to a read that will (V-4).
    fs::remove_file(vectors_path(&root)).expect("remove the payload");
    let lexical = load_with(&root, false, Some(&embedder)).expect("load");
    assert!(!lexical.rebuilt);
    assert!(lexical.stale.is_none());

    let wanting = load_with(&root, true, Some(&embedder)).expect("load");
    assert!(wanting.rebuilt, "a missing payload rebuilds");
    assert!(
        wanting
            .stale
            .expect("cause")
            .contains(&path_text(&vectors_path(&root))),
        "the cause names the payload"
    );
}

#[test]
fn a_corrupt_payload_rebuilds_and_never_misaligns_a_row() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));
    let good = fs::read(vectors_path(&root)).expect("payload");

    let corruptions: [Corruption; 3] = [
        ("flip one byte", |bytes| {
            let middle = bytes.len() / 2;
            bytes[middle] ^= 1;
        }),
        ("truncate", |bytes| bytes.truncate(bytes.len() / 2)),
        ("zero the file", |bytes| bytes.fill(0)),
    ];
    for (name, corrupt) in corruptions {
        let mut bytes = good.clone();
        corrupt(&mut bytes);
        fs::write(vectors_path(&root), &bytes).expect("corrupt the payload");

        let load = load_with(&root, true, Some(&embedder)).expect("load");
        assert!(load.rebuilt, "{name}: corruption must rebuild");
        assert!(
            load.stale.expect("cause").contains(VECTORS_FILE),
            "{name}: the cause names the payload"
        );
        assert_eq!(
            fs::read(vectors_path(&root)).expect("payload"),
            good,
            "{name}: a rebuild equals a clean build"
        );
    }
}

#[test]
fn a_payload_whose_manifest_shape_disagrees_rebuilds() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));

    // One extra (all-zero) row: the bytes and the manifest agree with each
    // other, but the row count disagrees with the snapshot.
    let mut bytes = fs::read(vectors_path(&root)).expect("payload");
    bytes.extend_from_slice(&vec![0u8; 64 * 4]);
    fs::write(vectors_path(&root), &bytes).expect("append a row");
    let sha = blake3::hash(&bytes).to_hex().to_string();
    let len = bytes.len() as u64;
    patch_manifest(&root, |manifest| {
        manifest["vectors_sha"] = serde_json::json!(sha);
        manifest["vectors_len"] = serde_json::json!(len);
    });

    let load = load_with(&root, true, Some(&embedder)).expect("load");
    assert!(load.rebuilt);
    let stale = load.stale.expect("cause");
    assert!(stale.contains(VECTORS_FILE), "{stale}");
    assert!(stale.contains("does not match"), "{stale}");
}

#[test]
fn a_format_version_2_manifest_is_stale_and_the_rebuild_writes_version_3() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    publish(&root, &clean_plan(&root));
    patch_manifest(&root, |manifest| {
        manifest["format_version"] = serde_json::json!(2)
    });

    let load = load_with(&root, false, None).expect("load");
    assert!(load.rebuilt, "an M4 index is refused and rebuilt");
    let stale = load.stale.expect("cause");
    assert!(stale.contains("format_version 2 != 3"), "{stale}");
    assert_eq!(
        read_manifest(&root).format_version,
        janus_mem::index::FORMAT_VERSION
    );
}

#[test]
fn a_manifest_holding_two_of_three_vector_fields_is_corrupt() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));
    patch_manifest(&root, |manifest| {
        manifest["vectors_sha"] = serde_json::Value::Null
    });

    let load = load_with(&root, false, None).expect("load");
    assert!(load.rebuilt);
    let stale = load.stale.expect("cause");
    assert!(stale.contains("vector fields disagree"), "{stale}");
    assert!(stale.contains("vectors_sha absent"), "{stale}");
}

#[test]
fn a_vectors_less_generation_is_stale_for_a_process_that_can_embed() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    publish(&root, &clean_plan(&root));
    assert!(!vectors_path(&root).exists());
    let dir = index_dir(&root);

    // `--no-rebuild` refuses without touching the generation.
    let before = dir_snapshot(&dir);
    let embedder = embedder();
    let config = config(&root);
    let mut refuse = LoadOptions::new(&root, &config, BUILT_AT);
    refuse.embedder = Some(&embedder);
    refuse.policy = Policy::Refuse;
    let error = store::load(&refuse).expect_err("row 5 must refuse without a rebuild");
    assert!(matches!(error, JanusError::IndexUnstable { .. }));
    assert_eq!(dir_snapshot(&dir), before);

    // Allowed to rebuild: one build, then the generation has vectors.
    let load = load_with(&root, true, Some(&embedder)).expect("load");
    assert!(load.rebuilt);
    let stale = load.stale.expect("cause");
    assert!(stale.contains("no vectors"), "{stale}");
    assert_eq!(read_manifest(&root).embedder.as_ref(), Some(embedder.id()));
    assert!(vectors_path(&root).exists());

    // …and it is stable: the next read adopts it.
    let again = load_with(&root, true, Some(&embedder)).expect("load");
    assert!(!again.rebuilt);
    assert!(again.stale.is_none());
}

#[test]
fn a_degraded_publish_drops_the_payload_and_the_next_lexical_read_loads() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));
    assert!(vectors_path(&root).exists());

    publish(&root, &clean_plan(&root));
    assert!(
        !vectors_path(&root).exists(),
        "a build without an embedder cannot describe the rows, so they go"
    );
    let manifest = read_manifest(&root);
    assert!(manifest.embedder.is_none());
    assert!(manifest.vectors_sha.is_none());
    assert!(manifest.vectors_len.is_none());

    // The next read with no embedder resolves to `embedder: None` and loads,
    // under `--no-rebuild` too: nothing is stale.
    let load = load_with(&root, false, None).expect("load");
    assert!(!load.rebuilt);
    assert!(load.stale.is_none());
    assert!(load.manifest.embedder.is_none());

    let config = config(&root);
    let mut refuse = LoadOptions::new(&root, &config, BUILT_AT);
    refuse.policy = Policy::Refuse;
    let load = store::load(&refuse).expect("a vectors-less generation is not stale for us");
    assert!(!load.rebuilt);
}

/// The embedder name the on-disk manifest currently records, if any.
fn on_disk_embedder(root: &Path) -> Option<String> {
    let path = index_dir(root).join(MANIFEST_FILE);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read the manifest"))
            .expect("the manifest is JSON");
    manifest["embedder"]["name"].as_str().map(str::to_owned)
}

#[test]
fn a_degraded_publish_drops_the_payload_after_the_commit_record() {
    let vault = temp_vault(&[("a.md", "Alpha", "alpha cache body")]);
    let root = vault.path().to_path_buf();
    let embedder = embedder();
    publish(&root, &plan_with(&root, Some(&embedder)));
    let payload = vectors_path(&root);
    assert!(payload.exists());

    let plan = clean_plan(&root);
    let mut stages = Vec::new();
    {
        let mut hook = |stage: Stage| {
            stages.push(stage);
            if stage == Stage::VectorsDropped {
                assert!(!payload.exists(), "the drop is the last stage");
                return;
            }
            // A reader that wants vectors must never meet a manifest naming a
            // file that is already gone. The old manifest is authoritative
            // until its rename, and the payload outlives it by one step.
            assert!(
                payload.exists(),
                "{stage:?}: the payload vanished before the commit record changed"
            );
            if matches!(stage, Stage::ManifestRenamed | Stage::ManifestDirSynced) {
                assert_eq!(on_disk_embedder(&root), None, "{stage:?}");
            } else {
                assert_eq!(
                    on_disk_embedder(&root).as_deref(),
                    Some("hash-64"),
                    "{stage:?}: the old manifest still names the payload"
                );
            }
        };
        publish_hooked(&root, &plan, &mut hook);
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
            Stage::VectorsDropped,
        ],
        "a degraded publish removes the payload only after the commit record"
    );
    assert!(!payload.exists());
}
