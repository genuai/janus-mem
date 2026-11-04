//! The corpus signature, the witness, and the writer's window (M4D §3.1, §3.2).
//!
//! The witness is a pure function over a walk, so every case here builds the
//! `Vec<DiscoveredFile>` by hand and states the mtime in the test. No case reads
//! the clock or sleeps, and no case depends on the filesystem's mtime
//! granularity. The build tests use an embedder that writes or removes a
//! document on its first call: that is how a corpus changes *during* a build
//! without a thread and a sleep.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use janus_mem::config::{Config, resolve_index_dir};
use janus_mem::index::Snapshot;
use janus_mem::index::embed::{EmbedError, Embedder, EmbedderId, HashEmbedder};
use janus_mem::index::refresh::{self, Signature, Witness};
use janus_mem::index::store::{self, LoadOptions, Lock, MANIFEST_FILE, Publication};
use janus_mem::vault::{self, DiscoveredFile, WalkOptions};

/// A pinned build time, so no manifest depends on the clock.
const BUILT_AT: &str = "2025-04-01T00:00:00Z";

// ─────────────────────────────── the vault ────────────────────────────────

fn temp_vault(docs: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"staleness\"\n",
    )
    .expect("write config");
    for (rel_path, body) in docs {
        write_doc(dir.path(), rel_path, body);
    }
    dir
}

fn write_doc(root: &Path, rel_path: &str, body: &str) {
    let path = root.join(rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create document directory");
    }
    let title = rel_path.rsplit('/').next().unwrap_or(rel_path);
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

fn walk(root: &Path) -> vault::WalkResult {
    vault::walk(root, &config(root), WalkOptions::default()).expect("walk the vault")
}

fn read_manifest(root: &Path) -> store::Manifest {
    let bytes = fs::read(index_dir(root).join(MANIFEST_FILE)).expect("read the manifest");
    serde_json::from_slice(&bytes).expect("the manifest is JSON")
}

/// Builds and publishes one generation the way `janus index` does, and returns
/// the manifest. The digest is over the walk that produced the plan, which is
/// the contract the writer keeps (§3.2).
fn index(root: &Path) -> store::Manifest {
    let config = config(root);
    let dir = index_dir(root);
    let walk = walk(root);
    let plan = refresh::plan_from_walk(&config, &walk, &dir, None, &BTreeSet::new(), false)
        .expect("plan the vault");
    let digest = Signature::of(&walk.files).digest();
    store::publish(&Publication {
        dir: &dir,
        root,
        snapshot: &plan.snapshot,
        chunker: plan.chunker,
        vectors: plan.vectors.as_ref(),
        vectors_dropped: plan.vectors_dropped,
        built_at: BUILT_AT,
        corpus_sig: Some(&digest),
    })
    .expect("publish")
}

/// `mtime(index.bin)`, the fallback's other input.
fn index_mtime(root: &Path) -> SystemTime {
    fs::metadata(index_dir(root).join(store::INDEX_FILE))
        .expect("stat index.bin")
        .modified()
        .expect("index.bin has an mtime")
}

/// The witness for a walk, measured against the manifest on disk.
fn stale(root: &Path, files: &[DiscoveredFile]) -> bool {
    Witness::of(files, &read_manifest(root), Some(index_mtime(root))).stale
}

/// A copy of `files` where one file was touched to `mtime`.
fn touched(files: &[DiscoveredFile], mtime: SystemTime) -> Vec<DiscoveredFile> {
    let mut copy = files.to_vec();
    for file in &mut copy {
        file.mtime = mtime;
    }
    copy
}

// ──────────────────────────────── the digest ──────────────────────────────

#[test]
fn two_walks_of_an_unchanged_corpus_give_the_same_digest() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);

    let first = Signature::of(&walk(vault.path()).files);
    let second = Signature::of(&walk(vault.path()).files);

    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.len(), 2);
}

#[test]
fn one_more_byte_in_one_file_changes_the_digest() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    let files = walk(vault.path()).files;
    let mut longer = files.clone();
    longer[0].size += 1;

    assert_ne!(
        Signature::of(&files).digest(),
        Signature::of(&longer).digest()
    );
}

// ─────────────────────────────── the witness ──────────────────────────────

#[test]
fn the_manifest_records_the_digest_of_the_walk_that_built_it() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    let manifest = index(vault.path());

    assert_eq!(
        manifest.corpus_sig,
        Some(Signature::of(&walk(vault.path()).files).digest())
    );
}

#[test]
fn a_fresh_index_is_not_stale() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    index(vault.path());

    assert!(!stale(vault.path(), &walk(vault.path()).files));
}

#[test]
fn an_edited_document_makes_the_index_stale() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    index(vault.path());
    let mut files = walk(vault.path()).files;
    files[0].size += 5;
    files[0].mtime += Duration::from_secs(1);

    assert!(stale(vault.path(), &files));
}

#[test]
fn an_added_document_makes_the_index_stale() {
    let vault = temp_vault(&[("a.md", "alpha")]);
    index(vault.path());
    let mut files = walk(vault.path()).files;
    files.push(DiscoveredFile {
        path: vault.path().join("new.md"),
        rel_path: "new.md".to_owned(),
        size: 40,
        mtime: UNIX_EPOCH + Duration::from_secs(1_000),
    });

    assert!(stale(vault.path(), &files));
}

#[test]
fn a_removed_document_makes_the_index_stale() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    index(vault.path());
    let mut files = walk(vault.path()).files;
    files.retain(|file| file.rel_path != "notes/b.md");

    assert!(stale(vault.path(), &files));
}

#[test]
fn a_rename_that_keeps_the_mtime_makes_the_index_stale() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    index(vault.path());
    let mut files = walk(vault.path()).files;
    let moved = files
        .iter_mut()
        .find(|file| file.rel_path == "notes/b.md")
        .expect("the fixture has two documents");
    // A Unix rename keeps size and mtime. Only the path changes.
    moved.rel_path = "notes/renamed.md".to_owned();

    assert!(stale(vault.path(), &files));
}

#[test]
fn a_file_touched_to_an_older_time_makes_the_index_stale() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    index(vault.path());
    let mut files = walk(vault.path()).files;
    files[0].mtime -= Duration::from_secs(3_600);

    assert!(stale(vault.path(), &files));
}

#[test]
fn a_content_change_that_keeps_path_size_and_mtime_is_invisible() {
    let vault = temp_vault(&[("a.md", "alpha")]);
    let root = vault.path();
    index(root);

    // Rewrite the file with different bytes of the same length, then give it
    // back its old mtime. This is what `cp -a` or a backup restore does.
    let path = root.join("a.md");
    let before = fs::metadata(&path).expect("stat the document");
    let mut bytes = fs::read(&path).expect("read the document");
    let first = bytes[0];
    bytes[0] = if first == b'-' { b'#' } else { b'-' };
    fs::write(&path, &bytes).expect("rewrite the document");
    let restored = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open the document")
        .set_times(
            fs::FileTimes::new().set_modified(before.modified().expect("the document mtime")),
        );
    restored.expect("restore the document mtime");

    let after = fs::metadata(&path).expect("stat the document again");
    assert_ne!(first, bytes[0], "the content changed");
    assert_eq!(before.len(), after.len(), "the size did not change");
    assert_eq!(
        before.modified().expect("mtime"),
        after.modified().expect("mtime"),
        "the mtime did not change"
    );

    // The signature encodes path, size and mtime only, so this rewrite is the
    // named blind spot (§3.1, D-1b). A digest over content would report stale.
    assert!(!stale(root, &walk(root).files));
}

// ──────────────────────── the pre-M4D fallback ────────────────────────────
/// Rewrites the manifest without `corpus_sig`, which is what a build by a
/// binary that predates M4D leaves behind.
fn strip_digest(root: &Path) {
    let path = index_dir(root).join(MANIFEST_FILE);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("read the manifest"))
            .expect("manifest JSON");
    let object = manifest.as_object_mut().expect("the manifest is an object");
    assert!(
        object.remove("corpus_sig").is_some(),
        "the digest is present"
    );
    fs::write(
        &path,
        serde_json::to_vec_pretty(&manifest).expect("encode the manifest"),
    )
    .expect("write the manifest");
}

#[test]
fn an_older_manifest_falls_back_to_the_newest_mtime() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    index(vault.path());
    strip_digest(vault.path());
    let manifest = read_manifest(vault.path());
    assert!(manifest.corpus_sig.is_none(), "the digest is gone");

    let mtime = index_mtime(vault.path());
    let files = touched(&walk(vault.path()).files, mtime - Duration::from_secs(60));
    assert!(!stale(vault.path(), &files), "every file is older");

    let mut newer = files.clone();
    newer[0].mtime = mtime + Duration::from_secs(60);
    assert!(stale(vault.path(), &newer), "one file is newer");
}

#[test]
fn the_fallback_cannot_see_a_deletion() {
    let vault = temp_vault(&[("a.md", "alpha"), ("notes/b.md", "beta")]);
    index(vault.path());
    strip_digest(vault.path());

    let mtime = index_mtime(vault.path());
    let mut files = touched(&walk(vault.path()).files, mtime - Duration::from_secs(60));
    files.retain(|file| file.rel_path != "notes/b.md");

    // A deletion lowers no maximum, so the mtime rule misses it. The next
    // `janus index` writes the digest, and the digest sees it (§3.1).
    assert!(!stale(vault.path(), &files));
}

#[test]
fn the_fallback_treats_an_empty_corpus_over_a_non_empty_index_as_stale() {
    let vault = temp_vault(&[("a.md", "alpha")]);
    index(vault.path());
    strip_digest(vault.path());

    assert!(stale(vault.path(), &[]));
}

#[test]
fn the_fallback_is_strict_so_an_edit_in_the_publish_tick_is_fresh() {
    let vault = temp_vault(&[("a.md", "alpha")]);
    index(vault.path());
    strip_digest(vault.path());

    let mtime = index_mtime(vault.path());
    let files = touched(&walk(vault.path()).files, mtime);

    assert!(!stale(vault.path(), &files));
}

// ──────────────────── the writer closes its own window ────────────────────

/// An embedder that runs one action on every call, then delegates.
///
/// This is how a test edits the corpus *during* a build without a thread and a
/// sleep: the build is inside `embed_all` when the action runs. A caller that
/// wants one edit wraps the action in its own flag.
struct WritingEmbedder {
    id: EmbedderId,
    inner: HashEmbedder,
    action: Mutex<Box<dyn FnMut() + Send>>,
}

impl WritingEmbedder {
    fn new(action: Box<dyn FnMut() + Send>) -> Self {
        let inner = HashEmbedder::new(64).expect("64 is a usable width");
        Self {
            id: inner.id().clone(),
            inner,
            action: Mutex::new(action),
        }
    }
}

impl Embedder for WritingEmbedder {
    fn id(&self) -> &EmbedderId {
        &self.id
    }

    fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        (self.action.lock().expect("the action lock"))();
        self.inner.embed(batch)
    }
}

/// The `rel_path` of every document in a snapshot, in index order.
fn paths(snapshot: &Snapshot) -> Vec<String> {
    snapshot
        .docs
        .iter()
        .map(|doc| {
            snapshot
                .strings
                .get(doc.rel_path)
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

fn build(root: &Path, embedder: &dyn Embedder, walk: vault::WalkResult) -> store::BuildOutcome {
    let config = config(root);
    let dir = index_dir(root);
    let lock = Lock::acquire(&dir, store::LOCK_TIMEOUT).expect("the writer lock");
    store::build_until_stable(
        &dir,
        root,
        &config,
        Some(embedder),
        BUILT_AT,
        walk,
        &BTreeSet::new(),
        false,
        &lock,
    )
    .expect("build until stable")
}

#[test]
fn a_document_written_during_a_build_is_in_the_index_when_it_returns() {
    let vault = temp_vault(&[("a.md", "alpha text")]);
    let root = vault.path();
    let embedder = WritingEmbedder::new(Box::new({
        let root = root.to_path_buf();
        let mut written = false;
        move || {
            if !written {
                written = true;
                write_doc(&root, "late.md", "a document written mid-build");
            }
        }
    }));

    let outcome = build(root, &embedder, walk(root));

    assert_eq!(outcome.passes, 2, "the second pass carries the new file");
    let mut found = paths(&outcome.plan.snapshot);
    found.sort();
    assert_eq!(found, vec!["a.md".to_owned(), "late.md".to_owned()]);
    // The published digest describes the corpus the index now covers.
    assert_eq!(
        read_manifest(root).corpus_sig,
        Some(Signature::of(&walk(root).files).digest())
    );
}

#[test]
fn a_document_removed_during_a_build_leaves_the_index() {
    let vault = temp_vault(&[("a.md", "alpha text"), ("b.md", "beta text")]);
    let root = vault.path();
    let embedder = WritingEmbedder::new(Box::new({
        let root = root.to_path_buf();
        let mut removed = false;
        move || {
            if !removed {
                removed = true;
                fs::remove_file(root.join("a.md")).expect("remove a document mid-build");
            }
        }
    }));

    let outcome = build(root, &embedder, walk(root));

    assert_eq!(outcome.passes, 2);
    assert_eq!(paths(&outcome.plan.snapshot), vec!["b.md".to_owned()]);
}

#[test]
fn a_corpus_that_never_stops_moving_stops_after_three_passes() {
    let vault = temp_vault(&[("a.md", "alpha text")]);
    let root = vault.path();
    let next = std::sync::atomic::AtomicUsize::new(1);
    let embedder = WritingEmbedder::new(Box::new({
        let root = root.to_path_buf();
        move || {
            let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            write_doc(
                &root,
                &format!("late-{index}.md"),
                "written during the build",
            );
        }
    }));

    let outcome = build(root, &embedder, walk(root));

    assert_eq!(outcome.passes, store::MAX_PASSES);
    // The last pass was published; the files it never saw are the limit.
    let found = paths(&outcome.plan.snapshot);
    assert!(found.contains(&"a.md".to_owned()), "{found:?}");
    assert!(found.len() < walk(root).files.len(), "{found:?}");
}

#[test]
fn a_read_triggered_build_records_the_digest_too() {
    let vault = temp_vault(&[("a.md", "alpha text")]);
    let root = vault.path();
    let load = store::load(&LoadOptions::new(root, &config(root), BUILT_AT)).expect("a build");

    assert!(load.rebuilt, "the first read builds");
    assert_eq!(
        load.manifest.corpus_sig,
        Some(Signature::of(&walk(root).files).digest())
    );
    assert_eq!(paths(&load.snapshot), vec!["a.md".to_owned()]);
}

#[test]
fn a_read_triggered_build_keeps_a_document_written_during_it() {
    let vault = temp_vault(&[("a.md", "alpha text")]);
    let root = vault.path();
    let embedder = WritingEmbedder::new(Box::new({
        let root = root.to_path_buf();
        let mut written = false;
        move || {
            if !written {
                written = true;
                write_doc(&root, "late.md", "written during the build");
            }
        }
    }));

    let load = store::load(&LoadOptions {
        embedder: Some(&embedder),
        ..LoadOptions::new(root, &config(root), BUILT_AT)
    })
    .expect("load");

    // This assertion fails on a build that plans once from one walk (M4D §3.2).
    let mut found = paths(&load.snapshot);
    found.sort();
    assert_eq!(found, vec!["a.md".to_owned(), "late.md".to_owned()]);
}
