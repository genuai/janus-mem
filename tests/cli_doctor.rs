//! `janus doctor` end to end: cases 10.1–10.8 of M5 §3.10.
//!
//! Every vault is a temp copy. The committed fixtures are never written to, and
//! no test reads the wall clock. Each expected check below is derived by hand
//! from the fixture document named in the comment.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use common::{fixture_vault, graph_vault, run, run_ok};
use serde_json::Value;

/// Runs `doctor --json` and returns the process output and parsed report.
fn doctor(root: &Path) -> (std::process::Output, Value) {
    let output = run(root, &["doctor", "--json"]);
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (output, report)
}

/// Every check's `(code, path)`, in report order.
fn code_paths(report: &Value) -> Vec<(String, String)> {
    report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| {
            (
                check["code"].as_str().expect("code").to_owned(),
                check["path"].as_str().expect("path").to_owned(),
            )
        })
        .collect()
}

/// Every check's code, in report order.
fn codes(report: &Value) -> Vec<String> {
    code_paths(report)
        .into_iter()
        .map(|(code, _)| code)
        .collect()
}

/// The check whose code and path match, expecting exactly one.
fn find<'a>(report: &'a Value, code: &str, path: &str) -> &'a Value {
    let mut found = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .filter(|check| check["code"] == code && check["path"] == path);
    let check = found
        .next()
        .unwrap_or_else(|| panic!("no {code} for {path}"));
    assert!(found.next().is_none(), "more than one {code} for {path}");
    check
}

/// Writes a config and one Markdown file per entry into a temp vault.
fn vault(docs: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"doctor\"\n",
    )
    .expect("write config");
    for (rel_path, text) in docs {
        write(dir.path(), rel_path, text);
    }
    dir
}

/// Writes one document, creating its directories.
fn write(root: &Path, rel_path: &str, text: &str) {
    let path = root.join(rel_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("directory");
    }
    fs::write(path, text).expect("write document");
}

/// Builds a lexical index, which is the generation the graph half reads.
fn index(root: &Path) {
    run_ok(root, &["index", "--no-embeddings"]);
}

/// Every file under `root`, with its bytes and mtime, keyed by `/` path.
fn tree(root: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<String, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(dir).expect("read directory") {
            let entry = entry.expect("directory entry");
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .expect("path under root")
                .to_string_lossy()
                .replace('\\', "/");
            if entry.file_type().expect("file type").is_dir() {
                collect(root, &path, out);
            } else {
                let bytes = fs::read(&path).expect("read file");
                let modified = entry
                    .metadata()
                    .expect("metadata")
                    .modified()
                    .expect("mtime");
                out.insert(rel, (bytes, modified));
            }
        }
    }

    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

// ────────────────────────────── cases 10.1–10.3 ─────────────────────────────

#[test]
fn case_10_1_the_main_fixture_reports_the_three_graph_errors() {
    // The cycle is postgres -> redis -> revisited -> postgres. The breaker drops
    // `(revisited, redis)`, so redis is replaced by postgres and its window ends
    // at postgres's 2024-11-02, before redis starts on 2024-12-01: one
    // `invalid_validity`. `broken-link.md` names a file that was never written:
    // one `dangling_link`. The dropped edge is one `supersession_cycle`.
    let vault = fixture_vault();
    index(vault.path());
    let (output, report) = doctor(vault.path());

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(report["ok"], false, "{report}");
    let found = codes(&report);
    for code in ["dangling_link", "supersession_cycle", "invalid_validity"] {
        assert!(
            found.iter().any(|item| item == code),
            "{code} missing: {found:?}"
        );
    }
    find(
        &report,
        "invalid_validity",
        "decisions/2024-12-01-redis-cache.md",
    );
    find(
        &report,
        "dangling_link",
        "decisions/2025-03-01-broken-link.md",
    );
    assert!(
        report["summary"]["errors"].as_u64().unwrap_or(0) >= 2,
        "{report}"
    );
}

#[test]
fn case_10_2_the_clean_graph_subset_is_ok() {
    // The clean subset removes every seeded defect: `bad/` and the one inert
    // scope pair, `scopes/acme-vs-beta.md` -> `scopes/beta-claim.md` (M5-41).
    // What remains is the five-link chain, the two effective scope pairs and the
    // near-identical pair. None has a defect, so the report is empty, ok is true
    // and the exit code is 0.
    let vault = graph_vault();
    let root = vault.path();
    fs::remove_dir_all(root.join("bad")).expect("remove bad/");
    fs::remove_file(root.join("scopes/acme-vs-beta.md")).expect("remove the inert edge");
    index(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["summary"]["errors"], 0, "{report}");
    assert_eq!(report["summary"]["warnings"], 0, "{report}");
    assert_eq!(report["checks"], serde_json::json!([]), "{report}");
}

#[test]
fn case_10_3_every_seeded_defect_gets_one_check() {
    // Hand-derived from the committed graph fixture:
    //   scopes/beta-claim.md     replaced by acme-vs-beta across disjoint scopes
    //   bad/dangling.md          supersedes bad/never-written.md
    //   bad/invalid-window.md    2021-01-01 .. 2020-01-01 (declared end)
    //   bad/one-sided.md         superseded_by the target, no answer back
    //   bad/status-lies.md       same missing answer
    //   bad/status-lies.md       declared active, effective superseded
    // `bad/expired.md` is valid [2019-01-01, 2020-01-01), so it is expired but
    // not invalid. Seven checks: three errors and three warnings.
    let vault = graph_vault();
    index(vault.path());
    let (output, report) = doctor(vault.path());

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(report["summary"]["errors"], 3, "{report}");
    assert_eq!(report["summary"]["warnings"], 3, "{report}");
    assert_eq!(
        code_paths(&report),
        [
            (
                "cross_scope_supersession".to_owned(),
                "scopes/beta-claim.md".to_owned()
            ),
            ("dangling_link".to_owned(), "bad/dangling.md".to_owned()),
            (
                "invalid_validity".to_owned(),
                "bad/invalid-window.md".to_owned()
            ),
            (
                "one_sided_supersession".to_owned(),
                "bad/one-sided.md".to_owned()
            ),
            (
                "one_sided_supersession".to_owned(),
                "bad/status-lies.md".to_owned()
            ),
            (
                "status_mismatch".to_owned(),
                "bad/status-lies.md".to_owned()
            ),
        ]
    );
}

#[test]
fn a_foreign_corpus_qualifier_is_reported_as_cross_corpus() {
    // The link names `other:`, so resolution stops at the qualifier and never
    // guesses a path. The row is `cross_corpus_link_unsupported`, not the
    // `dangling_link` a missing path would produce.
    let vault = vault(&[(
        "notes/a.md",
        "---
title: A
date: 2021-01-01
supersedes: other:notes/b.md
---
# A
",
    )]);
    let root = vault.path();
    index(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        codes(&report),
        ["cross_corpus_link_unsupported"],
        "{report}"
    );
    assert_eq!(report["checks"][0]["severity"], "error");
}

// ────────────────────────────── cases 10.4–10.6 ─────────────────────────────

#[test]
fn case_10_4_no_index_is_one_warning_and_writes_nothing() {
    // A clean one-document vault with no `.janus/`: the scan half finds nothing,
    // the missing generation is one warning, and the command creates nothing.
    let vault = vault(&[(
        "notes/a.md",
        "---\ntitle: A\ntype: note\ndate: 2021-01-01\n---\n# A\n",
    )]);
    let root = vault.path();
    let before = tree(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["summary"]["errors"], 0, "{report}");
    assert_eq!(report["summary"]["warnings"], 1, "{report}");
    assert_eq!(codes(&report), ["index_absent"]);
    assert!(!root.join(".janus").exists(), "doctor created .janus/");
    assert_eq!(tree(root), before, "doctor changed the vault");
}

#[test]
fn case_10_5_one_unknown_type_exits_zero() {
    // `speculative` is not in the registry, so the document keeps its name and
    // gets one `unknown_type` warning. A warning never changes the exit code.
    // The index is built first, so `index_absent` is not a second warning.
    let vault = vault(&[(
        "notes/a.md",
        "---\ntitle: A\ntype: speculative\ndate: 2021-01-01\n---\n# A\n",
    )]);
    let root = vault.path();
    index(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["summary"]["errors"], 0, "{report}");
    assert_eq!(report["summary"]["warnings"], 1, "{report}");
    assert_eq!(codes(&report), ["unknown_type"]);
    assert_eq!(report["checks"][0]["path"], "notes/a.md", "{report}");
}

#[test]
fn case_10_6_duplicate_identity_is_an_error_that_names_both_paths() {
    // Two documents with the same title and date carry the same rung-3 key. The
    // note is promoted to an error, and the one row names both paths. The index
    // is built first to prove the check survives a published generation (M5-44).
    let vault = vault(&[
        (
            "notes/a.md",
            "---\ntitle: Same\ndate: 2021-01-01\n---\n# A\n",
        ),
        (
            "notes/b.md",
            "---\ntitle: Same\ndate: 2021-01-01\n---\n# B\n",
        ),
    ]);
    let root = vault.path();
    index(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(report["ok"], false, "{report}");
    assert_eq!(report["summary"]["errors"], 1, "{report}");
    assert_eq!(codes(&report), ["duplicate_identity"], "{report}");
    let check = &report["checks"][0];
    assert_eq!(check["severity"], "error", "{check}");
    let message = check["message"].as_str().expect("message");
    assert!(message.contains("notes/a.md"), "{message}");
    assert!(message.contains("notes/b.md"), "{message}");
}

// ────────────────────────────── cases 10.7–10.8 ─────────────────────────────

#[test]
fn case_10_7_two_runs_are_byte_identical_and_write_nothing() {
    // The report carries no elapsed-time field, so two runs must print the same
    // bytes. The tree comparison covers both the vault files and `.janus/`.
    let vault = graph_vault();
    index(vault.path());
    let before = tree(vault.path());

    let first = run(vault.path(), &["doctor", "--json"]);
    let second = run(vault.path(), &["doctor", "--json"]);

    assert_eq!(first.status.code(), Some(1), "{first:?}");
    assert_eq!(first.stdout, second.stdout, "two runs differ");
    assert_eq!(tree(vault.path()), before, "doctor changed the vault");
}

#[test]
fn case_10_8_status_lies_is_one_warning_and_exit_zero_when_alone() {
    // The committed `status-lies.md` also has a one-sided warning, so it is
    // never the only finding. This vault keeps the same lie and gives the
    // target a later date and a reciprocal `supersedes` (M5-42, M5-43):
    // declared active, effective superseded, so the only finding is the status
    // warning.
    let vault = vault(&[
        (
            "bad/status-lies.md",
            "---\ntitle: Status lies\ntype: decision\nstatus: active\ndate: 2021-02-01\nsuperseded_by: bad/newer.md\n---\n# Status lies\n",
        ),
        (
            "bad/newer.md",
            "---\ntitle: Newer\ntype: decision\nstatus: active\ndate: 2022-01-01\nsupersedes: bad/status-lies.md\n---\n# Newer\n",
        ),
    ]);
    let root = vault.path();
    index(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["summary"]["errors"], 0, "{report}");
    assert_eq!(report["summary"]["warnings"], 1, "{report}");
    assert_eq!(codes(&report), ["status_mismatch"]);
    assert_eq!(report["checks"][0]["path"], "bad/status-lies.md");
    assert_eq!(report["checks"][0]["severity"], "warning");
}

// ─────────────────────────────── staleness ──────────────────────────────────

#[test]
fn index_stale_is_a_warning_after_an_edit() {
    // Edit a clean graph vault after the build. The scan half still parses, the
    // graph half describes the last build, and `index_stale` says so. The
    // warning alone keeps the exit code at 0.
    let vault = graph_vault();
    let root = vault.path();
    fs::remove_dir_all(root.join("bad")).expect("remove bad/");
    fs::remove_file(root.join("scopes/acme-vs-beta.md")).expect("remove the inert edge");
    index(root);
    write(
        root,
        "similar/alpha.md",
        "---\ntitle: Cache warmup alpha\ntype: finding\nstatus: active\ndate: 2021-06-01\n---\n# Edited\n",
    );
    let before = tree(root);

    let (output, report) = doctor(root);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(codes(&report), ["index_stale"], "{report}");
    assert_eq!(report["checks"][0]["severity"], "warning", "{report}");
    assert_eq!(tree(root), before, "doctor changed the vault");
}
