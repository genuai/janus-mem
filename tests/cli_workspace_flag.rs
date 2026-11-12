//! M6b phase 4: `--workspace` is one global flag (§9.1, §6.11 of SPEC).
//!
//! The flag moved beside `--vault`, so the parser accepts it on every command.
//! That is only safe if a command that works on one corpus refuses it:
//! `janus index --workspace w.toml` would otherwise publish one corpus and
//! report success. Both halves are here, on a copy of the fixture workspace with
//! a private `XDG_CONFIG_HOME` and no `JANUS_WORKSPACE`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use common::{copy_tree, mcp_call, mcp_opening};
use serde_json::{Value, json};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/workspace");
const ARCHIVED: &str = "2019-01-01-cache-server.md";

fn copied() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir for the fixture copy");
    copy_tree(Path::new(FIXTURE), dir.path()).expect("copy the fixture workspace");
    dir
}

/// The workspace file inside one fixture copy.
fn workspace_file(dir: &Path) -> PathBuf {
    dir.join("workspace.toml")
}

/// A `janus` command for one fixture copy, with no ambient workspace.
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

/// Runs `janus --workspace <file> <args…>`: the flag before the subcommand.
fn run_with_flag(dir: &Path, args: &[&str]) -> Output {
    janus_in(dir)
        .arg("--workspace")
        .arg(workspace_file(dir))
        .args(args)
        .output()
        .expect("run janus")
}

/// Parses a command's stdout as one JSON object, asserting success first.
fn json_of(output: Output) -> Value {
    assert!(
        output.status.success(),
        "janus failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Every stdout line of a session, parsed as one message.
fn messages(output: Output) -> Vec<Value> {
    String::from_utf8(output.stdout)
        .expect("stdout is UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON message per line"))
        .collect()
}

/// The corpus ids a report lists, in the order it lists them.
fn corpora_of(report: &Value) -> Vec<String> {
    report["corpora"]
        .as_array()
        .expect("the report lists the corpora it read")
        .iter()
        .map(|id| id.as_str().expect("a corpus id").to_owned())
        .collect()
}

#[test]
fn case_4_4_the_flag_before_the_subcommand_reads_the_workspace() {
    let dir = copied();

    // The listing command: both corpora, primary first.
    let report = json_of(run_with_flag(dir.path(), &["corpora", "--json"]));
    let ids: Vec<&str> = report["corpora"]
        .as_array()
        .expect("one row per corpus")
        .iter()
        .map(|row| row["id"].as_str().expect("an id"))
        .collect();
    assert_eq!(ids, ["main", "context"], "primary first, then archived");

    // `status`: the workspace path and one row per corpus.
    let report = json_of(run_with_flag(dir.path(), &["status", "--json"]));
    assert_eq!(
        report["workspace"].as_str(),
        Some(workspace_file(dir.path()).display().to_string().as_str())
    );
    assert_eq!(report["corpora"].as_array().expect("rows").len(), 2);

    // `search`: a federated query reads both corpora and names each row's own.
    let report = json_of(run_with_flag(
        dir.path(),
        &[
            "search",
            "cache",
            "--corpus",
            "all",
            "--no-embeddings",
            "--json",
        ],
    ));
    assert_eq!(corpora_of(&report), ["main", "context"]);
    let corpora: Vec<&str> = report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|row| row["corpus"].as_str().expect("every row names its corpus"))
        .collect();
    assert!(
        corpora.contains(&"context"),
        "`all` returns an archived row: {corpora:?}"
    );

    // `facts`: the same selection, through the same flag.
    let report = json_of(run_with_flag(
        dir.path(),
        &["facts", "--corpus", "all", "--no-embeddings", "--json"],
    ));
    assert_eq!(corpora_of(&report), ["main", "context"]);

    // `read`: the archived corpus answers for a document only it holds.
    let report = json_of(run_with_flag(
        dir.path(),
        &["read", ARCHIVED, "--corpus", "context", "--json"],
    ));
    assert_eq!(report["corpus"], "context");

    // `rpc`: one line in, one envelope out, with the flag before the command.
    let script = format!(
        "{}\n",
        json!({ "id": 1, "tool": "janus_status", "params": {} })
    );
    let answers = messages(
        janus_in(dir.path())
            .arg("--workspace")
            .arg(workspace_file(dir.path()))
            .arg("rpc")
            .write_stdin(script)
            .output()
            .expect("run janus rpc"),
    );
    assert_eq!(answers.len(), 1, "one request, one answer");
    assert_eq!(answers[0]["ok"], json!(true));
    assert_eq!(
        answers[0]["workspace"].as_str(),
        Some(workspace_file(dir.path()).display().to_string().as_str())
    );

    // `mcp`: the handshake, then one tool call, again with the flag first.
    let script = format!(
        "{}{}",
        mcp_opening(),
        mcp_call(2, "janus_status", json!({}))
    );
    let answers = messages(
        janus_in(dir.path())
            .arg("--workspace")
            .arg(workspace_file(dir.path()))
            .arg("mcp")
            .write_stdin(script)
            .output()
            .expect("run janus mcp"),
    );
    assert_eq!(answers.len(), 2, "the handshake and the call: {answers:?}");
    let envelope = &answers[1]["result"]["structuredContent"];
    assert_eq!(envelope["ok"], json!(true));
    assert_eq!(
        envelope["corpora"].as_array().expect("rows").len(),
        2,
        "the server answers from the workspace the flag named"
    );
}

#[test]
fn case_4_5_a_command_that_works_on_one_corpus_refuses_the_flag() {
    let dir = copied();

    // Every command that reads one corpus and applies no prune. `scan`, `index`
    // and `status` are absent because they read the workspace's prune (M6b-30),
    // and `doctor` because it reads the corpora a qualified link may reach
    // (M7-37).
    for command in ["init", "chunks", "ids", "moves", "lineage", "suggest-links"] {
        let output = run_with_flag(dir.path(), &[command]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(2),
            "`janus {command} --workspace` must be a usage error: {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "a refused command prints no report: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            stderr.contains(&format!("`janus {command}`")) && stderr.contains("cannot federate"),
            "the message names the command and the reason: {stderr}"
        );
        assert!(
            stderr.contains(&workspace_file(dir.path()).display().to_string()),
            "the message names the path it refused: {stderr}"
        );
    }

    // A global flag's position cannot change the answer, so this refusal holds
    // after the subcommand too.
    let output = janus_in(dir.path())
        .args(["chunks", "--workspace"])
        .arg(workspace_file(dir.path()))
        .output()
        .expect("run janus chunks");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot federate"));
}
