//! Cases 1.1–1.6, 2.1–2.6 and 10.1 of the M6 plan: one envelope, one code
//! table, and no diagnostic on stdout (M6 §3.1, §3.2, §3.10).
//!
//! Case 1.3 is `tests/golden_search.rs`. The three committed snapshots are the
//! record of "two declared changes per report", and this file does not repeat
//! them. Every expected value below is written out by hand. The one computed
//! value is the blake3 of a fixture file: the test reads the bytes and compares
//! them with the reported hash, so a hash of the body, or of a stale file,
//! differs.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{facts_json, fixture_vault, index, json_ok, run, search_json};
use janus_mem::api::ApiCode;
use janus_mem::error::JanusError;
use serde_json::{Value, json};

/// The sorted keys of one object as one line, so a test can state a shape.
fn shape(value: &Value) -> String {
    let keys: Vec<&str> = value
        .as_object()
        .expect("keys")
        .keys()
        .map(String::as_str)
        .collect();
    keys.join(" ")
}

/// Runs a command that must fail, and returns the failure object and the code.
fn failure(root: &Path, args: &[&str]) -> (Value, Option<i32>) {
    let output = run(root, args);
    assert!(!output.status.success(), "{args:?} must fail: {output:?}");
    let text = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let value = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("stdout is not JSON ({err}):\n{text}"));
    (value, output.status.code())
}

/// Asserts that a row's `content_hash` is the blake3 of the file it names.
fn assert_hash_of_file(root: &Path, row: &Value) {
    let rel = row["rel_path"].as_str().expect("rel_path");
    let bytes = fs::read(root.join(rel)).expect("read the fixture file");
    let expected = blake3::hash(&bytes).to_hex().to_string();
    assert_eq!(row["content_hash"], json!(expected), "{row}");
}

/// Writes one document, creating its directories.
fn write(root: &Path, rel_path: &str, text: &str) {
    let path = root.join(rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create the directory");
    }
    fs::write(path, text).expect("write the document");
}

// ──────────────────────────────── the envelope ─────────────────────────────

#[test]
fn case_1_1_search_carries_ok_api_and_one_hash_per_row() {
    // "authority" appears in no fixture document, so that query would return no
    // row and prove nothing about a row. This is the golden's query instead.
    let vault = fixture_vault();
    index(vault.path());
    let report = search_json(
        vault.path(),
        &[
            "cache",
            "--intent",
            "history",
            "--limit",
            "3",
            "--no-embeddings",
        ],
    );
    assert_eq!(report["ok"], json!(true), "{report}");
    assert_eq!(report["api"], json!(1), "{report}");
    // The envelope adds `ok` and nothing else: these are the keys of the report
    // as `main` committed it, plus `ok`.
    assert_eq!(
        shape(&report),
        "alpha api corpus_id degraded index intent limit notes ok query results taken_ms vault"
    );
    // Rank 1 of this query is the redis decision. Its hash is stated here once,
    // by hand, from the committed file. The row shape itself is case 1.3.
    let top = &report["results"][0];
    assert_eq!(
        top["rel_path"], "decisions/2024-12-01-redis-cache.md",
        "{top}"
    );
    assert_eq!(
        top["content_hash"], "cb739381d8b33fad95a8b8251836619738547a3fd83b2972f5740a12c7b30ae6",
        "{top}"
    );
    for hit in report["results"].as_array().expect("results") {
        assert_hash_of_file(vault.path(), hit);
    }
}

#[test]
fn case_1_2_facts_and_lineage_carry_the_envelope() {
    // `facts` renders through the same report, so the two commands agree by
    // construction. The order is type authority, then date: one decision and two
    // specs, and the newest spec does not outrank the decision.
    let vault = fixture_vault();
    index(vault.path());
    let facts = facts_json(vault.path(), &["--no-embeddings"]);
    assert_eq!(facts["ok"], json!(true), "{facts}");
    assert_eq!(facts["api"], json!(1), "{facts}");
    assert_eq!(
        shape(&facts),
        "alpha api corpus_id degraded index limit notes ok results taken_ms vault"
    );
    assert_eq!(
        common::result_paths(&facts).join(" "),
        "decisions/2025-03-01-broken-link.md specs/body-starts-with-rule.md specs/crlf-spec.md"
    );
    for row in facts["results"].as_array().expect("results") {
        assert_hash_of_file(vault.path(), row);
    }
    // Every node of a live chain carries its own file's hash.
    let report = json_ok(
        vault.path(),
        &["lineage", "decisions/2024-12-01-redis-cache.md", "--json"],
    );
    assert_eq!(report["ok"], json!(true), "{report}");
    assert_eq!(report["api"], json!(1), "{report}");
    assert_eq!(
        shape(&report),
        "api corpus_id cycle_breaks notes ok path results taken_ms terminal vault"
    );
    for node in report["results"].as_array().expect("results") {
        let hash = node["content_hash"]
            .as_str()
            .expect("a live node has a hash");
        assert_eq!(hash.len(), 64, "{node}");
    }
}

#[test]
fn case_1_2_a_retired_lineage_node_has_a_null_hash() {
    // A retired document is a `TombstoneMeta` row, and that row stores no hash.
    // This phase does not read the file from disk, so the answer is `null`: no
    // invented value, and no fallback to a zero hash (M6 §2.7 rule 2).
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let first = "decisions/2020-01-01-adopt-postgres.md";
    let second = "decisions/2021-01-01-move-to-redis.md";
    write(root, "janus.toml", "corpus_id = \"tombstone\"\n");
    // The predecessor declares the edge, because a retired document declares
    // nothing: it is the survivor that keeps the tombstone in the chain.
    write(
        root,
        first,
        "---\ntitle: Adopt Postgres\ntype: decision\nstatus: superseded\ndate: 2020-01-01\n\
         superseded_by: decisions/2021-01-01-move-to-redis.md\n---\n\nPostgres holds the data.\n",
    );
    write(
        root,
        second,
        "---\ntitle: Move to Redis\ntype: decision\nstatus: active\ndate: 2021-01-01\n---\n\n\
         The cache moves out.\n",
    );
    index(root);
    fs::remove_file(root.join(second)).expect("remove the successor");
    index(root);

    let report = json_ok(root, &["lineage", first, "--json", "--no-embeddings"]);
    let rows = report["results"].as_array().expect("results");
    assert_eq!(rows.len(), 2, "{report}");
    assert_eq!(rows[0]["path"], json!(first), "{report}");
    // The successor is a tombstone: it keeps its row and offers no hash.
    assert_eq!(rows[1]["path"], json!(second), "{report}");
    assert!(rows[1]["content_hash"].is_null(), "{report}");
    // The live node carries the blake3 of its own file.
    let bytes = fs::read(root.join(first)).expect("read the live document");
    assert_eq!(
        rows[0]["content_hash"],
        json!(blake3::hash(&bytes).to_hex().to_string()),
        "{report}"
    );
}

#[test]
fn case_1_4_a_json_failure_prints_the_envelope_on_stdout() {
    let vault = fixture_vault();
    index(vault.path());
    // A usage error: understood, rejected before any work, exit 2. No hint
    // exists yet, so the field is absent.
    let (value, code) = failure(vault.path(), &["lineage", "--json", "--no-embeddings"]);
    assert_eq!(code, Some(2), "{value}");
    assert_eq!(shape(&value), "api code message ok", "{value}");
    assert_eq!(value["api"], json!(1), "{value}");
    assert_eq!(value["ok"], json!(false), "{value}");
    assert_eq!(value["code"], json!("usage"), "{value}");
    assert_eq!(
        value["message"],
        json!("lineage needs a PATH or --topic QUERY: give exactly one"),
        "{value}"
    );
    // Without `--json` the CLI contract is unchanged: one line on stderr and
    // nothing on stdout, so a script can tell "no report" from an empty one.
    let output = run(vault.path(), &["lineage", "--no-embeddings"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("error: "),
        "{output:?}"
    );
}

#[test]
fn case_1_5_a_failure_has_no_results_and_no_process_facts() {
    // A runtime failure in the matrix, on a vault that was never indexed: the
    // shape a reader might expect to carry `taken_ms` and `vault`. No `results`,
    // no `taken_ms`, no `vault`, and no `hint` until one exists.
    let vault = fixture_vault();
    let (value, code) = failure(
        vault.path(),
        &[
            "search",
            "cache",
            "--json",
            "--no-rebuild",
            "--no-embeddings",
        ],
    );
    assert_eq!(code, Some(1), "{value}");
    assert_eq!(value["code"], json!("index_missing"), "{value}");
    assert_eq!(shape(&value), "api code message ok", "{value}");
}

#[test]
fn case_1_6_a_report_outside_the_matrix_is_unchanged() {
    // `scan --json` is a receipt, not a tool answer: it gains no `ok`. Its
    // failure half is pinned by
    // `cli_scan::scan_json_errors_stay_on_stderr_with_empty_stdout`.
    let vault = fixture_vault();
    let report = json_ok(vault.path(), &["scan", "--json"]);
    assert_eq!(report["api"], json!(1), "{report}");
    assert!(
        !shape(&report).split(' ').any(|key| key == "ok"),
        "{report}"
    );
}

// ──────────────────────────────── the codes ────────────────────────────────

#[test]
fn case_2_1_scope_only_without_a_scope_is_usage() {
    let vault = fixture_vault();
    index(vault.path());
    let (value, code) = failure(
        vault.path(),
        &[
            "search",
            "probe",
            "--scope-only",
            "--json",
            "--no-embeddings",
        ],
    );
    assert_eq!(code, Some(2), "{value}");
    assert_eq!(value["code"], json!("usage"), "{value}");
}

#[test]
fn case_2_2_lineage_needs_exactly_one_of_path_and_topic() {
    let vault = fixture_vault();
    index(vault.path());
    let (neither, code) = failure(vault.path(), &["lineage", "--json", "--no-embeddings"]);
    assert_eq!(code, Some(2), "{neither}");
    assert_eq!(neither["code"], json!("usage"), "{neither}");
    let (both, code) = failure(
        vault.path(),
        &[
            "lineage",
            "decisions/2024-12-01-redis-cache.md",
            "--topic",
            "cache",
            "--json",
        ],
    );
    assert_eq!(code, Some(2), "{both}");
    assert_eq!(both["code"], json!("usage"), "{both}");
    assert_eq!(
        both["message"],
        json!("lineage takes a PATH or --topic QUERY, not both"),
        "{both}"
    );
}

#[test]
fn case_2_3_a_path_outside_the_corpus_is_a_refusal() {
    // The table row: `path_escape` is a refusal, so it exits 4.
    let escape = JanusError::PathEscape {
        path: PathBuf::from("../outside.md"),
    };
    let row = ApiCode::of(&escape);
    assert_eq!((row.code, row.exit), ("path_escape", 4));
    assert_eq!(row.exit, escape.exit_code());
    // The only tool of this phase that takes a path is `lineage`, and an
    // escaping argument is `not_found` there: no live and no retired document
    // matched it. The confinement refusal belongs to the document read of the
    // next phase (cases 3.2 and 3.3), so this test pins today's answer.
    let vault = fixture_vault();
    index(vault.path());
    let (value, code) = failure(
        vault.path(),
        &["lineage", "../outside.md", "--json", "--no-embeddings"],
    );
    assert_eq!(code, Some(1), "{value}");
    assert_eq!(value["code"], json!("not_found"), "{value}");
}

#[test]
fn case_2_4_a_format_mismatch_names_the_command_that_cures_it() {
    let vault = fixture_vault();
    index(vault.path());
    let path = vault.path().join(".janus").join("manifest.json");
    let text = fs::read_to_string(&path).expect("read the manifest");
    let mut manifest: Value = serde_json::from_str(&text).expect("the manifest is JSON");
    assert_eq!(manifest["format_version"], json!(6), "{manifest}");
    manifest["format_version"] = json!(999);
    fs::write(&path, serde_json::to_string(&manifest).expect("serialize")).expect("write it");
    // A read may not rebuild across a format boundary, so the answer is a code
    // and a message that names the command the caller must run.
    let (value, code) = failure(
        vault.path(),
        &["search", "cache", "--json", "--no-embeddings"],
    );
    assert_eq!(code, Some(1), "{value}");
    assert_eq!(value["code"], json!("index_needs_full_build"), "{value}");
    let message = value["message"].as_str().expect("message");
    assert!(
        message.contains("index needs a full build: run janus index"),
        "{message}"
    );
}

#[test]
fn case_2_5_a_failed_call_leaves_the_next_one_working() {
    // The server halves of this case arrive with the rpc loop and the MCP
    // server. On the CLI the promise is that a failure ends its own process
    // only: the next command answers from the same generation.
    let vault = fixture_vault();
    index(vault.path());
    let (_, code) = failure(vault.path(), &["lineage", "--json", "--no-embeddings"]);
    assert_eq!(code, Some(2));
    let report = search_json(
        vault.path(),
        &[
            "cache",
            "--intent",
            "history",
            "--limit",
            "1",
            "--no-embeddings",
        ],
    );
    assert_eq!(report["ok"], json!(true), "{report}");
    assert_eq!(
        report["results"].as_array().expect("results").len(),
        1,
        "{report}"
    );
}

#[test]
fn case_2_6_the_code_table_is_total_and_faithful() {
    // The §2.2 table, one row per `JanusError` variant, written out by hand: the
    // code an agent branches on, and the exit code the CLI uses. `not_found`
    // carries two variants on purpose.
    let p = |text: &str| PathBuf::from(text);
    let io = || std::io::Error::other("probe");
    let rows: Vec<(&str, u8, JanusError)> = vec![
        (
            "usage",
            2,
            JanusError::Usage {
                message: "probe".to_owned(),
            },
        ),
        (
            "config_exists",
            4,
            JanusError::ConfigExists {
                path: p("janus.toml"),
            },
        ),
        (
            "vault_not_directory",
            4,
            JanusError::VaultNotDirectory { path: p("v.md") },
        ),
        (
            "path_escape",
            4,
            JanusError::PathEscape {
                path: p("../out.md"),
            },
        ),
        (
            "config_missing",
            1,
            JanusError::ConfigMissing {
                path: p("janus.toml"),
            },
        ),
        (
            "config_read",
            1,
            JanusError::ConfigRead {
                path: p("janus.toml"),
                source: io(),
            },
        ),
        // The parser's own message, so the row is built from a real failure.
        (
            "config_parse",
            1,
            JanusError::ConfigParse {
                path: p("janus.toml"),
                source: toml::from_str::<toml::Value>("= 1").expect_err("invalid TOML"),
            },
        ),
        (
            "vault_resolve",
            1,
            JanusError::VaultResolve {
                path: p("v"),
                source: io(),
            },
        ),
        (
            "vault_not_found",
            1,
            JanusError::VaultNotFound { from: p(".") },
        ),
        (
            "path_resolve",
            1,
            JanusError::PathResolve {
                path: p("a.md"),
                source: io(),
            },
        ),
        (
            "not_found",
            1,
            JanusError::DocumentNotInCorpus { path: p("a.md") },
        ),
        (
            "not_found",
            1,
            JanusError::TopicNotFound {
                topic: "cache".to_owned(),
            },
        ),
        (
            "index_missing",
            1,
            JanusError::IndexMissing { dir: p(".janus") },
        ),
        (
            "index_unstable",
            1,
            JanusError::IndexUnstable {
                path: p(".janus/index.bin"),
            },
        ),
        (
            "index_needs_full_build",
            1,
            JanusError::IndexNeedsFullBuild {
                reason: "format_version 999 != 6".to_owned(),
            },
        ),
        (
            "busy",
            1,
            JanusError::LockTimeout {
                path: p(".janus/lock"),
            },
        ),
        (
            "unstable_corpus",
            1,
            JanusError::ChangedDuringRead { path: p("a.md") },
        ),
        (
            "write",
            1,
            JanusError::Write {
                path: p("a.md"),
                source: io(),
            },
        ),
        (
            "create_dir",
            1,
            JanusError::CreateDir {
                path: p(".janus"),
                source: io(),
            },
        ),
        ("io", 1, JanusError::Io(io())),
    ];
    for (code, exit, err) in &rows {
        let row = ApiCode::of(err);
        assert_eq!(row.code, *code, "{err}");
        assert_eq!(row.exit, *exit, "{err}");
        // The table copies `exit_code`; it does not invent an exit code.
        assert_eq!(row.exit, err.exit_code(), "{err}");
    }
    assert_eq!(rows.len(), 20, "one row per variant");
    // `refused` has no variant: an adapter asks for it (M6 §2.2).
    let refused = ApiCode::refused();
    assert_eq!((refused.code, refused.exit), ("refused", 4));
}

// ───────────────────────────── stdout discipline ───────────────────────────

#[test]
fn case_10_1_the_json_commands_keep_stderr_off_stdout() {
    // A config warning and a changed corpus: both are diagnostics, and both
    // belong on stderr. The corpus is edited before each command, so each one
    // refreshes and prints its own `refresh:` line.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write(
        root,
        "janus.toml",
        "[authority]\nweird = 0.0\n\n[embedding]\nmodel = \"hash-64\"\n",
    );
    let rel = "decisions/2020-01-01-cache.md";
    let text =
        "---\ntitle: Cache\ntype: decision\nstatus: active\ndate: 2020-01-01\n---\n\ncache\n";
    write(root, rel, text);
    index(root);
    for args in [
        vec!["search", "cache", "--json", "--no-embeddings"],
        vec!["facts", "--json", "--no-embeddings"],
        vec!["lineage", rel, "--json", "--no-embeddings"],
    ] {
        // One edit per command, so the read has something to refresh.
        fs::write(root.join(rel), format!("{text}cache\n")).expect("edit the document");
        let output = run(root, &args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            !stdout.contains("warning:") && !stdout.contains("refresh:"),
            "{args:?} wrote a diagnostic to stdout:\n{stdout}"
        );
        let report: Value = serde_json::from_str(&stdout).expect("stdout is one JSON object");
        assert_eq!(report["ok"], json!(true), "{report}");
        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
        assert!(stderr.contains("warning:"), "{args:?}: {stderr}");
        assert!(stderr.contains("refresh:"), "{args:?}: {stderr}");
    }
    // A read that declines to refresh keeps the same rule: the `refresh:` line
    // goes to stderr, and the object says `stale: true` instead.
    fs::write(root.join(rel), format!("{text}cache more\n")).expect("edit the document");
    let output = run(
        root,
        &[
            "search",
            "cache",
            "--json",
            "--no-rebuild",
            "--no-embeddings",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(!stdout.contains("refresh:"), "{stdout}");
    let report: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(report["index"]["stale"], json!(true), "{report}");
    assert_eq!(report["ok"], json!(true), "{report}");
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("refresh: skipped, --no-rebuild"),
        "{stderr}"
    );
}
