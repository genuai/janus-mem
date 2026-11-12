//! `supersede_document` and `janus supersede` (M7-6): cases 6.1 to 6.8.
//!
//! Every case builds its own corpus in a temporary directory and reads the
//! bytes on disk afterwards. No case sleeps, reads the wall clock, opens a port
//! or looks at `$HOME`. A write needs no index (M7-17), so no case builds one.
//!
//! Case 6.8 drives the writer directly: the hook that fails the second write is
//! a test seam, and the CLI has no flag for it.

mod common;

use std::fs;
use std::path::Path;
use std::process::Output;

use common::{janus, write_document};

#[cfg(unix)]
use janus_mem::config::Config;
#[cfg(unix)]
use janus_mem::write::{
    CommitError, CommitOptions, Hooks, Patch, commit_pair, plan, plan_document,
};
use serde_json::{Value, json};
#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::time::Duration;

/// The old document every case retires, and its bytes after case 6.1.
const OLD: &str = "---\ntitle: Old\nstatus: active\n---\n\n# Old\n\nold prose\n";
const OLD_AFTER: &str =
    "---\ntitle: Old\nstatus: superseded\nsuperseded_by: notes/new.md\n---\n\n# Old\n\nold prose\n";

/// A corpus with one document and no index.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"supersede\"\n\n[embedding]\nenabled = false\n",
    )
    .expect("write the config");
    write_document(dir.path(), "notes/old.md", OLD);
    dir
}

/// The blake3 of one file, which is the etag a read reports.
fn hash(root: &Path, rel: &str) -> String {
    let bytes = fs::read(root.join(rel)).expect("read the document");
    blake3::hash(&bytes).to_hex().to_string()
}

/// Runs `janus supersede` against one corpus root.
fn supersede(root: &Path, args: &[&str]) -> Output {
    let mut all = vec!["--vault", root.to_str().expect("a UTF-8 path"), "supersede"];
    all.extend_from_slice(args);
    janus()
        .env("RUST_LOG", "off")
        .args(all)
        .output()
        .expect("run janus supersede")
}

/// One command's stdout, which must exit `0`.
fn ok(output: Output) -> String {
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// One command's JSON failure object, and its exit code.
fn failed(output: Output, expected: i32) -> Value {
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).expect("the failure is one JSON object")
}

/// Runs one `janus rpc` session and answers its one call.
fn rpc(root: &Path, params: Value) -> Value {
    let request = json!({ "id": 1, "tool": "supersede_document", "params": params });
    let output = janus()
        .env("RUST_LOG", "off")
        .arg("--vault")
        .arg(root)
        .args(["rpc", "--allow-write"])
        .write_stdin(format!("{request}\n"))
        .output()
        .expect("run janus rpc");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    serde_json::from_str(stdout.lines().next().expect("one answer line"))
        .expect("the answer is one JSON object")
}

/// The top-level keys of one document's frontmatter, in file order.
fn keys(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim_end() == "---" {
            inside = !inside;
            continue;
        }
        if inside && let Some((key, _)) = line.split_once(':') {
            out.push(key.to_owned());
        }
    }
    out
}

// ─────────────────────────────── in place ──────────────────────────────────

#[test]
fn case_6_1_in_place_retires_the_old_and_records_the_new() {
    let dir = scratch();
    let root = dir.path();
    let stdout = ok(supersede(
        root,
        &[
            "notes/old.md",
            "notes/new.md",
            "--expect-hash",
            &hash(root, "notes/old.md"),
            "--json",
        ],
    ));
    let receipt: Value = serde_json::from_str(&stdout).expect("the receipt is one JSON object");
    assert_eq!(receipt["ok"], json!(true), "{receipt}");
    assert_eq!(
        fs::read_to_string(root.join("notes/old.md")).expect("old"),
        OLD_AFTER
    );
    assert_eq!(
        fs::read_to_string(root.join("notes/new.md")).expect("new"),
        "---\nsupersedes: notes/old.md\n---\n"
    );
}

#[test]
fn case_6_2_only_the_three_supersession_keys_are_written() {
    let dir = scratch();
    let root = dir.path();
    let receipt = rpc(
        root,
        json!({
            "old_path": "notes/old.md",
            "new_path": "notes/new.md",
            "new_title": "New",
            "expected_hash": hash(root, "notes/old.md"),
        }),
    );
    assert_eq!(receipt["ok"], json!(true), "{receipt}");
    let old = fs::read_to_string(root.join("notes/old.md")).expect("old");
    let new = fs::read_to_string(root.join("notes/new.md")).expect("new");
    assert_eq!(keys(&old), ["title", "status", "superseded_by"], "{old}");
    assert_eq!(keys(&new), ["supersedes", "title"], "{new}");
    assert!(!old.contains("effective_to"), "{old}");
    assert!(!old.contains("doc_version"), "{old}");
    assert!(!old.contains("\nid:"), "{old}");
    assert!(!new.contains("effective_to"), "{new}");
    assert!(!new.contains("doc_version"), "{new}");
    assert!(!new.contains("\nid:"), "{new}");
}

#[test]
fn case_6_3_a_refused_patch_writes_neither_file() {
    let dir = scratch();
    let root = dir.path();
    let broken = "---\ntitle: Old\nstatus:\n  nested: value\n---\n\n# Old\n\nold prose\n";
    write_document(root, "notes/old.md", broken);
    let before = hash(root, "notes/old.md");
    let failure = failed(
        supersede(
            root,
            &[
                "notes/old.md",
                "notes/new.md",
                "--expect-hash",
                &before,
                "--json",
            ],
        ),
        4,
    );
    assert_eq!(failure["code"], json!("unsupported_patch"), "{failure}");
    assert!(
        failure["message"]
            .as_str()
            .is_some_and(|m| m.contains("status"))
    );
    assert_eq!(
        fs::read_to_string(root.join("notes/old.md")).expect("old"),
        broken
    );
    assert!(
        !root.join("notes/new.md").exists(),
        "the new document was written"
    );
}

#[test]
fn case_6_4_a_cycle_is_refused_before_staging() {
    let dir = scratch();
    let root = dir.path();
    // Build the cycle with plain files: new -> bridge -> old. The new file does
    // not exist yet, but both documents already name it.
    write_document(
        root,
        "notes/old.md",
        "---\ntitle: Old\nstatus: active\nsupersedes: tools/bridge.md\n---\n\n# Old\n",
    );
    write_document(
        root,
        "tools/bridge.md",
        "---\ntitle: Bridge\nsupersedes: tools/new.md\n---\n\n# Bridge\n",
    );
    let old_before = hash(root, "notes/old.md");
    let bridge_before = hash(root, "tools/bridge.md");
    let failure = failed(
        supersede(
            root,
            &[
                "notes/old.md",
                "tools/new.md",
                "--expect-hash",
                &old_before,
                "--json",
            ],
        ),
        4,
    );
    assert_eq!(failure["code"], json!("unsupported_patch"), "{failure}");
    let message = failure["message"].as_str().expect("a message");
    for path in ["notes/old.md", "tools/bridge.md", "tools/new.md"] {
        assert!(message.contains(path), "{message} must name {path}");
    }
    assert!(message.contains("cycle"), "{message}");
    assert_eq!(hash(root, "notes/old.md"), old_before);
    assert_eq!(hash(root, "tools/bridge.md"), bridge_before);
    assert!(!root.join("tools/new.md").exists());
}

// ────────────────────────────── append only ────────────────────────────────

#[test]
fn case_6_5_append_only_leaves_the_old_document_byte_identical() {
    let dir = scratch();
    let root = dir.path();
    let old_before = hash(root, "notes/old.md");
    let stdout = ok(supersede(
        root,
        &[
            "notes/old.md",
            "notes/new.md",
            "--expect-hash",
            &old_before,
            "--append-only",
            "--json",
        ],
    ));
    let receipt: Value = serde_json::from_str(&stdout).expect("the receipt is one JSON object");
    assert_eq!(receipt["ok"], json!(true), "{receipt}");
    assert_eq!(hash(root, "notes/old.md"), old_before);
    assert_eq!(
        fs::read_to_string(root.join("notes/new.md")).expect("new"),
        "---\nsupersedes: notes/old.md\n---\n"
    );
    assert_eq!(receipt["receipts"].as_array().expect("receipts").len(), 1);
}

#[test]
fn case_6_6_the_receipt_carries_both_receipts_and_the_chain() {
    let dir = scratch();
    let root = dir.path();
    let receipt = rpc(
        root,
        json!({
            "old_path": "notes/old.md",
            "new_path": "notes/new.md",
            "expected_hash": hash(root, "notes/old.md"),
        }),
    );
    assert_eq!(receipt["ok"], json!(true), "{receipt}");
    let receipts = receipt["receipts"].as_array().expect("receipts");
    assert_eq!(receipts.len(), 2, "{receipt}");
    assert_eq!(receipts[0]["path"], json!("notes/old.md"), "{receipt}");
    assert_eq!(receipts[1]["path"], json!("notes/new.md"), "{receipt}");
    assert_eq!(
        receipt["chain"],
        json!(["notes/old.md", "notes/new.md"]),
        "{receipt}"
    );
    assert_eq!(receipt["path"], json!("notes/old.md"), "{receipt}");
}

#[test]
fn case_6_7_an_existing_new_path_is_refused() {
    let dir = scratch();
    let root = dir.path();
    const TAKEN: &str = "---\ntitle: Taken\n---\n\n# Taken\n";
    write_document(root, "notes/new.md", TAKEN);
    let old_before = hash(root, "notes/old.md");
    let new_before = hash(root, "notes/new.md");
    let failure = failed(
        supersede(
            root,
            &[
                "notes/old.md",
                "notes/new.md",
                "--expect-hash",
                &old_before,
                "--json",
            ],
        ),
        4,
    );
    assert_eq!(failure["code"], json!("path_exists"), "{failure}");
    assert_eq!(hash(root, "notes/old.md"), old_before);
    assert_eq!(hash(root, "notes/new.md"), new_before);
}

// ──────────────────────────────── rollback ─────────────────────────────────

/// Fails the second write by making its parent directory read-only.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct ReadOnlyParent;

#[cfg(unix)]
impl Hooks for ReadOnlyParent {
    fn after_pre_lock(&self, _path: &Path) {}

    fn before_second_rename(&self, path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = path.parent().expect("the new file has a parent");
        fs::set_permissions(parent, fs::Permissions::from_mode(0o555))
            .expect("make the new file's parent read-only");
    }
}

#[cfg(unix)]
#[test]
fn case_6_8_a_failed_second_write_restores_the_old_file_and_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = scratch();
    let root = dir.path();
    fs::create_dir(root.join("successor")).expect("create the new file's directory");
    let old_path = root.join("notes/old.md");
    let new_path = root.join("successor/new.md");
    let original = fs::read(&old_path).expect("read the old document");
    let mode = fs::metadata(&old_path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;

    let patch = Patch {
        path: old_path.clone(),
        keys: vec![
            ("status".to_owned(), Some("superseded".to_owned())),
            (
                "superseded_by".to_owned(),
                Some("successor/new.md".to_owned()),
            ),
        ],
        expected_hash: Some(blake3::hash(&original).to_hex().to_string()),
        create_frontmatter: false,
        dry_run: false,
    };
    let old_plan = plan(std::str::from_utf8(&original).expect("UTF-8"), &patch).expect("plan");
    let mut frontmatter: BTreeMap<String, Option<String>> = BTreeMap::new();
    frontmatter.insert("supersedes".to_owned(), Some("notes/old.md".to_owned()));
    let new_document = plan_document(&frontmatter, "", None).expect("plan the new document");
    let config = Config::load(root).expect("the corpus config");
    let options = CommitOptions {
        path: old_path.clone(),
        root: root.to_path_buf(),
        config: &config,
        timeout: Duration::from_secs(1),
        hooks: &ReadOnlyParent,
    };
    let result = commit_pair(&old_plan, &new_path, new_document.text.as_bytes(), &options);

    // The hook's own change is undone before the assertions, so a failure here
    // does not hide the file the transaction was supposed to restore.
    fs::set_permissions(root.join("successor"), fs::Permissions::from_mode(0o755))
        .expect("restore the directory mode");
    match result {
        Err(CommitError::RolledBack {
            failed, restored, ..
        }) => {
            assert!(failed.ends_with("new.md"), "{}", failed.display());
            assert!(restored.ends_with("old.md"), "{}", restored.display());
        }
        other => panic!("expected a rollback, got {other:?}"),
    }
    assert_eq!(fs::read(&old_path).expect("old"), original);
    assert_eq!(
        fs::metadata(&old_path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        mode
    );
    assert!(!new_path.exists(), "the new file was left behind");
}
