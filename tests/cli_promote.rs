//! `promote_document` and cross-corpus link resolution (M7-7): cases 7.1 to 7.8.
//!
//! Every case copies the two-corpus fixture into a temporary directory and
//! writes the workspace file it needs, because the policy and the archived
//! corpus's `read_only` are the two facts a case flips. No case sleeps, reads
//! the wall clock or opens a port.
//!
//! The archive is hashed and its mtime read before and after every write, so a
//! promotion that touched it fails here (case 7.3).

mod common;

use std::fs;
use std::path::Path;
use std::process::Output;
use std::time::SystemTime;

use assert_cmd::Command;
use serde_json::{Value, json};

/// The committed two-corpus fixture (§M7-7).
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/workspace-promote"
);

/// The archived decision a promotion names, and its path inside its corpus.
const SOURCE_PATH: &str = "decisions/2020-01-01-postgres.md";
const SOURCE: &str = "context:decisions/2020-01-01-postgres.md";

/// The primary decision that gains the link.
const TARGET: &str = "decisions/2026-01-01-sqlite.md";

/// The primary document whose `supersedes` already names another document.
const LINKED: &str = "decisions/2026-02-02-already-linked.md";

/// The document [`LINKED`] already names, so a second link would drop it.
const EARLY: &str = "decisions/2025-01-01-early.md";

/// The target's bytes after `link` mode: one appended key, nothing else.
const TARGET_AFTER: &str = "---\ntitle: Adopt SQLite\ntype: decision\nstatus: active\ndate: 2026-01-01\nsupersedes: context:decisions/2020-01-01-postgres.md\n---\n\nUse SQLite.\n";

/// The source's bytes, which no mode may change.
const SOURCE_BEFORE: &str = "---\ntitle: Adopt Postgres\ntype: decision\nstatus: active\ndate: 2020-01-01\n---\n\nUse Postgres.\n";

/// The file `adopt` mode writes: the qualified source, the source's title, then
/// its body byte for byte.
const ADOPTED: &str = "---\nsupersedes: context:decisions/2020-01-01-postgres.md\ntitle: Adopt Postgres\n---\n\nUse Postgres.\n";

// ──────────────────────────────── helpers ──────────────────────────────────

/// A private copy of the committed fixture workspace.
fn copied() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir for the fixture copy");
    copy_tree(Path::new(FIXTURE), dir.path());
    dir
}

/// Copies one tree, skipping any index a previous run left behind.
fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            if entry.file_name() == ".janus" {
                continue;
            }
            fs::create_dir_all(&target).expect("create the directory");
            copy_tree(&entry.path(), &target);
        } else if entry.file_name() != "README.md" {
            fs::copy(entry.path(), &target).expect("copy the file");
        }
    }
}

/// Writes the workspace file one case reads, and answers its path.
///
/// Two facts are flipped per case: whether a qualified link resolves, and
/// whether the archived corpus accepts a write. The role alone refuses a write
/// by default; `read_only = false` is what makes the archive writable, which is
/// what lets case 7.3 show that the source guard is the thing that stops it.
fn write_workspace(dir: &Path, allow_links: bool, archive_writable: bool) -> String {
    let path = dir.join("workspace.toml");
    fs::write(
        &path,
        format!(
            "[[corpora]]\nid = \"main\"\nroot = \"primary\"\nrole = \"primary\"\n\n\
             [[corpora]]\nid = \"context\"\nroot = \"context\"\nrole = \"archived\"\n\
             read_only = {}\n\n\
             [search]\nallow_cross_corpus_links = {allow_links}\n",
            !archive_writable
        ),
    )
    .expect("write the workspace");
    path.display().to_string()
}

/// A `janus` command for one fixture copy, with no workspace in the ambient
/// configuration: every case names its workspace with the flag or not at all.
fn janus_in(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("janus").expect("the janus binary must build for tests");
    command
        .current_dir(dir)
        .env("RUST_LOG", "off")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env_remove("JANUS_WORKSPACE")
        .env_remove("JANUS_VAULT")
        .env_remove("APPDATA")
        .env_remove("HOME");
    command
}

/// Runs `janus` and returns the finished process.
fn run(dir: &Path, args: &[&str]) -> Output {
    janus_in(dir).args(args).output().expect("run janus")
}

/// Runs one command that must succeed and returns its stdout.
fn ok(dir: &Path, args: &[&str]) -> String {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "janus {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// Runs one command that must succeed and parses its report.
fn json(dir: &Path, args: &[&str]) -> Value {
    let text = ok(dir, args);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("stdout is not JSON ({err}):\n{text}"))
}

/// One request line to `janus rpc`, and the envelope it answered.
fn rpc(dir: &Path, ws: &str, tool: &str, params: Value) -> Value {
    let output = janus_in(dir)
        .args(["--workspace", ws, "rpc", "--allow-write"])
        .write_stdin(format!(
            "{}\n",
            json!({ "id": 1, "tool": tool, "params": params })
        ))
        .output()
        .expect("run janus rpc");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let line = stdout.lines().next().expect("one answer line");
    let mut answer: Value = serde_json::from_str(line).expect("the answer is JSON");
    answer.as_object_mut().expect("an object").remove("id");
    answer
}

/// Builds both indexes, so a graph check has a generation to read.
///
/// `janus index` builds one corpus, named by its root, so the workspace is not
/// part of this: the fixture's corpora do not nest.
fn index(dir: &Path) {
    for corpus in ["primary", "context"] {
        let root = dir.join(corpus).display().to_string();
        ok(dir, &["--vault", &root, "index", "--no-embeddings"]);
    }
}

/// One file's blake3, which is the etag a read reports.
fn blake(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    blake3::hash(&bytes).to_hex().to_string()
}

/// One file's modification time.
fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .unwrap_or_else(|err| panic!("stat {}: {err}", path.display()))
        .modified()
        .expect("a modification time")
}

/// One document's text.
fn text(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel))
        .unwrap_or_else(|err| panic!("read {rel} in {}: {err}", root.display()))
}

/// The failure code of one envelope, asserting that it failed.
fn code(envelope: &Value) -> &str {
    assert_eq!(envelope["ok"], json!(false), "{envelope}");
    envelope["code"].as_str().expect("a code")
}

/// The paths of one `get_lineage` answer, in order.
fn paths(envelope: &Value) -> Vec<String> {
    assert_eq!(envelope["ok"], json!(true), "{envelope}");
    envelope["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|node| node["path"].as_str().expect("a path").to_owned())
        .collect()
}

/// One promote command that must succeed, and its receipt.
fn promote(dir: &Path, ws: &str, args: &[&str]) -> Value {
    let mut all = vec!["--workspace", ws, "promote"];
    all.extend_from_slice(args);
    all.push("--json");
    json(dir, &all)
}

/// Runs one promote command built from owned arguments.
fn promote_owned(dir: &Path, ws: &str, args: &[String]) -> Output {
    let mut all: Vec<&str> = vec!["--workspace", ws, "promote"];
    all.extend(args.iter().map(String::as_str));
    all.push("--json");
    run(dir, &all)
}

// ──────────────────────────────── the write ────────────────────────────────

#[test]
fn case_7_1_link_patches_supersedes_and_touches_nothing_else() {
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, false, false);
    let primary = root.join("primary");
    let etag = blake(&primary.join(TARGET));

    let receipt = promote(
        root,
        &ws,
        &[
            SOURCE,
            "--link-from",
            TARGET,
            "--expect-hash",
            etag.as_str(),
        ],
    );

    assert_eq!(receipt["ok"], json!(true), "{receipt}");
    assert_eq!(receipt["path"], json!(TARGET), "{receipt}");
    assert_eq!(receipt["changed_keys"], json!(["supersedes"]), "{receipt}");
    assert_eq!(receipt["appended_keys"], json!(["supersedes"]), "{receipt}");
    assert_eq!(receipt["old_hash"], json!(etag), "{receipt}");
    // The receipt names the corpus and the path it wrote (§3.7 item 7).
    assert_eq!(receipt["source_corpus"], json!("context"), "{receipt}");
    assert_eq!(receipt["source"], json!(SOURCE), "{receipt}");
    assert_eq!(text(&primary, TARGET), TARGET_AFTER);

    // The link text is the qualified one, so a reader of the primary corpus can
    // see which corpus the document came from (§6.11).
    assert!(TARGET_AFTER.contains("supersedes: context:"));
}

#[test]
fn case_7_1b_a_second_link_is_refused_and_the_bytes_stay() {
    // The writer replaces one value and cannot append to a list, so a link that
    // would drop the one the document already holds is refused (M7-59).
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, false, false);
    let primary = root.join("primary");
    let before = blake(&primary.join(LINKED));

    let output = promote_owned(
        root,
        &ws,
        &[
            SOURCE.to_owned(),
            "--link-from".to_owned(),
            LINKED.to_owned(),
            "--expect-hash".to_owned(),
            before.clone(),
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(4),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let failure: Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("one JSON object");
    assert_eq!(failure["code"], json!("refused"), "{failure}");
    let message = failure["message"].as_str().expect("a message");
    assert!(
        message.contains(EARLY),
        "{message} must name the link it keeps"
    );
    assert_eq!(blake(&primary.join(LINKED)), before);
}

#[test]
fn case_7_2_adopt_creates_a_copy_and_leaves_the_source_in_place() {
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, false, false);
    let primary = root.join("primary");
    let archive = root.join("context");
    let source_before = blake(&archive.join(SOURCE_PATH));

    let receipt = promote(
        root,
        &ws,
        &[SOURCE, "--adopt", "--target-path", "decisions/adopted.md"],
    );

    assert_eq!(receipt["ok"], json!(true), "{receipt}");
    assert_eq!(receipt["path"], json!("decisions/adopted.md"), "{receipt}");
    assert_eq!(receipt["old_hash"], json!(null), "{receipt}");
    assert_eq!(receipt["source_corpus"], json!("context"), "{receipt}");
    assert_eq!(text(&primary, "decisions/adopted.md"), ADOPTED);
    // The source is still where it was, byte for byte.
    assert_eq!(text(&archive, SOURCE_PATH), SOURCE_BEFORE);
    assert_eq!(blake(&archive.join(SOURCE_PATH)), source_before);
}

#[test]
fn case_7_3_the_archive_keeps_its_bytes_and_its_mtime() {
    // The archived corpus is writable here, so `read_only` cannot be what keeps
    // a write out: the source can never be the target of the call (M7-23).
    // Removing that guard makes this case fail.
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, true, true);
    let archive = root.join("context");
    let source = archive.join(SOURCE_PATH);
    let before_hash = blake(&source);
    let before_mtime = mtime(&source);

    // Link mode, then adopt mode, both naming the archive.
    let etag = blake(&root.join("primary").join(TARGET));
    assert_eq!(
        promote(
            root,
            &ws,
            &[
                SOURCE,
                "--link-from",
                TARGET,
                "--expect-hash",
                etag.as_str()
            ]
        )["ok"],
        json!(true)
    );
    assert_eq!(
        promote(
            root,
            &ws,
            &[SOURCE, "--adopt", "--target-path", "decisions/adopted.md"],
        )["ok"],
        json!(true)
    );

    // A promote whose target is the source document itself. The corpus it names
    // is writable, so a refusal here can only come from the source rule.
    let refusal = rpc(
        root,
        &ws,
        "promote_document",
        json!({
            "source": SOURCE,
            "target_path": format!("context:{SOURCE_PATH}"),
            "mode": "link",
            "expected_hash": before_hash,
        }),
    );
    assert_eq!(code(&refusal), "read_only_corpus", "{refusal}");

    assert_eq!(blake(&source), before_hash, "the archive changed");
    assert_eq!(mtime(&source), before_mtime, "the archive was touched");
}

#[test]
fn case_7_4_an_inert_link_is_reported_and_kept_out_of_the_chain() {
    // The policy is off, so the written link resolves to nothing. This is the
    // M6b rule that stays until the workspace allows it (D42, case 7.4).
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, false, false);
    let primary = root.join("primary");
    let etag = blake(&primary.join(TARGET));
    assert_eq!(
        promote(
            root,
            &ws,
            &[
                SOURCE,
                "--link-from",
                TARGET,
                "--expect-hash",
                etag.as_str()
            ]
        )["ok"],
        json!(true)
    );
    index(root);

    // `doctor` reports the row, because the link is inert.
    let output = run(
        root,
        &[
            "--vault",
            &primary.display().to_string(),
            "--workspace",
            &ws,
            "doctor",
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: Value =
        serde_json::from_str(&String::from_utf8(output.stdout).expect("stdout is UTF-8"))
            .expect("one JSON object");
    let codes: Vec<&str> = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["code"].as_str().expect("a code"))
        .collect();
    assert_eq!(codes, ["cross_corpus_link_unsupported"], "{report}");

    // The chain ends at the primary document and says why.
    let answer = rpc(
        root,
        &ws,
        "get_lineage",
        json!({ "path": TARGET, "corpus": "all" }),
    );
    assert_eq!(paths(&answer), [TARGET]);
    let notes = answer["notes"].as_array().expect("notes");
    assert_eq!(notes.len(), 1, "{answer}");
    assert!(notes[0].as_str().is_some_and(|note| note.contains(SOURCE)));
}

#[test]
fn case_7_5_a_target_in_an_archived_corpus_is_refused() {
    // The archived role is read-only by default, so a promotion that writes
    // there is refused before any byte is read (M7-16, case 7.5).
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, false, false);

    let answer = rpc(
        root,
        &ws,
        "promote_document",
        json!({
            "source": format!("main:{TARGET}"),
            "mode": "adopt",
            "target_path": "context:decisions/copy.md",
        }),
    );
    assert_eq!(code(&answer), "read_only_corpus", "{answer}");
    assert!(
        !root.join("context").join("decisions/copy.md").exists(),
        "a write reached the archive"
    );
}

// ─────────────────────────────── resolution ────────────────────────────────

#[test]
fn case_7_6_the_promoted_link_resolves_through_get_lineage() {
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, true, false);
    let primary = root.join("primary");
    let etag = blake(&primary.join(TARGET));
    assert_eq!(
        promote(
            root,
            &ws,
            &[
                SOURCE,
                "--link-from",
                TARGET,
                "--expect-hash",
                etag.as_str()
            ]
        )["ok"],
        json!(true)
    );
    index(root);

    let answer = rpc(
        root,
        &ws,
        "get_lineage",
        json!({ "path": TARGET, "corpus": "all" }),
    );
    // The archived source is the root, and the promoted document is one step
    // below it: the link is a lineage edge, not a line in a file.
    assert_eq!(paths(&answer), [SOURCE, TARGET]);
    assert_eq!(answer["results"][0]["depth"], json!(0), "{answer}");
    assert_eq!(
        answer["results"][0]["effective_status"],
        json!("superseded"),
        "{answer}"
    );
    assert_eq!(answer["results"][1]["depth"], json!(1), "{answer}");
    assert_eq!(
        answer["results"][1]["predecessors"],
        json!([SOURCE]),
        "{answer}"
    );
    assert!(
        answer["notes"].as_array().is_some_and(Vec::is_empty),
        "{answer}"
    );

    // `doctor` stops reporting the row it reported in case 7.4.
    let report = json(
        root,
        &[
            "--vault",
            &primary.display().to_string(),
            "--workspace",
            &ws,
            "doctor",
            "--json",
        ],
    );
    assert_eq!(report["summary"]["errors"], json!(0), "{report}");
}

#[test]
fn case_7_7_the_policy_flip_resolves_with_no_rebuild() {
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, false, false);
    let primary = root.join("primary");
    let etag = blake(&primary.join(TARGET));
    assert_eq!(
        promote(
            root,
            &ws,
            &[
                SOURCE,
                "--link-from",
                TARGET,
                "--expect-hash",
                etag.as_str()
            ]
        )["ok"],
        json!(true)
    );
    index(root);

    // The policy is off: one node, and a note.
    let before = rpc(
        root,
        &ws,
        "get_lineage",
        json!({ "path": TARGET, "corpus": "all" }),
    );
    assert_eq!(paths(&before), [TARGET]);
    let manifest = primary.join(".janus").join("manifest.json");
    let built = fs::read(&manifest).expect("the manifest");

    // Flip the workspace file and nothing else. The policy is read when the
    // graph is built, so no index has to be rebuilt (case 7.7).
    let ws = write_workspace(root, true, false);
    let after = rpc(
        root,
        &ws,
        "get_lineage",
        json!({ "path": TARGET, "corpus": "all" }),
    );
    assert_eq!(paths(&after), [SOURCE, TARGET]);
    assert_eq!(
        fs::read(&manifest).expect("the manifest"),
        built,
        "the index was rebuilt"
    );
}

#[test]
fn case_7_8_an_unknown_corpus_is_not_found_and_an_excluded_one_ends_the_chain() {
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, true, false);
    let primary = root.join("primary");
    common::write_document(
        &primary,
        "decisions/2026-03-03-typo.md",
        "---\ntitle: A typo\ndate: 2026-03-03\nsupersedes: nosuch:decisions/2020-01-01-postgres.md\n---\n\nA typo.\n",
    );
    let etag = blake(&primary.join(TARGET));
    assert_eq!(
        promote(
            root,
            &ws,
            &[
                SOURCE,
                "--link-from",
                TARGET,
                "--expect-hash",
                etag.as_str()
            ]
        )["ok"],
        json!(true)
    );
    index(root);

    // A corpus this workspace does not hold is `not_found`, and the message
    // lists the ids it does hold (case 7.8).
    let answer = rpc(
        root,
        &ws,
        "get_lineage",
        json!({ "path": "decisions/2026-03-03-typo.md" }),
    );
    assert_eq!(code(&answer), "not_found", "{answer}");
    let message = answer["message"].as_str().expect("a message");
    for id in ["nosuch", "main", "context"] {
        assert!(message.contains(id), "{message} must name {id}");
    }

    // A link into a corpus this query does not read ends the chain with a note,
    // and the corpus needs no generation to say so.
    let answer = rpc(root, &ws, "get_lineage", json!({ "path": TARGET }));
    assert_eq!(paths(&answer), [TARGET]);
    let notes: Vec<&str> = answer["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(notes.len(), 1, "{answer}");
    assert!(notes[0].contains("context"), "{notes:?}");
    assert!(notes[0].contains("does not read"), "{notes:?}");
}

#[test]
fn a_promote_source_must_be_markdown() {
    let dir = copied();
    let root = dir.path();
    let ws = write_workspace(root, false, false);
    fs::write(root.join("context/decisions/notes.txt"), "text\n").expect("write the file");

    let output = promote_owned(
        root,
        &ws,
        &[
            "context:decisions/notes.txt".to_owned(),
            "--adopt".to_owned(),
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
