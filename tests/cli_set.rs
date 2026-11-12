//! `janus set` (M7-4, 4b): the first CLI write, and the four facts a caller
//! needs from it — the table, the exit code, the workspace and the keys.
//!
//! Every case drives the binary and reads the bytes on disk afterwards, so a
//! write is a changed file and not a claim. No case sleeps, reads the wall clock
//! or opens a port, and every corpus is a temporary directory.
//!
//! A CLI write command needs no `--allow-write`: typing the command is the
//! consent, and the flag gates the two server surfaces only (M7-15). The exit
//! codes here are the ones §9.1 lists, and they belong to the process.

mod common;

use std::fs;
use std::path::Path;
use std::process::Output;

use common::{janus, write_document};
use serde_json::json;

/// The one document every write names, and the bytes it becomes.
const ONE: &str = "---\ntitle: One\nstatus: active\n---\n\n# One\n\nfirst prose\n";

/// [`ONE`] after `status: superseded` and an appended `superseded_by`.
const ONE_AFTER: &str = "---\ntitle: One\nstatus: superseded\nsuperseded_by: notes/two.md\n---\n\n# One\n\nfirst prose\n";

// ──────────────────────────────── helpers ──────────────────────────────────

/// A corpus with one document and no index: a write needs none (M7-17).
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"write\"\n\n[embedding]\nenabled = false\n",
    )
    .expect("write the config");
    write_document(dir.path(), "notes/one.md", ONE);
    dir
}

/// A workspace whose primary corpus is either writable or read-only.
fn workspace(primary_read_only: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (corpus, id) in [("primary", "main"), ("archive", "context")] {
        let root = dir.path().join(corpus);
        fs::create_dir_all(&root).expect("create the corpus");
        fs::write(
            root.join("janus.toml"),
            format!(
                "index_dir = \".janus\"\ncorpus_id = \"{id}\"\n\n[embedding]\nenabled = false\n"
            ),
        )
        .expect("write the corpus config");
        write_document(&root, "notes/one.md", ONE);
    }
    fs::write(
        dir.path().join("workspace.toml"),
        format!(
            "[[corpora]]\nid = \"main\"\nroot = \"primary\"\nrole = \"primary\"\n\
             read_only = {primary_read_only}\n\n\
             [[corpora]]\nid = \"context\"\nroot = \"archive\"\nrole = \"archived\"\n\
             read_only = false\n"
        ),
    )
    .expect("write the workspace");
    dir
}

/// The blake3 of one file, which is the etag a read reports.
fn hash_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// The blake3 of the document at `root/rel`.
fn hash(root: &Path, rel: &str) -> String {
    hash_of(&fs::read(root.join(rel)).expect("read the document"))
}

/// Runs `janus` with the arguments a case names.
fn run(args: &[&str]) -> Output {
    janus()
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("run janus")
}

/// Runs `janus set` against one corpus root.
fn set(root: &Path, args: &[&str]) -> Output {
    let mut all = vec!["--vault", root.to_str().expect("a UTF-8 path"), "set"];
    all.extend_from_slice(args);
    run(&all)
}

/// One command's stdout and its exit code, which must be `0`.
fn ok(output: Output) -> (String, i32) {
    let code = output.status.code().expect("the command exited");
    assert_eq!(
        code,
        0,
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        code,
    )
}

/// One command's stderr and its exit code, which must be the one a case names.
fn failed(output: Output, expected: i32) -> String {
    let code = output.status.code().expect("the command exited");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(code, expected, "stdout:\n{stdout}");
    // A receipt command prints nothing on stdout, so "no report" and "a report
    // with no rows" stay distinguishable (§9.4).
    assert_eq!(stdout, "", "a failure writes no receipt");
    String::from_utf8(output.stderr).expect("stderr is UTF-8")
}

// ──────────────────────────────── the command ──────────────────────────────

#[test]
fn m7_15_janus_set_writes_two_keys_without_any_flag() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let after = hash_of(ONE_AFTER.as_bytes());
    let (stdout, _) = ok(set(
        root,
        &[
            "notes/one.md",
            "--expect-hash",
            &before,
            "--key",
            "status=superseded",
            "--key",
            "superseded_by=notes/two.md",
        ],
    ));
    // The table is the whole answer, and every line is written out by hand.
    assert_eq!(
        stdout,
        format!(
            "wrote       notes/one.md\n\
             changed     status, superseded_by\n\
             appended    superseded_by\n\
             hash        {before} -> {after}\n\
             bytes       {} -> {} bytes\n",
            ONE.len(),
            ONE_AFTER.len()
        )
    );
    assert_eq!(
        fs::read_to_string(root.join("notes/one.md")).expect("the written document"),
        ONE_AFTER
    );
}

#[test]
fn m7_15_the_flag_is_a_server_gate_and_set_does_not_take_it() {
    // A CLI write reads no flag: `--allow-write` is not an argument of `set`, so
    // the parser refuses it as an unknown argument (exit 2).
    let dir = scratch();
    let root = dir.path();
    let stderr = failed(
        set(
            root,
            &[
                "notes/one.md",
                "--allow-write",
                "--expect-hash",
                "x",
                "--key",
                "status=superseded",
            ],
        ),
        2,
    );
    assert!(stderr.contains("--allow-write"), "{stderr}");
    assert_eq!(
        fs::read_to_string(root.join("notes/one.md")).expect("the document"),
        ONE
    );
}

#[test]
fn case_2_1_a_dry_run_prints_the_preview_and_writes_nothing() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let (stdout, _) = ok(set(
        root,
        &[
            "notes/one.md",
            "--expect-hash",
            &before,
            "--key",
            "status=superseded",
            "--dry-run",
        ],
    ));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "{stdout}");
    assert_eq!(lines[0], "would write notes/one.md");
    assert_eq!(lines[2], "appended    -");
    assert!(
        lines[3].starts_with(&format!("hash        {before} -> ")),
        "{stdout}"
    );
    assert_eq!(lines[5], "dry run     nothing was written");
    // The preview is not a write.
    assert_eq!(
        fs::read_to_string(root.join("notes/one.md")).expect("the document"),
        ONE
    );
}

#[test]
fn case_2_2_a_stale_hash_exits_three_and_writes_nothing() {
    let dir = scratch();
    let root = dir.path();
    let current = hash(root, "notes/one.md");
    let presented = "0".repeat(64);
    let stderr = failed(
        set(
            root,
            &[
                "notes/one.md",
                "--expect-hash",
                &presented,
                "--key",
                "status=superseded",
            ],
        ),
        3,
    );
    assert!(stderr.contains(&presented), "{stderr}");
    assert!(stderr.contains(&current), "{stderr}");
    assert_eq!(
        fs::read_to_string(root.join("notes/one.md")).expect("the document"),
        ONE
    );
}

#[test]
fn case_2_8_a_path_outside_the_corpus_exits_four() {
    let dir = scratch();
    let root = dir.path();
    let outside = tempfile::tempdir().expect("tempdir");
    write_document(outside.path(), "outside.md", ONE);
    let escaping = outside.path().join("outside.md");
    let stderr = failed(
        set(
            root,
            &[
                escaping.to_str().expect("a UTF-8 path"),
                "--expect-hash",
                "0",
                "--key",
                "status=superseded",
            ],
        ),
        4,
    );
    assert!(stderr.contains("outside the corpus"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&escaping).expect("the outside document"),
        ONE
    );
}

#[test]
fn case_2_7a_a_read_only_corpus_exits_four() {
    // `read_only` lives only in a workspace, so this case names one (M7-38).
    let dir = workspace(true);
    let found = dir.path().join("workspace.toml");
    let root = dir.path().join("primary");
    let before = hash(&root, "notes/one.md");
    let stderr = failed(
        run(&[
            "--workspace",
            found.to_str().expect("a UTF-8 path"),
            "set",
            "notes/one.md",
            "--expect-hash",
            &before,
            "--key",
            "status=superseded",
        ]),
        4,
    );
    assert!(stderr.contains("read-only"), "{stderr}");
    assert_eq!(
        fs::read_to_string(root.join("notes/one.md")).expect("the document"),
        ONE
    );
}

#[test]
fn m7_38_a_qualified_path_writes_the_corpus_it_names() {
    let dir = workspace(false);
    let found = dir.path().join("workspace.toml");
    let primary = dir.path().join("primary");
    let archived = dir.path().join("archive");
    let before = hash(&archived, "notes/one.md");
    let (stdout, _) = ok(run(&[
        "--workspace",
        found.to_str().expect("a UTF-8 path"),
        "set",
        "context:notes/one.md",
        "--expect-hash",
        &before,
        "--key",
        "status=superseded",
    ]));
    // The receipt names the path inside the corpus it wrote, without the
    // qualifier: the qualifier chose the corpus and is not part of the path.
    assert!(stdout.starts_with("wrote       notes/one.md\n"), "{stdout}");
    assert_eq!(
        fs::read_to_string(archived.join("notes/one.md")).expect("the archived document"),
        ONE.replace("status: active", "status: superseded")
    );
    // The primary corpus is untouched.
    assert_eq!(
        fs::read_to_string(primary.join("notes/one.md")).expect("the primary document"),
        ONE
    );
}

// ──────────────────────────────── the receipt ──────────────────────────────

#[test]
fn case_4_5_a_json_receipt_is_the_document_a_consumer_reads() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let (stdout, _) = ok(set(
        root,
        &[
            "notes/one.md",
            "--expect-hash",
            &before,
            "--key",
            "status=superseded",
            "--key",
            "superseded_by=notes/two.md",
            "--json",
        ],
    ));
    let receipt: serde_json::Value =
        serde_json::from_str(&stdout).expect("the receipt is one JSON object");
    // The exact key set of §2.3: no `results` and no `notes`, because a receipt
    // describes bytes and not rows. The proof is the key set, so a field that
    // leaks in fails here.
    let mut keys: Vec<&str> = receipt
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "api",
            "appended_keys",
            "bytes_after",
            "bytes_before",
            "changed_keys",
            "created_block",
            "dry_run",
            "new_hash",
            "ok",
            "old_hash",
            "path",
        ]
    );
    assert_eq!(receipt["api"], json!(1));
    assert_eq!(receipt["ok"], json!(true));
    assert_eq!(receipt["path"], json!("notes/one.md"));
    assert_eq!(receipt["changed_keys"], json!(["status", "superseded_by"]));
    assert_eq!(receipt["appended_keys"], json!(["superseded_by"]));
    assert_eq!(receipt["old_hash"], json!(before));
    assert_eq!(receipt["new_hash"], json!(hash(root, "notes/one.md")));
    assert_eq!(receipt["dry_run"], json!(false));
    assert_eq!(receipt["bytes_before"], json!(ONE.len()));
    assert_eq!(receipt["bytes_after"], json!(ONE_AFTER.len()));
}

#[test]
fn case_2_2_a_json_failure_prints_the_object_and_exits_three() {
    let dir = scratch();
    let root = dir.path();
    let current = hash(root, "notes/one.md");
    let presented = "0".repeat(64);
    let output = set(
        root,
        &[
            "notes/one.md",
            "--expect-hash",
            &presented,
            "--key",
            "status=superseded",
            "--json",
        ],
    );
    // The failure object is the answer, and the exit code still says which kind
    // (§9.1, §9.4).
    assert_eq!(output.status.code(), Some(3));
    let failure: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the failure is one JSON object");
    assert_eq!(failure["api"], json!(1));
    assert_eq!(failure["ok"], json!(false));
    assert_eq!(failure["code"], json!("conflict"));
    let message = failure["message"].as_str().expect("a message");
    assert!(message.contains(&presented), "{message}");
    assert!(message.contains(&current), "{message}");
    assert!(failure.get("results").is_none(), "{failure}");
    // The file is untouched.
    assert_eq!(hash(root, "notes/one.md"), current);
}

// ──────────────────────────────── the keys ─────────────────────────────────

#[test]
fn m7_6_a_key_with_no_value_writes_null() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/one.md");
    let (stdout, _) = ok(set(
        root,
        &[
            "notes/one.md",
            "--expect-hash",
            &before,
            "--key",
            "superseded_by",
        ],
    ));
    assert!(stdout.contains("appended    superseded_by"), "{stdout}");
    assert_eq!(
        fs::read_to_string(root.join("notes/one.md")).expect("the document"),
        ONE.replace("\n---\n\n# One", "\nsuperseded_by: null\n---\n\n# One")
    );
}

#[test]
fn a_request_the_writer_cannot_mean_is_refused() {
    let dir = scratch();
    let root = dir.path();
    let etag = hash(root, "notes/one.md");
    // The etag is not optional: the parser refuses a write without one before
    // any work runs, and names the flag.
    let stderr = failed(set(root, &["notes/one.md", "--key", "status=active"]), 2);
    assert!(stderr.contains("--expect-hash"), "{stderr}");

    // One key twice, and a key that is not a key: neither can be a write.
    let stderr = failed(
        set(
            root,
            &[
                "notes/one.md",
                "--expect-hash",
                &etag,
                "--key",
                "status=active",
                "--key",
                "status=draft",
            ],
        ),
        2,
    );
    assert!(stderr.contains("twice"), "{stderr}");
    let stderr = failed(
        set(
            root,
            &["notes/one.md", "--expect-hash", &etag, "--key", "a: b=x"],
        ),
        4,
    );
    assert!(stderr.contains("is not a key"), "{stderr}");
    assert_eq!(
        fs::read_to_string(root.join("notes/one.md")).expect("the document"),
        ONE
    );
}

// ─────────────────────────── creating a block (M7b-1) ──────────────────────

#[test]
fn case_1_6_set_create_frontmatter_adds_the_block() {
    let dir = scratch();
    let root = dir.path();
    let input = "# Plain\n\nBody line.\n";
    write_document(root, "notes/plain.md", input);
    let before = hash(root, "notes/plain.md");
    let (stdout, _) = ok(set(
        root,
        &[
            "notes/plain.md",
            "--expect-hash",
            &before,
            "--key",
            "status=active",
            "--create-frontmatter",
            "--json",
        ],
    ));

    let receipt: serde_json::Value =
        serde_json::from_str(&stdout).expect("the receipt is one JSON object");
    assert_eq!(receipt["created_block"], json!(true), "{receipt}");
    assert_eq!(
        fs::read_to_string(root.join("notes/plain.md")).expect("the written document"),
        "---\nstatus: active\n---\n# Plain\n\nBody line.\n"
    );
}
