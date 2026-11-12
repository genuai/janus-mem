//! M6b phase 6 cases 6.1–6.5: `index`, `scan` and `status` count what the
//! prune leaves (§3.6 of the M6b plan).
//!
//! Every case runs the binary against a copy of `tests/fixtures/workspace-nested`,
//! whose archived root lies inside the primary's root. The primary's own tree is
//! one document; the nested tree is another. So the walk admits two and the
//! prune leaves one, and a printed `2` is a walk count while a printed `1` is the
//! published count. No case sleeps, reads the clock, or writes to the fixture.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use common::copy_tree;
use serde_json::{Value, json};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/workspace-nested"
);

/// The primary corpus's root, relative to the fixture copy.
const PRIMARY: &str = "primary";

/// The document the primary owns, and the one its nested corpus owns.
const OWNED: &str = "notes/shared.md";

/// A private copy of the committed fixture, without any index.
fn copied() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir for the fixture copy");
    copy_tree(Path::new(FIXTURE), dir.path()).expect("copy the fixture");
    dir
}

/// A `janus` command with no workspace in the ambient configuration, so a case
/// states how its workspace was named.
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

fn run(dir: &Path, args: &[&str]) -> Output {
    janus_in(dir).args(args).output().expect("run janus")
}

/// Runs one command that must succeed, and returns its stdout and stderr.
fn run_ok(dir: &Path, args: &[&str]) -> (String, String) {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "janus {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    )
}

/// One `--json` report of the workspace the case names.
fn report(dir: &Path, workspace: &Path, args: &[&str]) -> Value {
    let named = workspace.display().to_string();
    let mut all: Vec<&str> = args.to_vec();
    all.extend(["--workspace", &named, "--json"]);
    let (text, _) = run_ok(dir, &all);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("stdout is not JSON ({err}):\n{text}"))
}

/// The document count a manifest states.
fn docs(dir: &Path, corpus: &str) -> u64 {
    let path = dir.join(corpus).join(".janus/manifest.json");
    let manifest: Value =
        serde_json::from_slice(&fs::read(path).expect("the manifest")).expect("JSON");
    manifest["docs"].as_u64().expect("docs")
}

/// Writes one file, creating its directory.
fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("a parent")).expect("create the directory");
    fs::write(path, text).expect("write the file");
}

/// A second nested corpus inside the primary, so the prune holds two roots.
fn second_nested(dir: &Path) -> PathBuf {
    let retired = dir.join(PRIMARY).join("retired");
    write(
        &retired.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"retired\"\n\n[embedding]\nenabled = false\n",
    );
    for name in ["one", "two"] {
        write(
            &retired.join("notes").join(format!("{name}.md")),
            &format!("# Retired {name}\n\nOld fleet note {name}.\n"),
        );
    }
    let workspace = dir.join("two-nested.toml");
    write(
        &workspace,
        "[[corpora]]\nid = \"main\"\nroot = \"primary\"\nrole = \"primary\"\n\n\
         [[corpora]]\nid = \"context\"\nroot = \"primary/archive\"\nrole = \"archived\"\n\n\
         [[corpora]]\nid = \"retired\"\nroot = \"primary/retired\"\nrole = \"archived\"\n",
    );
    workspace
}

// ─────────────────────────────── the cases ─────────────────────────────────

#[test]
fn case_6_1_index_scan_and_status_count_what_the_prune_leaves() {
    let dir = copied();
    let named = dir.path().join("workspace.toml");

    // `index` prints the count it publishes, and it publishes the pruned count.
    let (stdout, stderr) = run_ok(
        dir.path(),
        &[
            "--vault",
            PRIMARY,
            "index",
            "--no-embeddings",
            "--workspace",
            &named.display().to_string(),
        ],
    );
    assert!(stdout.contains("1 document(s)"), "{stdout}");
    assert!(
        stderr.contains("is inside corpus"),
        "one prune line: {stderr}"
    );
    assert_eq!(docs(dir.path(), PRIMARY), 1, "what the manifest states");

    // The dry run and the receipt report the same number the manifest holds.
    let receipt = report(
        dir.path(),
        &named,
        &["--vault", PRIMARY, "index", "--dry-run"],
    );
    assert_eq!(receipt["docs"], json!(1), "{receipt}");

    // `scan` reports the pruned corpus, not the walk that found the tree.
    let scanned = report(dir.path(), &named, &["--vault", PRIMARY, "scan"]);
    assert_eq!(scanned["docs"].as_array().expect("docs").len(), 1);
    assert_eq!(scanned["walk"]["files"], json!(1), "{scanned}");

    // `status` calls that index fresh, because its walk is the build's walk.
    let status = report(dir.path(), &named, &["--vault", PRIMARY, "status"]);
    assert_eq!(status["index"]["stale"], json!(false), "{status}");
    assert_eq!(status["index"]["docs"], json!(1));
    assert_eq!(status["metadata"]["dated"], json!(1));
    assert_eq!(status["metadata"]["undated"], json!(0));

    // The human forms print the same numbers as the JSON ones.
    let named_text = named.display().to_string();
    let (scan, _) = run_ok(
        dir.path(),
        &["--vault", PRIMARY, "scan", "--workspace", &named_text],
    );
    assert!(scan.contains("1 document(s)"), "{scan}");
    let (state, _) = run_ok(
        dir.path(),
        &["--vault", PRIMARY, "status", "--workspace", &named_text],
    );
    assert!(state.contains("fresh: 1 documents"), "{state}");
    assert!(!state.contains("stale"), "{state}");
}

#[test]
fn case_6_2_the_ambient_workspace_counts_the_same_as_the_flag() {
    let dir = copied();
    let named = dir.path().join("workspace.toml").display().to_string();

    // The build names the workspace by variable only, as a shell session does.
    let built = janus_in(dir.path())
        .env("JANUS_WORKSPACE", &named)
        .args(["--vault", PRIMARY, "index", "--no-embeddings"])
        .output()
        .expect("run janus index");
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert_eq!(docs(dir.path(), PRIMARY), 1);

    // The same three commands answer the same numbers with the variable.
    let scanned = janus_in(dir.path())
        .env("JANUS_WORKSPACE", &named)
        .args(["--vault", PRIMARY, "scan", "--json"])
        .output()
        .expect("run janus scan");
    let scanned: Value = serde_json::from_slice(&scanned.stdout).expect("JSON");
    assert_eq!(scanned["walk"]["files"], json!(1), "{scanned}");

    let state = janus_in(dir.path())
        .env("JANUS_WORKSPACE", &named)
        .args(["--vault", PRIMARY, "status", "--json"])
        .output()
        .expect("run janus status");
    let state: Value = serde_json::from_slice(&state.stdout).expect("JSON");
    assert_eq!(state["index"]["stale"], json!(false), "{state}");
    assert_eq!(state["index"]["docs"], json!(1));
}

#[test]
fn case_6_3_two_nested_corpora_are_counted_not_subtracted() {
    let dir = copied();
    let named = second_nested(dir.path());

    // The primary owns one document. The two nested corpora hold one and two,
    // so the walk would admit four and the prune leaves one.
    run_ok(
        dir.path(),
        &[
            "--vault",
            PRIMARY,
            "index",
            "--no-embeddings",
            "--workspace",
            &named.display().to_string(),
        ],
    );
    assert_eq!(docs(dir.path(), PRIMARY), 1);

    let (_, stderr) = run_ok(
        dir.path(),
        &[
            "--vault",
            PRIMARY,
            "scan",
            "--workspace",
            &named.display().to_string(),
        ],
    );
    assert_eq!(
        stderr.matches("is inside corpus").count(),
        2,
        "one line per nested root: {stderr}"
    );

    let scanned = report(dir.path(), &named, &["--vault", PRIMARY, "scan"]);
    assert_eq!(scanned["walk"]["files"], json!(1), "{scanned}");
    let status = report(dir.path(), &named, &["--vault", PRIMARY, "status"]);
    assert_eq!(status["index"]["stale"], json!(false), "{status}");

    // The listing names both children, so the count came from one list.
    let listed = report(dir.path(), &named, &["corpora"]);
    assert_eq!(
        listed["corpora"][0]["pruned"],
        json!(["context", "retired"])
    );
}

#[test]
fn case_6_4_a_workspace_that_names_no_corpus_at_the_root_is_a_usage_error() {
    let dir = copied();
    // A workspace naming only the nested corpus, asked to prune the parent.
    let other = dir.path().join("other.toml");
    write(
        &other,
        "[[corpora]]\nid = \"context\"\nroot = \"primary/archive\"\nrole = \"primary\"\n",
    );
    let output = run(
        dir.path(),
        &[
            "--vault",
            PRIMARY,
            "index",
            "--workspace",
            &other.display().to_string(),
        ],
    );
    assert_eq!(output.status.code(), Some(2), "a usage failure");
    assert!(output.stdout.is_empty(), "a refused command prints nothing");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no corpus has root"), "{stderr}");
    assert!(stderr.contains("other.toml"), "the file is named: {stderr}");
}

#[test]
fn case_6_5_a_command_that_reads_no_workspace_still_refuses_the_flag() {
    let dir = copied();
    let named = dir.path().join("workspace.toml");
    // The list is deliberate: these commands read one corpus and apply no prune.
    // `doctor` reads a workspace for the corpora a qualified link may reach
    // (M7-37), so it is not one of them.
    for command in ["init", "chunks", "ids", "moves", "lineage", "suggest-links"] {
        let output = run(
            dir.path(),
            &["--workspace", &named.display().to_string(), command],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(2),
            "`janus {command} --workspace` must be a usage error: {stderr}"
        );
        assert!(output.stdout.is_empty(), "{command} printed a report");
        assert!(
            stderr.contains(&format!("`janus {command}`")) && stderr.contains("cannot federate"),
            "the message names the command and the reason: {stderr}"
        );
    }
}

/// The one path both roots hold, so a case can prove which corpus answered.
#[test]
fn the_fixture_is_the_nested_case() {
    let dir = copied();
    assert!(dir.path().join(PRIMARY).join(OWNED).is_file());
    assert!(
        dir.path()
            .join(PRIMARY)
            .join("archive")
            .join(OWNED)
            .is_file()
    );
}
