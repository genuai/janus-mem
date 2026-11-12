//! Cases 2.4, 2.4a, 2.5, 2.6 and 3.1 to 3.5: the atomic replace and the corpus
//! lock (§3.2 and §3.3 of the M7 plan).
//!
//! Every case builds its own corpus in a temporary directory and calls the
//! library. No case sleeps, starts a second process, reads the clock or looks at
//! `$HOME`. The only thread is the test thread, so a "concurrent writer" is a
//! second write to the same file between two calls, which is exactly the race
//! the two hash checks exist for (M7-11).
//!
//! The lock cases acquire the same `store::Lock` the index builder takes, in one
//! process. `flock` and `LockFileEx` both refuse a second handle, so the second
//! acquisition waits and then fails on the timeout.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use janus_mem::api::{Call, Tool};
use janus_mem::error::JanusError;
use janus_mem::id::Blake3Hash;
use janus_mem::index::store::{LOCK_FILE, LOCK_TIMEOUT, Lock};
use janus_mem::write::{
    self, CommitError, CommitOptions, Committed, Hooks, NoHooks, Patch, Plan, commit,
};
use serde_json::json;

/// The document every case replaces.
const DOC: &str = "notes/decision.md";

/// The bytes the document holds before a commit.
const ORIGINAL: &str = "---\ntitle: DB choice\ndate: 2025-01-01\ntype: note\nstatus: active\n---\n\nWe choose Postgres.\n";

/// The bytes a second writer leaves behind.
const ELSEWHERE: &str = "---\ntitle: DB choice\ndate: 2025-01-01\ntype: note\nstatus: retired\n---\n\nAnother writer edited this.\n";

/// A timeout short enough to keep a busy case quick, long enough to be a wait.
const SHORT: Duration = Duration::from_millis(20);

// ──────────────────────────────── helpers ──────────────────────────────────

/// A corpus of one document, with the config a walk needs.
fn corpus() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"write-atomic\"\n",
    )
    .expect("write the config");
    common::write_document(dir.path(), DOC, ORIGINAL);
    dir
}

/// The document's path inside `root`.
fn doc(root: &Path) -> PathBuf {
    root.join(DOC)
}

/// The names of the entries in the document's directory, sorted.
fn entries(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root.join("notes"))
        .expect("read the document's directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// A plan that sets `status` to `value` against the bytes on disk now.
fn planned(root: &Path, value: &str) -> Plan {
    let text = fs::read_to_string(doc(root)).expect("read the document");
    let patch = Patch {
        path: doc(root),
        keys: vec![("status".to_owned(), Some(value.to_owned()))],
        expected_hash: None,
        create_frontmatter: false,
        dry_run: false,
    };
    write::plan(&text, &patch).expect("the status patch plans")
}

/// The options a case commits with.
fn options<'a>(root: &Path, hooks: &'a dyn Hooks, timeout: Duration) -> CommitOptions<'a> {
    CommitOptions {
        path: doc(root),
        index_dir: root.join(".janus"),
        timeout,
        hooks,
    }
}

/// Commits with no seam and the default timeout, asserting success.
fn committed(root: &Path, plan: &Plan) -> Committed {
    commit(plan, &options(root, &NoHooks, LOCK_TIMEOUT)).expect("the commit must succeed")
}

/// A hook that writes another writer's bytes after the pre-lock check passed.
struct EditsAfterPreLock {
    path: PathBuf,
    bytes: &'static str,
}

impl Hooks for EditsAfterPreLock {
    fn after_pre_lock(&self, path: &Path) {
        assert_eq!(path, self.path, "the seam names the document it edits");
        fs::write(&self.path, self.bytes).expect("the second writer's bytes");
    }

    fn before_second_rename(&self, _path: &Path) {
        panic!("a commit of one file never renames twice (M7-6)");
    }
}

// ─────────────────────────────── the checks ────────────────────────────────

#[test]
fn case_2_4_a_change_before_the_commit_is_a_conflict() {
    let dir = corpus();
    let root = dir.path();
    let plan = planned(root, "superseded");

    // Another writer changes the file between the plan and the commit.
    fs::write(doc(root), ELSEWHERE).expect("the second writer's bytes");

    match commit(&plan, &options(root, &NoHooks, LOCK_TIMEOUT)) {
        Err(CommitError::Conflict { expected, current }) => {
            assert_eq!(
                expected, plan.old_hash,
                "the conflict names what was expected"
            );
            assert_eq!(current, Blake3Hash::of_bytes(ELSEWHERE.as_bytes()).to_hex());
        }
        other => panic!("a changed file must answer Conflict, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(doc(root)).expect("read back"),
        ELSEWHERE,
        "the other writer's bytes must survive"
    );
    assert_eq!(entries(root), ["decision.md"], "no temp file may be left");
}

#[test]
fn case_2_4a_a_change_under_the_lock_is_a_conflict() {
    let dir = corpus();
    let root = dir.path();
    let plan = planned(root, "superseded");
    let hook = EditsAfterPreLock {
        path: doc(root),
        bytes: ELSEWHERE,
    };

    // The pre-lock check sees the plan's bytes. The hook then changes the file,
    // so only the under-lock re-read can catch it.
    match commit(&plan, &options(root, &hook, LOCK_TIMEOUT)) {
        Err(CommitError::Conflict { expected, current }) => {
            assert_eq!(expected, plan.old_hash);
            assert_eq!(current, Blake3Hash::of_bytes(ELSEWHERE.as_bytes()).to_hex());
        }
        other => panic!("a file changed under the lock must answer Conflict, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(doc(root)).expect("read back"),
        ELSEWHERE,
        "the commit must write nothing when the authoritative check fails"
    );
    assert_eq!(entries(root), ["decision.md"], "no temp file may be left");
}

#[test]
fn case_2_5_one_etag_wins_and_the_lock_is_the_corpus_lock() {
    let dir = corpus();
    let root = dir.path();
    let lock_path = root.join(".janus").join(LOCK_FILE);
    let plan = planned(root, "superseded");

    // The first commit takes the lock and writes.
    let first = committed(root, &plan);
    assert_eq!(first.old_hash, plan.old_hash);
    assert_eq!(first.new_hash, plan.new_hash);
    assert!(
        lock_path.is_file(),
        "the commit must take {}",
        lock_path.display()
    );

    // The second presents the same etag, which is now stale.
    match commit(&plan, &options(root, &NoHooks, LOCK_TIMEOUT)) {
        Err(CommitError::Conflict { expected, current }) => {
            assert_eq!(expected, plan.old_hash);
            assert_eq!(current, first.new_hash);
        }
        other => panic!("the second commit must answer Conflict, got {other:?}"),
    }

    // A second acquisition in this process waits, then fails on the timeout.
    let held = Lock::acquire(&root.join(".janus"), LOCK_TIMEOUT).expect("the lock is free again");
    match Lock::acquire(&root.join(".janus"), SHORT) {
        Err(JanusError::LockTimeout { path }) => assert_eq!(path, lock_path),
        other => panic!("a held lock must time out, got {other:?}"),
    }

    // While the lock is held, a commit with bytes that are current answers Busy
    // and writes nothing.
    let busy_plan = planned(root, "active");
    match commit(&busy_plan, &options(root, &NoHooks, SHORT)) {
        Err(CommitError::Busy { path }) => assert_eq!(path, lock_path),
        other => panic!("a busy corpus must answer Busy, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(doc(root)).expect("read back"),
        plan.text,
        "a busy commit must write nothing"
    );
    drop(held);
}

#[test]
fn case_2_6_the_new_hash_is_the_next_reads_content_hash() {
    let dir = corpus();
    let root = dir.path();
    let plan = planned(root, "superseded");
    let done = committed(root, &plan);

    let engine = common::engine(root);
    let response = engine.call(&Call::new(Tool::ReadDocument, json!({ "path": DOC })));
    assert!(response.ok, "the read failed: {}", response.json);
    assert_eq!(
        response.json["content_hash"],
        json!(done.new_hash),
        "the receipt's new_hash must be what the next read reports"
    );
    assert_eq!(response.json["content_hash"], json!(plan.new_hash));
}

// ────────────────────────────── the replace ────────────────────────────────

#[test]
fn case_3_1_a_successful_write_leaves_no_temp_file() {
    let dir = corpus();
    let root = dir.path();
    let plan = planned(root, "superseded");
    assert_eq!(entries(root), ["decision.md"]);

    let done = committed(root, &plan);

    assert_eq!(
        fs::read_to_string(doc(root)).expect("read back"),
        plan.text,
        "the document must hold the plan's bytes"
    );
    assert_eq!(done.bytes_after, plan.text.len());
    assert_eq!(entries(root), ["decision.md"], "no temp file may survive");
}

#[test]
fn case_3_2_a_refused_write_leaves_no_temp_file() {
    let dir = corpus();
    let root = dir.path();
    let plan = planned(root, "superseded");
    fs::write(doc(root), ELSEWHERE).expect("the second writer's bytes");

    let answer = commit(&plan, &options(root, &NoHooks, LOCK_TIMEOUT));
    assert!(answer.is_err(), "a stale plan must be refused");
    assert_eq!(entries(root), ["decision.md"], "a refusal stages no file");
    assert_eq!(
        fs::read(doc(root)).expect("read back"),
        ELSEWHERE.as_bytes(),
        "a refusal leaves the bytes alone"
    );

    // A failed replace leaves no temp file either. The temp file exists before
    // the rename, and a rename over a directory fails after staging.
    let blocked = root.join("notes/blocked");
    fs::create_dir(&blocked).expect("create the directory the rename cannot replace");
    match write::replace(&blocked, b"unreachable") {
        Err(CommitError::Io { .. }) => {}
        other => panic!("a rename over a directory must fail, got {other:?}"),
    }
    assert_eq!(
        entries(root),
        ["blocked", "decision.md"],
        "a failed replace must remove its temp file"
    );
}

#[cfg(unix)]
#[test]
fn case_3_3_the_mode_survives_and_the_inode_changes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let dir = corpus();
    let root = dir.path();
    fs::set_permissions(doc(root), fs::Permissions::from_mode(0o640)).expect("set the mode");
    let before = fs::metadata(doc(root)).expect("stat the document");
    assert_eq!(before.permissions().mode() & 0o777, 0o640);

    let plan = planned(root, "superseded");
    committed(root, &plan);

    let after = fs::metadata(doc(root)).expect("stat the document");
    assert_eq!(
        after.permissions().mode() & 0o777,
        0o640,
        "the replaced file must keep the mode it had"
    );
    assert_ne!(
        after.ino(),
        before.ino(),
        "a new inode proves the replace was a rename, not a truncate"
    );
}

#[cfg(unix)]
#[test]
fn case_3_4_a_directory_without_write_permission_fails_and_keeps_the_bytes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = corpus();
    let root = dir.path();
    let notes = root.join("notes");
    let plan = planned(root, "superseded");
    let before = fs::read(doc(root)).expect("read the document");

    fs::set_permissions(&notes, fs::Permissions::from_mode(0o500)).expect("close the directory");
    let answer = commit(&plan, &options(root, &NoHooks, LOCK_TIMEOUT));
    // Restore the mode before asserting, so a failure leaves a readable temp dir.
    fs::set_permissions(&notes, fs::Permissions::from_mode(0o700)).expect("reopen the directory");

    match answer {
        Err(error @ CommitError::Io { .. }) => {
            assert!(
                error.to_string().contains("notes"),
                "the failure must name the path: {error}"
            );
        }
        other => panic!("an unwritable directory must fail with Io, got {other:?}"),
    }
    assert_eq!(fs::read(doc(root)).expect("read back"), before);
    assert_eq!(entries(root), ["decision.md"], "a failure stages no file");
}

#[test]
fn case_3_5_the_parent_directory_is_synced_through_one_function() {
    let dir = corpus();
    let root = dir.path();
    let plan = planned(root, "superseded");
    committed(root, &plan);

    // The bytes are on disk, so the effect of the directory sync is not
    // observable and this case does not claim it. What it pins is the one
    // function that performs it: it answers for the document's own directory,
    // and it names the path when the directory is gone.
    write::sync_parent_dir(&root.join("notes")).expect("a directory on this platform syncs");
    match write::sync_parent_dir(&root.join("gone")) {
        Err(CommitError::Io { path, .. }) => assert_eq!(path, root.join("gone")),
        other => panic!("a vanished directory must answer Io, got {other:?}"),
    }
}
