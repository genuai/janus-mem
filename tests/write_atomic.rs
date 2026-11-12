//! Cases 2.4, 2.4a, 2.4b, 2.4c, 2.5, 2.6 and 3.1 to 3.6: the atomic replace and
//! the corpus lock (§3.2 and §3.3 of the M7 plan).
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
use janus_mem::config::Config;
use janus_mem::error::JanusError;
use janus_mem::id::Blake3Hash;
use janus_mem::index::store::{LOCK_FILE, LOCK_TIMEOUT, Lock};
use janus_mem::write::{
    self, CommitError, CommitOptions, Committed, Hooks, Mode, NoHooks, Patch, Plan, commit,
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

/// A corpus of one document, its config, and the options a case commits with.
struct Fixture {
    dir: tempfile::TempDir,
    config: Config,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("janus.toml"),
            "index_dir = \".janus\"\ncorpus_id = \"write-atomic\"\n",
        )
        .expect("write the config");
        common::write_document(dir.path(), DOC, ORIGINAL);
        let config = Config::load(dir.path()).expect("the corpus config");
        Self { dir, config }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The document's path inside the corpus.
    fn doc(&self) -> PathBuf {
        self.root().join(DOC)
    }

    /// The options for [`Self::doc`], with the default timeout.
    fn options<'a>(&'a self, hooks: &'a dyn Hooks) -> CommitOptions<'a> {
        self.options_at(self.doc(), hooks)
    }

    /// The options for another path in the same corpus.
    fn options_at<'a>(&'a self, path: PathBuf, hooks: &'a dyn Hooks) -> CommitOptions<'a> {
        CommitOptions {
            path,
            root: self.root().to_path_buf(),
            config: &self.config,
            timeout: LOCK_TIMEOUT,
            hooks,
        }
    }

    /// The same options, with a timeout that expires while the lock is held.
    fn impatient<'a>(&'a self, path: PathBuf, hooks: &'a dyn Hooks) -> CommitOptions<'a> {
        let mut options = self.options_at(path, hooks);
        options.timeout = SHORT;
        options
    }

    /// The names of the entries in the document's directory, sorted.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.root().join("notes"))
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

    /// The document's bytes, as text.
    fn text(&self) -> String {
        fs::read_to_string(self.doc()).expect("read the document")
    }

    /// Commits with no seam and the default timeout, asserting success.
    fn committed(&self, plan: &Plan) -> Committed {
        commit(plan, &self.options(&NoHooks)).expect("the commit must succeed")
    }
}

/// A plan for one `status` value against `text`.
fn plan_from(text: &str, config: &Config, value: &str) -> Plan {
    let patch = Patch {
        path: PathBuf::from(DOC),
        keys: vec![("status".to_owned(), Some(value.to_owned()))],
        expected_hash: None,
        create_frontmatter: false,
        dry_run: false,
    };
    write::plan(text, config, &patch).expect("the status patch plans")
}

/// A plan that sets `status` to `value` against the document's bytes now.
fn planned(fixture: &Fixture, value: &str) -> Plan {
    plan_from(&fixture.text(), &fixture.config, value)
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

/// A hook that would fail the case if the second seam were ever called.
struct PanicsOnSecondRename;

impl Hooks for PanicsOnSecondRename {
    fn after_pre_lock(&self, _path: &Path) {}

    fn before_second_rename(&self, _path: &Path) {
        panic!("a commit of one file never renames twice (M7-6)");
    }
}

// ─────────────────────────────── the checks ────────────────────────────────

#[test]
fn case_2_4_a_change_before_the_commit_is_a_conflict() {
    let fixture = Fixture::new();
    let plan = planned(&fixture, "superseded");

    // Another writer changes the file between the plan and the commit.
    fs::write(fixture.doc(), ELSEWHERE).expect("the second writer's bytes");

    match commit(&plan, &fixture.options(&NoHooks)) {
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
        fixture.text(),
        ELSEWHERE,
        "the other writer's bytes must survive"
    );
    assert_eq!(
        fixture.entries(),
        ["decision.md"],
        "no temp file may be left"
    );
}

#[test]
fn case_2_4a_a_change_under_the_lock_is_a_conflict() {
    let fixture = Fixture::new();
    let plan = planned(&fixture, "superseded");
    let hook = EditsAfterPreLock {
        path: fixture.doc(),
        bytes: ELSEWHERE,
    };

    // The pre-lock check sees the plan's bytes. The hook then changes the file,
    // so only the under-lock re-read can catch it.
    match commit(&plan, &fixture.options(&hook)) {
        Err(CommitError::Conflict { expected, current }) => {
            assert_eq!(expected, plan.old_hash);
            assert_eq!(current, Blake3Hash::of_bytes(ELSEWHERE.as_bytes()).to_hex());
        }
        other => panic!("a file changed under the lock must answer Conflict, got {other:?}"),
    }
    assert_eq!(
        fixture.text(),
        ELSEWHERE,
        "the commit must write nothing when the authoritative check fails"
    );
    assert_eq!(
        fixture.entries(),
        ["decision.md"],
        "no temp file may be left"
    );
}

#[test]
fn case_2_4b_the_second_rename_seam_is_not_called() {
    let fixture = Fixture::new();
    let plan = planned(&fixture, "superseded");

    // The hook panics if the seam is called, so a successful commit is the
    // proof that a one-file commit renames once (M7-33).
    let done = commit(&plan, &fixture.options(&PanicsOnSecondRename)).expect("the commit succeeds");

    assert_eq!(done.new_hash, plan.new_hash);
    assert_eq!(fixture.text(), plan.text);
}

#[cfg(unix)]
#[test]
fn case_2_4c_a_symlink_is_written_through_and_one_outside_the_corpus_is_refused() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let root = fixture.root();

    // A link that leaves the corpus names a file this corpus does not own.
    let outside_dir = tempfile::tempdir().expect("outside tempdir");
    let outside = outside_dir.path().join("outside.md");
    fs::write(&outside, ORIGINAL).expect("write the outside document");
    let escape = root.join("notes/escape.md");
    symlink(&outside, &escape).expect("link out of the corpus");
    let escape_plan = plan_from(ORIGINAL, &fixture.config, "superseded");
    match commit(&escape_plan, &fixture.options_at(escape, &NoHooks)) {
        Err(CommitError::OutsideCorpus { .. }) => {}
        other => panic!("a link out of the corpus must be refused, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(&outside).expect("read the outside document"),
        ORIGINAL,
        "a refused write must leave the outside file alone"
    );

    // A link inside the corpus is a document under its own path, so the bytes
    // live at its target and the target is what gets replaced.
    let alias = root.join("notes/alias.md");
    symlink(fixture.doc(), &alias).expect("link the document");
    let plan = planned(&fixture, "superseded");
    commit(&plan, &fixture.options_at(alias.clone(), &NoHooks)).expect("the commit must succeed");

    assert!(
        fs::symlink_metadata(&alias)
            .expect("stat the link")
            .file_type()
            .is_symlink(),
        "the link must survive: replacing it would leave its target stale"
    );
    assert_eq!(
        fixture.text(),
        plan.text,
        "the bytes must land in the target, not in a new file at the link"
    );
}

#[test]
fn case_2_5_one_etag_wins_and_the_lock_is_the_corpus_lock() {
    let fixture = Fixture::new();
    let lock_path = fixture.root().join(".janus").join(LOCK_FILE);
    let plan = planned(&fixture, "superseded");

    // The first commit takes the lock and writes.
    let first = fixture.committed(&plan);
    assert_eq!(first.old_hash, plan.old_hash);
    assert_eq!(first.new_hash, plan.new_hash);
    assert!(
        lock_path.is_file(),
        "the commit must take {}",
        lock_path.display()
    );

    // The second presents the same etag, which is now stale.
    match commit(&plan, &fixture.options(&NoHooks)) {
        Err(CommitError::Conflict { expected, current }) => {
            assert_eq!(expected, plan.old_hash);
            assert_eq!(current, first.new_hash);
        }
        other => panic!("the second commit must answer Conflict, got {other:?}"),
    }

    // A second acquisition in this process waits, then fails on the timeout.
    let held = Lock::acquire(&fixture.root().join(".janus"), LOCK_TIMEOUT)
        .expect("the lock is free again");
    match Lock::acquire(&fixture.root().join(".janus"), SHORT) {
        Err(JanusError::LockTimeout { path }) => assert_eq!(path, lock_path),
        other => panic!("a held lock must time out, got {other:?}"),
    }

    // While the lock is held, a commit with bytes that are current answers Busy
    // and writes nothing.
    let busy_plan = planned(&fixture, "active");
    match commit(&busy_plan, &fixture.impatient(fixture.doc(), &NoHooks)) {
        Err(CommitError::Busy { path }) => assert_eq!(path, lock_path),
        other => panic!("a busy corpus must answer Busy, got {other:?}"),
    }
    assert_eq!(
        fixture.text(),
        plan.text,
        "a busy commit must write nothing"
    );
    drop(held);
}

#[test]
fn case_2_6_the_new_hash_is_the_next_reads_content_hash() {
    let fixture = Fixture::new();
    let plan = planned(&fixture, "superseded");
    let done = fixture.committed(&plan);

    let engine = common::engine(fixture.root());
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
    let fixture = Fixture::new();
    let plan = planned(&fixture, "superseded");
    assert_eq!(fixture.entries(), ["decision.md"]);

    let done = fixture.committed(&plan);

    assert_eq!(
        fixture.text(),
        plan.text,
        "the document must hold the plan's bytes"
    );
    assert_eq!(done.bytes_after, plan.text.len());
    assert_eq!(
        fixture.entries(),
        ["decision.md"],
        "no temp file may survive"
    );
}

#[test]
fn case_3_2_a_refused_write_leaves_no_temp_file() {
    let fixture = Fixture::new();
    let plan = planned(&fixture, "superseded");
    fs::write(fixture.doc(), ELSEWHERE).expect("the second writer's bytes");

    let answer = commit(&plan, &fixture.options(&NoHooks));
    assert!(answer.is_err(), "a stale plan must be refused");
    assert_eq!(
        fixture.entries(),
        ["decision.md"],
        "a refusal stages no file"
    );
    assert_eq!(
        fs::read(fixture.doc()).expect("read back"),
        ELSEWHERE.as_bytes(),
        "a refusal leaves the bytes alone"
    );

    // A failed replace leaves no temp file either. The temp file exists before
    // the rename, and a rename over a directory fails after staging.
    let blocked = fixture.root().join("notes/blocked");
    fs::create_dir(&blocked).expect("create the directory the rename cannot replace");
    match write::replace(&blocked, b"unreachable", Mode::Preserve) {
        Err(CommitError::Io { .. }) => {}
        other => panic!("a rename over a directory must fail, got {other:?}"),
    }
    assert_eq!(
        fixture.entries(),
        ["blocked", "decision.md"],
        "a failed replace must remove its temp file"
    );
}

#[cfg(unix)]
#[test]
fn case_3_3_the_mode_survives_and_the_inode_changes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let fixture = Fixture::new();
    fs::set_permissions(fixture.doc(), fs::Permissions::from_mode(0o640)).expect("set the mode");
    let before = fs::metadata(fixture.doc()).expect("stat the document");
    assert_eq!(before.permissions().mode() & 0o777, 0o640);

    let plan = planned(&fixture, "superseded");
    fixture.committed(&plan);

    let after = fs::metadata(fixture.doc()).expect("stat the document");
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
fn case_3_3b_the_owner_survives_a_rename() {
    use std::os::unix::fs::MetadataExt;

    let fixture = Fixture::new();
    let doc = fixture.doc();
    let current = fs::metadata(&doc).expect("stat the document").gid();
    let Some(other) = groups_of_this_process()
        .into_iter()
        .find(|gid| *gid != current)
    else {
        // One group means the rename cannot change the group, so the case has
        // nothing to prove. It is skipped, not weakened.
        eprintln!("case 3.3b skipped: this process has no second group");
        return;
    };
    std::os::unix::fs::chown(&doc, None, Some(other)).expect("give the document a second group");
    assert_eq!(fs::metadata(&doc).expect("stat the document").gid(), other);

    let plan = planned(&fixture, "superseded");
    fixture.committed(&plan);

    assert_eq!(
        fs::metadata(&doc).expect("stat the document").gid(),
        other,
        "a rename installs a new inode, so the group must be copied onto it"
    );
}

/// The groups this process belongs to, from `id -G`.
#[cfg(unix)]
fn groups_of_this_process() -> Vec<u32> {
    let output = std::process::Command::new("id")
        .arg("-G")
        .output()
        .expect("run id -G");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|gid| gid.parse().ok())
        .collect()
}

#[cfg(unix)]
#[test]
fn case_3_4_a_directory_without_write_permission_fails_and_keeps_the_bytes() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let notes = fixture.root().join("notes");
    let plan = planned(&fixture, "superseded");
    let before = fs::read(fixture.doc()).expect("read the document");

    fs::set_permissions(&notes, fs::Permissions::from_mode(0o500)).expect("close the directory");
    let answer = commit(&plan, &fixture.options(&NoHooks));
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
    assert_eq!(fs::read(fixture.doc()).expect("read back"), before);
    assert_eq!(
        fixture.entries(),
        ["decision.md"],
        "a failure stages no file"
    );
}

#[test]
fn case_3_5_the_directory_sync_names_the_directory_it_could_not_sync() {
    let fixture = Fixture::new();
    let plan = planned(&fixture, "superseded");
    fixture.committed(&plan);

    // The bytes are on disk, so the effect of the directory sync is not
    // observable and this case does not claim it. The call site inside `replace`
    // is unobservable too, for the same reason: what a test can pin is the
    // function `replace` calls, which answers for the document's own directory
    // and names the directory when it cannot sync one.
    write::sync_parent_dir(&fixture.root().join("notes"))
        .expect("a directory on this platform syncs");

    #[cfg(unix)]
    match write::sync_parent_dir(&fixture.root().join("gone")) {
        Err(CommitError::Unsynced { path, .. }) => {
            assert_eq!(path, fixture.root().join("gone"))
        }
        other => panic!("a vanished directory must answer Unsynced, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn case_3_6_a_new_document_takes_the_umask_mode() {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt};

    let fixture = Fixture::new();
    let notes = fixture.root().join("notes");
    // A plain create, the same call any new file makes, so its mode is the
    // umask's. The case reads no umask of its own.
    let probe = notes.join("probe.none");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o666)
        .open(&probe)
        .expect("create the probe");
    let expected = fs::metadata(&probe)
        .expect("stat the probe")
        .permissions()
        .mode()
        & 0o777;

    let target = notes.join("new.md");
    write::replace(&target, b"---\ntitle: new\n---\n", Mode::New).expect("replace");

    let actual = fs::metadata(&target)
        .expect("stat the new document")
        .permissions()
        .mode()
        & 0o777;
    // A umask of 0o077 makes the two modes equal at 0o600, and then this case
    // cannot tell a mask from a hardcoded mode. No other umask hides it.
    assert_eq!(
        actual, expected,
        "a new document must take the umask's mode, because the mode is set at creation"
    );
}
