use assert_cmd::Command;
use predicates::str::contains;
use std::fs;

fn janus_cmd() -> Command {
    Command::cargo_bin("janus").unwrap()
}

#[test]
fn test_cli_version() {
    janus_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn test_cli_init_success() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    janus_cmd()
        .arg("--vault")
        .arg(root)
        .arg("init")
        .assert()
        .success();

    assert!(root.join(".janus").is_dir());
    assert!(root.join("janus.toml").is_file());

    let content = fs::read_to_string(root.join("janus.toml")).unwrap();
    assert!(content.contains("index_dir = \".janus\""));
}

#[test]
fn test_cli_init_twice_fails() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    // first time succeeds
    janus_cmd()
        .arg("--vault")
        .arg(root)
        .arg("init")
        .assert()
        .success();

    // second time fails
    janus_cmd()
        .arg("--vault")
        .arg(root)
        .arg("init")
        .assert()
        .code(4)
        .stderr(contains("already"));
}

#[test]
fn test_cli_vault_does_not_exist() {
    // Portable: a child of a temp dir that was never created.
    let temp_dir = tempfile::tempdir().unwrap();
    let missing = temp_dir.path().join("does-not-exist");

    janus_cmd()
        .arg("--vault")
        .arg(&missing)
        .arg("init")
        .assert()
        .code(1)
        // The error must name the path that failed, not just the OS error.
        .stderr(contains("does-not-exist"));
}
