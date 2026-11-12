//! `write_document` and `janus write` (M7-5): cases 5.1 to 5.6, the mode of a
//! created file, and the body written byte for byte.
//!
//! Every case builds its own corpus in a temporary directory and reads the bytes
//! on disk afterwards. No case sleeps, reads the wall clock, opens a port or
//! looks at `$HOME`. A write needs no index (M7-17), so no case builds one.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{janus, write_document};
use serde_json::{Value, json};

/// The document an overwrite replaces.
const OLD: &str = "---\ntitle: Old\nstatus: active\n---\n\n# Old\n\nold prose\n";

/// A corpus with one document and no index.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"write\"\n\n[embedding]\nenabled = false\n",
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

/// Runs one `janus write` command, with optional stdin.
fn write_command(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    let mut command = janus();
    command
        .env("RUST_LOG", "off")
        .arg("--vault")
        .arg(root)
        .arg("write");
    command.args(args);
    if let Some(bytes) = stdin {
        command.write_stdin(bytes);
    }
    command.output().expect("run janus write")
}

/// One command's stdout and exit code, which must be `0`.
fn ok(output: std::process::Output) -> String {
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
fn failure(output: std::process::Output, expected: i32) -> Value {
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&stdout).expect("the failure is one JSON object")
}

/// Runs one `janus rpc` session and returns its one answer.
fn rpc(root: &Path, params: Value) -> Value {
    let request = json!({ "id": 1, "tool": "write_document", "params": params });
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

/// Writes one YAML frontmatter file and answers its path.
fn frontmatter(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, text).expect("write the frontmatter file");
    path
}

// ──────────────────────────────── create ───────────────────────────────────

#[test]
fn case_5_1_create_on_an_absent_path_answers_a_receipt_with_no_old_hash() {
    let dir = scratch();
    let root = dir.path();
    let fm = frontmatter(
        root,
        "frontmatter.yaml",
        "title: New\nstatus: active\ndate: 2026-04-01\ntype: decision\n",
    );
    let body = b"# New\n\nthe new body\n";
    let stdout = ok(write_command(
        root,
        &[
            "notes/new.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--create",
            "--json",
        ],
        Some(body),
    ));
    let receipt: Value = serde_json::from_str(&stdout).expect("the receipt is one JSON object");
    let expected = "---\ndate: 2026-04-01\nstatus: active\ntitle: New\ntype: decision\n---\n# New\n\nthe new body\n";
    assert_eq!(
        fs::read(root.join("notes/new.md")).expect("the created document"),
        expected.as_bytes()
    );
    assert_eq!(receipt["ok"], json!(true), "{receipt}");
    assert_eq!(receipt["path"], json!("notes/new.md"), "{receipt}");
    assert_eq!(receipt["old_hash"], Value::Null, "{receipt}");
    assert_eq!(receipt["bytes_before"], json!(0), "{receipt}");
    assert_eq!(receipt["bytes_after"], json!(expected.len()), "{receipt}");
    assert_eq!(receipt["new_hash"], json!(hash(root, "notes/new.md")));
    assert_eq!(
        receipt["changed_keys"],
        json!(["date", "status", "title", "type"]),
        "{receipt}"
    );
    // Every key is appended, because the file did not exist.
    assert_eq!(
        receipt["appended_keys"],
        json!(["date", "status", "title", "type"]),
        "{receipt}"
    );
    assert!(receipt.get("results").is_none(), "{receipt}");
    assert!(receipt.get("notes").is_none(), "{receipt}");
}

#[test]
fn case_5_2_create_on_an_existing_path_is_refused_and_writes_nothing() {
    let dir = scratch();
    let root = dir.path();
    let fm = frontmatter(root, "frontmatter.yaml", "status: superseded\n");
    let before = hash(root, "notes/old.md");
    let output = write_command(
        root,
        &[
            "notes/old.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--create",
            "--json",
        ],
        Some(b"replacement\n"),
    );
    let answer = failure(output, 4);
    assert_eq!(answer["code"], json!("path_exists"), "{answer}");
    assert_eq!(hash(root, "notes/old.md"), before);
}

#[test]
fn m7_5_a_create_needs_a_markdown_path() {
    let dir = scratch();
    let root = dir.path();
    let fm = frontmatter(root, "frontmatter.yaml", "status: active\n");
    let output = write_command(
        root,
        &[
            "notes/new.txt",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--create",
            "--json",
        ],
        Some(b"not a document\n"),
    );
    let answer = failure(output, 2);
    assert_eq!(answer["code"], json!("usage"), "{answer}");
    assert!(!root.join("notes/new.txt").exists());
}

#[cfg(unix)]
#[test]
fn m7_5_a_create_never_replaces_a_symlink_target() {
    use std::os::unix::fs::symlink;

    let dir = scratch();
    let root = dir.path();
    let target = root.join("notes/target.md");
    write_document(root, "notes/target.md", OLD);
    let link = root.join("notes/link.md");
    symlink(&target, &link).expect("create the link");
    let before = hash(root, "notes/target.md");
    let fm = frontmatter(root, "frontmatter.yaml", "status: active\n");
    let output = write_command(
        root,
        &[
            "notes/link.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--create",
            "--json",
        ],
        Some(b"replacement\n"),
    );
    let answer = failure(output, 4);
    assert_eq!(answer["code"], json!("path_exists"), "{answer}");
    // The link is still a link, and its target keeps its bytes.
    assert!(
        link.symlink_metadata()
            .expect("stat the link")
            .file_type()
            .is_symlink()
    );
    assert_eq!(hash(root, "notes/target.md"), before);
}

// ─────────────────────────────── overwrite ─────────────────────────────────

#[test]
fn case_5_3_overwrite_without_an_expected_hash_is_refused() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/old.md");
    let answer = rpc(
        root,
        json!({
            "path": "notes/old.md",
            "frontmatter": { "status": "superseded", "title": "New" },
            "body": "# New\n",
            "mode": "overwrite",
        }),
    );
    assert_eq!(answer["ok"], json!(false), "{answer}");
    assert_eq!(answer["code"], json!("precondition_required"), "{answer}");
    let hint = answer["hint"].as_str().expect("a hint");
    assert!(hint.contains("status"), "{hint}");
    assert!(!hint.contains('\n'), "the hint is one line: {hint}");
    assert_eq!(hash(root, "notes/old.md"), before);
}

#[test]
fn case_5_4_overwrite_with_the_current_hash_replaces_the_whole_file() {
    let dir = scratch();
    let root = dir.path();
    let before = hash(root, "notes/old.md");
    let fm = frontmatter(root, "frontmatter.yaml", "title: New\nstatus: superseded\n");
    let expected = "---\nstatus: superseded\ntitle: New\n---\n# New\n";
    let stdout = ok(write_command(
        root,
        &[
            "notes/old.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--overwrite",
            "--expect-hash",
            &before,
            "--json",
        ],
        Some(b"# New\n"),
    ));
    let receipt: Value = serde_json::from_str(&stdout).expect("the receipt is one JSON object");
    assert_eq!(
        fs::read_to_string(root.join("notes/old.md")).expect("the replaced document"),
        expected
    );
    assert_eq!(receipt["old_hash"], json!(before), "{receipt}");
    assert_eq!(receipt["new_hash"], json!(hash(root, "notes/old.md")));
    assert_eq!(receipt["bytes_before"], json!(OLD.len()), "{receipt}");
    assert_eq!(receipt["bytes_after"], json!(expected.len()), "{receipt}");
    // Every key is written, and both keys were already in the file.
    assert_eq!(
        receipt["changed_keys"],
        json!(["status", "title"]),
        "{receipt}"
    );
    assert_eq!(receipt["appended_keys"], json!([]), "{receipt}");

    // The hash the caller presented is stale now, so a second call is a
    // conflict and the bytes stay as the first call wrote them.
    let after = hash(root, "notes/old.md");
    let output = write_command(
        root,
        &[
            "notes/old.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--overwrite",
            "--expect-hash",
            &before,
            "--json",
        ],
        Some(b"# Other\n"),
    );
    let answer = failure(output, 3);
    assert_eq!(answer["code"], json!("conflict"), "{answer}");
    assert_eq!(hash(root, "notes/old.md"), after);
}

// ───────────────────────────────── the body ────────────────────────────────

#[test]
fn case_5_5_a_body_that_is_not_utf8_is_a_usage_error() {
    let dir = scratch();
    let root = dir.path();
    let fm = frontmatter(root, "frontmatter.yaml", "status: active\n");
    let body = root.join("body.bin");
    fs::write(&body, [0xff, 0xfe, 0x00]).expect("write the bad body");
    let output = write_command(
        root,
        &[
            "notes/new.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            body.to_str().expect("a UTF-8 path"),
            "--create",
            "--json",
        ],
        None,
    );
    let answer = failure(output, 2);
    assert_eq!(answer["code"], json!("usage"), "{answer}");
    assert!(!root.join("notes/new.md").exists());
}

#[test]
fn m7_5_a_body_is_written_verbatim_in_three_shapes() {
    let dir = scratch();
    let root = dir.path();
    let fm = frontmatter(root, "frontmatter.yaml", "title: Body\n");
    let cases: [(&str, &[u8]); 3] = [
        ("notes/crlf.md", b"line one\r\nline two\r\n"),
        ("notes/no-newline.md", b"no trailing newline"),
        ("notes/empty.md", b""),
    ];
    for (rel, body) in cases {
        let output = write_command(
            root,
            &[
                rel,
                "--frontmatter",
                fm.to_str().expect("a UTF-8 path"),
                "--body",
                "-",
                "--create",
            ],
            Some(body),
        );
        ok(output);
        let mut expected = b"---\ntitle: Body\n---\n".to_vec();
        expected.extend_from_slice(body);
        assert_eq!(
            fs::read(root.join(rel)).expect("the created document"),
            expected,
            "{rel}"
        );
    }
}

// ─────────────────────────────── the first read ────────────────────────────

#[test]
fn case_5_6_the_first_read_agrees_with_the_request() {
    let dir = scratch();
    let root = dir.path();
    let fm = frontmatter(
        root,
        "frontmatter.yaml",
        "title: New\ntype: decision\nstatus: active\ndate: 2026-04-01\n",
    );
    let body = b"# New\n\nthe new body\n";
    ok(write_command(
        root,
        &[
            "notes/new.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--create",
        ],
        Some(body),
    ));
    let output = janus()
        .env("RUST_LOG", "off")
        .arg("--vault")
        .arg(root)
        .args(["read", "notes/new.md", "--json"])
        .output()
        .expect("run janus read");
    let stdout = ok(output);
    let read: Value = serde_json::from_str(&stdout).expect("the read is one JSON object");
    assert_eq!(read["frontmatter"]["type"], json!("decision"), "{read}");
    assert_eq!(read["frontmatter"]["status"], json!("active"), "{read}");
    assert_eq!(read["frontmatter"]["date"], json!("2026-04-01"), "{read}");
    assert_eq!(
        read["body"],
        json!(String::from_utf8(body.to_vec()).expect("UTF-8")),
        "{read}"
    );
}

// ────────────────────────────────── the mode ───────────────────────────────

#[cfg(unix)]
#[test]
fn m7_5_a_created_document_takes_the_umask_mode() {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt};

    let dir = scratch();
    let root = dir.path();
    let fm = frontmatter(root, "frontmatter.yaml", "title: Mode\n");
    ok(write_command(
        root,
        &[
            "notes/new.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--create",
        ],
        Some(b"body\n"),
    ));
    // A plain create, the same call any new file makes, so its mode is the
    // umask's. The case reads no umask of its own (case 3.6 of the writer).
    let probe = root.join("notes/probe.none");
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
    let actual = fs::metadata(root.join("notes/new.md"))
        .expect("stat the new document")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(actual, expected);
}

#[test]
fn m7_5_a_create_keeps_the_stdout_free_of_diagnostics() {
    let dir = scratch();
    let root = dir.path();
    let fm = frontmatter(root, "frontmatter.yaml", "title: Quiet\n");
    let output = write_command(
        root,
        &[
            "notes/new.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--create",
        ],
        Some(b"body\n"),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 5, "{stdout}");
    assert_eq!(lines[0], "wrote       notes/new.md", "{stdout}");
    assert_eq!(lines[1], "changed     title", "{stdout}");
    assert_eq!(lines[2], "appended    title", "{stdout}");
    assert!(!stdout.contains("warning"), "{stdout}");
}

// ──────────────────────────── receipt accuracy ─────────────────────────────

/// The value bytes one key holds, as this test reads them.
///
/// The test's own reader, not the writer's: it compares two files, so the
/// receipt is checked against the bytes and not against the code that made them.
fn value_of(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    text.lines()
        .find(|line| line.starts_with(&prefix))
        .map(|line| line[prefix.len()..].to_owned())
}

/// The requested keys whose value bytes differ between two files, sorted.
fn differing(before: &str, after: &str, keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = keys
        .iter()
        .filter(|key| value_of(before, key) != value_of(after, key))
        .map(|key| (*key).to_owned())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The requested keys `before` does not hold, sorted because `keys` is.
fn appended(before: &str, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .filter(|key| value_of(before, key).is_none())
        .map(|key| (*key).to_owned())
        .collect()
}

#[test]
fn case_3_1_write_document_reports_only_the_keys_whose_bytes_differ() {
    let dir = scratch();
    let root = dir.path();
    let before = "---\ntitle: Old\nowner: \"Ann\"\nstatus: active\n---\n\n# Old\n";
    write_document(root, "notes/old.md", before);
    let answer = rpc(
        root,
        json!({
            "path": "notes/old.md",
            "frontmatter": {
                "title": "Old",
                "owner": "Ann",
                "status": "superseded",
                "tag": "new",
            },
            "body": "\n# Old\n",
            "mode": "overwrite",
            "expected_hash": hash(root, "notes/old.md"),
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    let after = fs::read_to_string(root.join("notes/old.md")).expect("the written document");
    // `title` keeps its bytes; `owner` loses its quotes; `status` changes value;
    // `tag` was absent. The test decides that, not the receipt.
    let requested = ["owner", "status", "tag", "title"];
    assert_eq!(
        differing(before, &after, &requested),
        ["owner", "status", "tag"]
    );
    assert_eq!(
        answer["changed_keys"],
        json!(differing(before, &after, &requested)),
        "{answer}"
    );
    assert_eq!(
        answer["appended_keys"],
        json!(appended(before, &requested)),
        "{answer}"
    );
    assert_eq!(answer["appended_keys"], json!(["tag"]), "{answer}");
}

#[test]
fn case_3_2_a_key_rewritten_with_the_same_bytes_is_in_neither_list() {
    let dir = scratch();
    let root = dir.path();
    let text = "---\nstatus: active\ntitle: Old\n---\n# Old\n";
    write_document(root, "notes/old.md", text);
    let before = hash(root, "notes/old.md");
    let answer = rpc(
        root,
        json!({
            "path": "notes/old.md",
            "frontmatter": { "status": "active", "title": "Old" },
            "body": "# Old\n",
            "mode": "overwrite",
            "expected_hash": before,
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(answer["changed_keys"], json!([]), "{answer}");
    assert_eq!(answer["appended_keys"], json!([]), "{answer}");
    // The whole file kept its bytes, so the two hashes agree.
    assert_eq!(answer["old_hash"], json!(before), "{answer}");
    assert_eq!(answer["new_hash"], json!(before), "{answer}");
    assert_eq!(
        fs::read_to_string(root.join("notes/old.md")).expect("the document"),
        text
    );
}

#[test]
fn case_3_3_a_created_block_reports_every_key_as_appended() {
    let dir = scratch();
    let root = dir.path();
    let before = "no frontmatter here\n";
    write_document(root, "notes/plain.md", before);
    let answer = rpc(
        root,
        json!({
            "path": "notes/plain.md",
            "frontmatter": { "title": "New", "status": "active" },
            "body": before,
            "mode": "overwrite",
            "expected_hash": hash(root, "notes/plain.md"),
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(answer["created_block"], json!(true), "{answer}");
    assert_eq!(
        answer["changed_keys"],
        json!(["status", "title"]),
        "{answer}"
    );
    assert_eq!(
        answer["appended_keys"],
        json!(["status", "title"]),
        "{answer}"
    );
    assert_eq!(
        fs::read_to_string(root.join("notes/plain.md")).expect("the document"),
        "---\nstatus: active\ntitle: New\n---\nno frontmatter here\n"
    );
}

#[test]
fn case_3_4_the_receipt_lists_are_sorted_and_deduplicated() {
    let dir = scratch();
    let root = dir.path();
    write_document(root, "notes/old.md", "---\ntitle: Old\n---\n# Old\n");
    // The request names its keys out of order on purpose.
    let fm = frontmatter(
        root,
        "frontmatter.yaml",
        "zeta: last\nmu: middle\nalpha: first\n",
    );
    let stdout = ok(write_command(
        root,
        &[
            "notes/old.md",
            "--frontmatter",
            fm.to_str().expect("a UTF-8 path"),
            "--body",
            "-",
            "--overwrite",
            "--expect-hash",
            &hash(root, "notes/old.md"),
            "--json",
        ],
        Some(b"# Old\n"),
    ));
    let receipt: Value = serde_json::from_str(&stdout).expect("the receipt is one JSON object");
    assert_eq!(
        receipt["changed_keys"],
        json!(["alpha", "mu", "zeta"]),
        "{receipt}"
    );
    assert_eq!(
        receipt["appended_keys"],
        json!(["alpha", "mu", "zeta"]),
        "{receipt}"
    );
}

#[test]
fn m7b_20_a_block_the_scanner_refuses_reports_every_key_as_changed() {
    let dir = scratch();
    let root = dir.path();
    // Two definitions of one key: the block cannot be read as keys, so the
    // writer must not claim that `title` kept its bytes.
    let before = "---\ntitle: Old\ntitle: Older\n---\n# Old\n";
    write_document(root, "notes/old.md", before);
    let answer = rpc(
        root,
        json!({
            "path": "notes/old.md",
            "frontmatter": { "title": "Old" },
            "body": "# Old\n",
            "mode": "overwrite",
            "expected_hash": hash(root, "notes/old.md"),
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    assert_eq!(answer["changed_keys"], json!(["title"]), "{answer}");
    // The file did hold `title`, so the write appends nothing.
    assert_eq!(answer["appended_keys"], json!([]), "{answer}");
}

#[test]
fn m7b_20_a_dropped_trailing_comment_reports_a_change() {
    let dir = scratch();
    let root = dir.path();
    let before = "---\nstatus: active  # current\n---\n# Old\n";
    write_document(root, "notes/old.md", before);
    let answer = rpc(
        root,
        json!({
            "path": "notes/old.md",
            "frontmatter": { "status": "active" },
            "body": "# Old\n",
            "mode": "overwrite",
            "expected_hash": hash(root, "notes/old.md"),
        }),
    );
    assert_eq!(answer["ok"], json!(true), "{answer}");
    // The value bytes are the same, but the line loses its comment.
    assert_eq!(answer["changed_keys"], json!(["status"]), "{answer}");
    assert_eq!(
        fs::read_to_string(root.join("notes/old.md")).expect("the document"),
        "---\nstatus: active\n---\n# Old\n"
    );
}
