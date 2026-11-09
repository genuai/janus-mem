//! M6b phase 1 cases 1.1–1.5: the workspace file, its resolution, and
//! `janus corpora` (§3.1 of the M6b plan, §6.11 of SPEC).
//!
//! Every case runs the binary with `XDG_CONFIG_HOME` pointed at a temporary
//! directory and `JANUS_WORKSPACE`, `APPDATA` and `HOME` removed, so no case
//! reads the developer's own config directory or answers from a workspace that
//! happens to exist on the machine.
//!
//! The committed fixture under `tests/fixtures/workspace/` is copied to a
//! temporary directory first: its roots are relative to the workspace file, so
//! the copy is what the loader resolves. Nothing writes to the committed tree.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::{Value, json};

const WORKSPACE_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/workspace");

// ──────────────────────────────── helpers ──────────────────────────────────

/// A `janus` command with a private platform config directory.
fn janus_in(config_home: &Path) -> Command {
    let mut command = Command::cargo_bin("janus").expect("the janus binary must build for tests");
    command
        .env("RUST_LOG", "off")
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("JANUS_WORKSPACE")
        .env_remove("APPDATA")
        .env_remove("HOME");
    command
}

/// Runs `janus corpora --json` with zero, one or two named sources.
fn corpora_from(config_home: &Path, flag: Option<&Path>, env: Option<&Path>) -> Output {
    let mut command = janus_in(config_home);
    command.arg("corpora").arg("--json");
    if let Some(path) = flag {
        command.arg("--workspace").arg(path);
    }
    if let Some(path) = env {
        command.env("JANUS_WORKSPACE", path);
    }
    command.output().expect("run janus corpora")
}

/// Parses a command's stdout as the report, asserting success first.
fn report_of(output: Output) -> Value {
    assert!(
        output.status.success(),
        "janus corpora failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Runs `janus corpora --json` on a named workspace and parses the report.
fn corpora(config_home: &Path, workspace: &Path) -> Value {
    report_of(corpora_from(config_home, Some(workspace), None))
}

/// One row of the report, by corpus id.
fn row<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["corpora"]
        .as_array()
        .expect("corpora is an array")
        .iter()
        .find(|row| row["id"] == json!(id))
        .unwrap_or_else(|| panic!("no corpus {id:?} in {report}"))
}

/// The committed workspace fixture, copied to a temporary directory.
fn workspace_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir for the workspace fixture");
    common::copy_tree(Path::new(WORKSPACE_FIXTURE), dir.path())
        .expect("copy the workspace fixture");
    dir
}

/// A corpus with a config and one document, under `dir`.
fn scratch_corpus(dir: &Path, id: &str) -> PathBuf {
    let root = dir.join(id);
    fs::create_dir_all(&root).expect("create the corpus");
    fs::write(root.join("janus.toml"), format!("corpus_id = \"{id}\"\n"))
        .expect("write the corpus config");
    fs::write(
        root.join("notes.md"),
        "---\ntitle: Notes\ndate: 2024-01-01\n---\n\nprose\n",
    )
    .expect("write a document");
    root
}

/// A one-primary workspace naming `root`, written at `path`.
///
/// The root is a TOML literal string, so a Windows path keeps its backslashes.
fn workspace_for(path: &Path, id: &str, root: &Path) -> PathBuf {
    fs::write(
        path,
        format!(
            "[[corpora]]\nid = \"{id}\"\nroot = '{}'\nrole = \"primary\"\n",
            root.display()
        ),
    )
    .expect("write the workspace");
    path.to_path_buf()
}

/// A workspace that names `root` once, with an id to match, all under `dir`.
fn one_corpus_workspace(dir: &Path, id: &str) -> PathBuf {
    let root = scratch_corpus(dir, &format!("{id}-corpus"));
    workspace_for(&dir.join(format!("{id}.toml")), id, &root)
}

/// The failure of one `janus corpora` run: its exit code and stderr.
fn failure(output: Output) -> (Option<i32>, String) {
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ────────────────────────────── case 1.1 ───────────────────────────────────

#[test]
fn case_1_1_the_flag_beats_the_env_and_the_env_beats_the_config_dir() {
    let home = tempfile::tempdir().expect("tempdir for the config home");
    let dir = tempfile::tempdir().expect("tempdir for the workspaces");
    let flag = one_corpus_workspace(dir.path(), "flag");
    let env = one_corpus_workspace(dir.path(), "env");
    let platform = one_corpus_workspace(dir.path(), "platform");
    let installed = home.path().join("janus").join("workspace.toml");
    fs::create_dir_all(installed.parent().expect("the config dir"))
        .expect("create the config directory");
    fs::copy(&platform, &installed).expect("install the platform workspace");

    let all = report_of(corpora_from(home.path(), Some(&flag), Some(&env)));
    assert_eq!(all["corpora"][0]["id"], json!("flag"), "{all}");

    let two = report_of(corpora_from(home.path(), None, Some(&env)));
    assert_eq!(two["corpora"][0]["id"], json!("env"), "{two}");

    let last = report_of(corpora_from(home.path(), None, None));
    assert_eq!(last["corpora"][0]["id"], json!("platform"), "{last}");
    assert_eq!(last["workspace"], json!(installed.display().to_string()));

    // No source at all is a usage error that names the three places to look.
    let empty = tempfile::tempdir().expect("tempdir for an empty config home");
    let (code, stderr) = failure(corpora_from(empty.path(), None, None));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("--workspace"), "{stderr}");
    assert!(stderr.contains("JANUS_WORKSPACE"), "{stderr}");
    assert!(stderr.contains("workspace.toml"), "{stderr}");

    // A named path that does not exist is an error, not a fall-through.
    let absent = dir.path().join("absent.toml");
    let (code, stderr) = failure(corpora_from(home.path(), Some(&absent), None));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("invalid workspace"), "{stderr}");
    assert!(stderr.contains("absent.toml"), "{stderr}");
}

// ────────────────────────────── case 1.2 ───────────────────────────────────

#[test]
fn case_1_2_no_workspace_changes_nothing_about_a_single_corpus_report() {
    let home = tempfile::tempdir().expect("tempdir for an empty config home");
    let vault = common::fixture_vault();

    let output = janus_in(home.path())
        .arg("--vault")
        .arg(vault.path())
        .args([
            "search",
            "cache",
            "--limit",
            "1",
            "--no-embeddings",
            "--json",
        ])
        .output()
        .expect("run janus search");

    assert!(
        output.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    // The single-corpus report keeps its shape: no corpus set, no workspace.
    assert_eq!(report["api"], json!(1));
    assert_eq!(report["ok"], json!(true));
    assert!(report.get("corpora").is_none(), "{report}");
    assert!(report.get("workspace").is_none(), "{report}");
    // Resolution happens only where a workspace is read, so a query that has
    // none says nothing about one.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("workspace"), "{stderr}");
    assert!(!stderr.contains("warning:"), "{stderr}");
}

// ────────────────────────────── case 1.3 ───────────────────────────────────

#[test]
fn case_1_3_two_primaries_name_the_file_and_both_entries() {
    let home = tempfile::tempdir().expect("tempdir for the config home");
    let dir = tempfile::tempdir().expect("tempdir for the corpora");
    let first = scratch_corpus(dir.path(), "one");
    let second = scratch_corpus(dir.path(), "two");
    let path = dir.path().join("workspace.toml");
    fs::write(
        &path,
        format!(
            "[[corpora]]\nid = \"one\"\nroot = '{}'\nrole = \"primary\"\n\n\
             [[corpora]]\nid = \"two\"\nroot = '{}'\nrole = \"primary\"\n",
            first.display(),
            second.display()
        ),
    )
    .expect("write the workspace");

    let (code, stderr) = failure(corpora_from(home.path(), Some(&path), None));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(stderr.contains("corpora[0]"), "{stderr}");
    assert!(stderr.contains("corpora[1]"), "{stderr}");
}

#[test]
fn case_1_3_a_duplicate_id_names_both_entries() {
    let home = tempfile::tempdir().expect("tempdir for the config home");
    let dir = tempfile::tempdir().expect("tempdir for the corpora");
    let first = scratch_corpus(dir.path(), "one");
    let second = scratch_corpus(dir.path(), "two");
    let path = dir.path().join("workspace.toml");
    fs::write(
        &path,
        format!(
            "[[corpora]]\nid = \"same\"\nroot = '{}'\nrole = \"primary\"\n\n\
             [[corpora]]\nid = \"same\"\nroot = '{}'\nrole = \"archived\"\n",
            first.display(),
            second.display()
        ),
    )
    .expect("write the workspace");

    let (code, stderr) = failure(corpora_from(home.path(), Some(&path), None));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(stderr.contains("corpora[0]"), "{stderr}");
    assert!(stderr.contains("corpora[1]"), "{stderr}");
    assert!(stderr.contains("same"), "{stderr}");
}

#[test]
fn case_1_3_a_missing_root_names_the_entry_and_the_file() {
    let home = tempfile::tempdir().expect("tempdir for the config home");
    let dir = tempfile::tempdir().expect("tempdir for the corpora");
    let path = dir.path().join("workspace.toml");
    fs::write(&path, "[[corpora]]\nid = \"main\"\nrole = \"primary\"\n")
        .expect("write the workspace");

    let (code, stderr) = failure(corpora_from(home.path(), Some(&path), None));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(stderr.contains("corpora[0]"), "{stderr}");
    assert!(stderr.contains("root"), "{stderr}");
}

#[test]
fn case_1_3_an_unknown_key_is_tolerated_with_a_warning() {
    let home = tempfile::tempdir().expect("tempdir for the config home");
    let dir = tempfile::tempdir().expect("tempdir for the corpora");
    let root = scratch_corpus(dir.path(), "main");
    let path = dir.path().join("workspace.toml");
    fs::write(
        &path,
        format!(
            "future_top_level = true\n\n\
             [[corpora]]\nid = \"main\"\nroot = '{}'\nrole = \"primary\"\n\
             future_entry_key = 7\n\n[search]\nfuture_search_key = \"x\"\n",
            root.display()
        ),
    )
    .expect("write the workspace");

    let output = corpora_from(home.path(), Some(&path), None);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let report = report_of(output);
    assert_eq!(report["corpora"][0]["id"], json!("main"), "{report}");
    for key in ["future_top_level", "future_entry_key", "future_search_key"] {
        assert!(stderr.contains("warning:"), "{stderr}");
        assert!(stderr.contains(key), "{stderr}");
    }
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
}

#[test]
fn case_1_3_an_id_the_command_line_cannot_name_is_an_error() {
    let home = tempfile::tempdir().expect("tempdir for the config home");
    let dir = tempfile::tempdir().expect("tempdir for the corpora");
    let root = scratch_corpus(dir.path(), "main");
    let path = dir.path().join("workspace.toml");
    fs::write(
        &path,
        format!(
            "[[corpora]]\nid = \"Main Vault\"\nroot = '{}'\nrole = \"primary\"\n",
            root.display()
        ),
    )
    .expect("write the workspace");

    let (code, stderr) = failure(corpora_from(home.path(), Some(&path), None));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(stderr.contains("corpora[0]"), "{stderr}");
    assert!(stderr.contains("[a-z0-9]"), "{stderr}");
}

// ────────────────────────────── case 1.4 ───────────────────────────────────

#[test]
fn case_1_4_an_archived_corpus_is_read_only_unless_it_says_otherwise() {
    let home = tempfile::tempdir().expect("tempdir for the config home");
    let fixture = workspace_fixture();
    let path = fixture.path().join("workspace.toml");

    let report = corpora(home.path(), &path);
    assert_eq!(row(&report, "context")["role"], json!("archived"));
    assert_eq!(
        row(&report, "context")["read_only"],
        json!(true),
        "{report}"
    );
    assert_eq!(row(&report, "main")["read_only"], json!(false), "{report}");

    // The entry now declares the write it is allowed, and the report says so.
    let declared = fs::read_to_string(&path)
        .expect("read the workspace")
        .replace(
            "role = \"archived\"",
            "role = \"archived\"\nread_only = false",
        );
    fs::write(&path, declared).expect("write the workspace");
    let report = corpora(home.path(), &path);
    assert_eq!(
        row(&report, "context")["read_only"],
        json!(false),
        "{report}"
    );
}

// ────────────────────────────── case 1.5 ───────────────────────────────────

#[test]
fn case_1_5_corpora_reports_freshness_without_building_or_locking() {
    let home = tempfile::tempdir().expect("tempdir for the config home");
    let fixture = workspace_fixture();
    let workspace = fixture.path().join("workspace.toml");
    let primary = fixture.path().join("primary");
    let archive = fixture.path().join("archive");

    let report = corpora(home.path(), &workspace);
    // The file lists the archive first; the report puts the primary first.
    let ids: Vec<&str> = report["corpora"]
        .as_array()
        .expect("corpora is an array")
        .iter()
        .map(|row| row["id"].as_str().expect("an id"))
        .collect();
    assert_eq!(ids, ["main", "context"], "{report}");

    for id in ["main", "context"] {
        let row = row(&report, id);
        assert_eq!(row["config_present"], json!(true), "{report}");
        assert_eq!(row["index_present"], json!(false), "{report}");
        assert_eq!(row["index_stale"], Value::Null, "{report}");
        // The root is canonical and absolute, whichever directory was copied.
        let directory = if id == "main" { "primary" } else { "archive" };
        assert!(
            row["root"].as_str().expect("a root").ends_with(directory),
            "{report}"
        );
    }
    // Asking must build nothing and take no lock.
    assert!(!primary.join(".janus").exists(), "corpora built an index");
    assert!(!archive.join(".janus").exists(), "corpora built an index");

    // Build the primary's index, then ask again.
    let built = janus_in(home.path())
        .arg("--vault")
        .arg(&primary)
        .args(["index", "--no-embeddings"])
        .output()
        .expect("run janus index");
    assert!(
        built.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let report = corpora(home.path(), &workspace);
    assert_eq!(
        row(&report, "main")["index_present"],
        json!(true),
        "{report}"
    );
    assert_eq!(
        row(&report, "main")["index_stale"],
        json!(false),
        "{report}"
    );
    assert_eq!(
        row(&report, "context")["index_present"],
        json!(false),
        "{report}"
    );

    // A new document changes the corpus, so the index is stale.
    common::write_document(
        &primary,
        "notes/2024-07-01-later.md",
        "---\ntitle: Later\ndate: 2024-07-01\n---\n\nlater prose\n",
    );
    let report = corpora(home.path(), &workspace);
    assert_eq!(row(&report, "main")["index_stale"], json!(true), "{report}");
}
