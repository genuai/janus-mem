//! `janus scan` end-to-end: corpus discovery, the walk, and exit codes.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

fn janus() -> Command {
    Command::cargo_bin("janus").unwrap()
}

/// Creates a vault with a config and a small, deliberately mixed tree.
fn sample_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\n\n[index]\nexclude = [\"archive/private/**\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".janus")).unwrap();
    fs::create_dir_all(root.join("decisions")).unwrap();
    fs::create_dir_all(root.join("archive/private")).unwrap();
    fs::create_dir_all(root.join("target")).unwrap();

    fs::write(root.join("notes.md"), "---\ntitle: Notes\n---\n\n# Notes\n").unwrap();
    fs::write(root.join("decisions/adr-1.md"), "# ADR 1\n").unwrap();
    fs::write(root.join("decisions/draft.txt"), "not markdown").unwrap();
    fs::write(root.join("archive/private/secret.md"), "# secret\n").unwrap();
    fs::write(root.join(".hidden.md"), "# hidden\n").unwrap();
    fs::write(root.join("target/build.md"), "# build\n").unwrap();

    dir
}

#[test]
fn scan_lists_markdown_and_attributes_skips() {
    let vault = sample_vault();

    janus()
        .arg("--vault")
        .arg(vault.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("decisions/adr-1.md"))
        .stdout(predicate::str::contains("notes.md"))
        .stdout(predicate::str::contains("2 file(s)"))
        .stdout(predicate::str::contains("archive/private/secret.md").not())
        .stdout(predicate::str::contains(".hidden.md").not())
        .stdout(predicate::str::contains("config_exclude"))
        .stdout(predicate::str::contains("not_markdown"));
}

#[test]
fn scan_finds_the_corpus_from_a_subdirectory() {
    let vault = sample_vault();
    let nested = vault.path().join("decisions");

    janus()
        .current_dir(&nested)
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("notes.md"));
}

#[test]
fn scan_reports_a_corpus_without_a_config() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".janus")).unwrap();

    // `.janus/` makes the directory discoverable, and the missing config is then
    // reported by name rather than as a mystery.
    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("scan")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("janus.toml"));
}

#[test]
fn scan_fails_clearly_when_no_corpus_can_be_found() {
    let dir = tempfile::tempdir().unwrap();

    janus()
        .current_dir(dir.path())
        .arg("scan")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("no corpus found"))
        .stderr(predicate::str::contains("janus init"));
}

#[test]
fn scan_refuses_a_vault_path_that_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    fs::write(&file, "# notes\n").unwrap();

    janus()
        .arg("--vault")
        .arg(&file)
        .arg("scan")
        .assert()
        .code(4);
}

#[test]
fn scan_works_on_an_empty_corpus() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("janus.toml"), "index_dir = \".janus\"\n").unwrap();

    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("0 file(s)"));
}

#[test]
fn scan_surfaces_config_warnings_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\n\n[authority]\nweird = 0.0\n",
    )
    .unwrap();

    janus()
        .arg("--vault")
        .arg(dir.path())
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("warning:"))
        .stdout(predicate::str::contains("weird"));
}

#[test]
fn scan_never_writes_to_the_vault() {
    let vault = sample_vault();
    let before = snapshot(vault.path());

    janus()
        .arg("--vault")
        .arg(vault.path())
        .arg("scan")
        .assert()
        .success();

    assert_eq!(
        before,
        snapshot(vault.path()),
        "scan must not mutate the vault"
    );
}

/// A cheap recursive listing: every path plus its length.
fn snapshot(root: &Path) -> Vec<(String, u64)> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.push((rel, entry.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
    }

    let mut out = Vec::new();
    visit(root, root, &mut out);
    out.sort();
    out
}
