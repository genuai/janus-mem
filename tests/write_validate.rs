//! The value check every write runs (§2.4, cases 4.1 to 4.7).
//!
//! A refusal is proved twice: the failure object a caller branches on, and the
//! bytes on disk. Case 4.3 asks `parse_date` rather than repeating its answer.
//! Every corpus is a temporary directory; no case sleeps or reads the clock.

mod common;

use std::fs;
use std::path::Path;
use std::process::Output;

use janus_mem::config::Config;
use janus_mem::vault::frontmatter;
use janus_mem::write;
use serde_json::{Value, json};

/// The document the patch and whole-file cases name.
const ONE: &str = "---\ntitle: One\nstatus: active\ndate: 2026-10-29\neffective_to: 2026-12-01\n---\n\n# One\n\nprose\n";

/// The corpus config: a write needs no index (M7-17).
const CONFIG: &str = "index_dir=\".janus\"\ncorpus_id=\"validate\"\n[embedding]\nenabled=false\n";

/// The forms a write is asked to store. `parse_date` decides each one.
const DATE_FORMS: [&str; 9] = [
    "2026-10-29",
    "2026-10-29T10:00:00Z",
    "2026-10-29 10:00:00",
    " 2026-10-29 ",
    "2026-02-30",
    "2026-10-29junk",
    "2026-11",
    "2026",
    "not-a-date",
];

// ──────────────────────────────── helpers ──────────────────────────────────

/// A corpus with one document, an archive, and the workspace between them.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let (primary, archive) = (dir.path().join("primary"), dir.path().join("archive"));
    for path in [
        primary.join("notes"),
        primary.join("decisions"),
        archive.join("decisions"),
    ] {
        fs::create_dir_all(path).expect("create the corpus");
    }
    fs::write(primary.join("janus.toml"), CONFIG).expect("the config");
    fs::write(
        archive.join("janus.toml"),
        CONFIG.replace("validate", "archive"),
    )
    .expect("config");
    fs::write(primary.join("notes/one.md"), ONE).expect("the document");
    fs::write(
        archive.join("decisions/postgres.md"),
        "---\ntitle: Adopt Postgres\ntype: decision\nstatus: active\ndate: 2020-01-01\n---\n\nUse Postgres.\n",
    )
    .expect("the source");
    fs::write(
        dir.path().join("workspace.toml"),
        "[[corpora]]\nid=\"main\"\nroot=\"primary\"\nrole=\"primary\"\n\
         [[corpora]]\nid=\"context\"\nroot=\"archive\"\nrole=\"archived\"\n",
    )
    .expect("the workspace");
    fs::write(dir.path().join("body.md"), "# Two\n\nwritten\n").expect("the body");
    dir
}

/// One document of the primary corpus, as text.
fn text(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join("primary").join(rel)).expect("read the document")
}

/// One document of the primary corpus, as bytes.
fn bytes(root: &Path, rel: &str) -> Vec<u8> {
    fs::read(root.join("primary").join(rel)).expect("read the document")
}

/// `janus` over one corpus root, or over the workspace, with no ambient config.
fn janus(root: &Path, file: &str, flag: &str, args: &[&str]) -> Output {
    common::janus()
        .current_dir(root)
        .env("RUST_LOG", "off")
        .env_remove("JANUS_WORKSPACE")
        .env_remove("JANUS_VAULT")
        .arg(flag)
        .arg(root.join(file))
        .args(args)
        .output()
        .expect("run janus")
}

/// `janus` over the primary corpus alone.
fn corpus(root: &Path, args: &[&str]) -> Output {
    janus(root, "primary", "--vault", args)
}

/// `janus` over the workspace, which is how `promote` names two corpora.
fn workspace(root: &Path, args: &[&str]) -> Output {
    janus(root, "workspace.toml", "--workspace", args)
}

/// One `set` request on `notes/one.md`, against the hash it holds now.
fn set(root: &Path, key: &str) -> Output {
    let hash = etag(root, "notes/one.md");
    let args = [
        "set",
        "notes/one.md",
        "--expect-hash",
        &hash,
        "--key",
        key,
        "--json",
    ];
    corpus(root, &args)
}

/// The etag a read reports for one document of the primary corpus.
fn etag(root: &Path, rel: &str) -> String {
    let read: Value = serde_json::from_slice(&corpus(root, &["read", rel, "--json"]).stdout)
        .expect("the read is JSON");
    read["content_hash"]
        .as_str()
        .expect("content_hash")
        .to_owned()
}

/// Asserts a command succeeded.
fn ok(output: &Output) {
    assert!(
        output.status.success(),
        "janus failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The message of a refused write: exit 4, `invalid_value`, `api` 1.
fn refusal(output: &Output) -> String {
    assert_eq!(output.status.code(), Some(4), "a refused value exits 4");
    let failure: Value = serde_json::from_slice(&output.stdout).expect("the failure object");
    assert_eq!(failure["api"], json!(1));
    assert_eq!(failure["code"], json!("invalid_value"));
    failure["message"].as_str().expect("the message").to_owned()
}

// ─────────────────────────── the refused values ────────────────────────────

/// Cases 4.1 and 4.2 — a nonsense date and a nonsense status are refused, exit
/// 4, and the file keeps its bytes. Deleting the call from `plan` fails this.
#[test]
fn case_4_1_and_4_2_a_nonsense_date_and_status_are_refused() {
    let dir = scratch();
    let root = dir.path();
    let cases = [
        (
            "date=not-a-date",
            "`date`",
            "YYYY-MM-DD, an ISO-8601 timestamp, or null",
        ),
        (
            "status=nonsense",
            "`status`",
            "active, draft, superseded, deprecated, unknown, null",
        ),
    ];

    for (key, name, accepted) in cases {
        let before = bytes(root, "notes/one.md");
        let message = refusal(&set(root, key));
        let value = key
            .trim_start_matches("date=")
            .trim_start_matches("status=");
        assert!(message.contains(name), "{message}");
        assert!(message.contains(value), "{message}");
        assert!(message.contains(accepted), "{message}");
        assert_eq!(bytes(root, "notes/one.md"), before, "{key} moved the file");
    }
    // The five status names are compared case-insensitively.
    ok(&set(root, "status=Active"));
}

/// Case 4.3 — every form `parse_date` accepts, the write accepts; one it
/// rejects, the write rejects. The parser decides both sides.
#[test]
fn case_4_3_the_write_accepts_exactly_the_dates_the_parser_accepts() {
    let dir = scratch();
    let root = dir.path();
    let config = Config::load(&root.join("primary")).expect("the corpus config");
    let (mut accepted, mut refused) = (0, 0);

    for form in DATE_FORMS {
        let parsed = frontmatter::parse_date(form).is_some();
        for key in ["date", "effective_from", "effective_to"] {
            let stored = write::validate(&config, key, Some(form)).is_ok();
            assert_eq!(parsed, stored, "{key} = {form:?} disagrees with the parser");
        }
        if parsed { accepted += 1 } else { refused += 1 }
    }
    assert!(
        accepted >= 3 && refused >= 3,
        "{accepted} accepted, {refused} refused"
    );

    // End to end: a timestamp is stored, and a month the parser refuses is not.
    ok(&set(root, "date=2026-11-01T10:00:00Z"));
    assert!(text(root, "notes/one.md").contains("2026-11-01T10:00:00Z"));
    assert!(refusal(&set(root, "date=2026-11")).contains("2026-11"));
}

/// Case 4.4 — `null` is a value, not an absence, so a request for it is stored.
#[test]
fn case_4_4_a_null_is_accepted_on_every_checked_key() {
    let dir = scratch();
    let root = dir.path();
    let config = Config::load(&root.join("primary")).expect("the corpus config");
    for key in ["status", "date", "effective_from", "effective_to"] {
        assert!(write::validate(&config, key, None).is_ok(), "{key} = null");
    }
    ok(&set(root, "effective_to"));
    assert!(text(root, "notes/one.md").contains("effective_to: null\n"));
}

/// Case 4.5 — the type registry stays open: an unknown name is stored, the
/// document keeps it, and `doctor` reports it.
#[test]
fn case_4_5_an_unknown_type_is_stored_and_doctor_reports_it() {
    let dir = scratch();
    let root = dir.path();
    ok(&set(root, "type=nonsense"));
    assert!(text(root, "notes/one.md").contains("type: nonsense\n"));

    let report: Value = serde_json::from_slice(&corpus(root, &["doctor", "--json"]).stdout)
        .expect("the report is JSON");
    let reported = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .any(|check| {
            check["code"] == json!("unknown_type") && check["path"] == json!("notes/one.md")
        });
    assert!(reported, "doctor reports the unknown type:\n{report}");
}

/// Case 4.6 — a key the config does not interpret is stored unchanged.
#[test]
fn case_4_6_a_key_the_config_ignores_is_stored_as_written() {
    let dir = scratch();
    let root = dir.path();
    ok(&set(root, "owner=café"));
    assert!(text(root, "notes/one.md").contains("owner: \"café\"\n"));
}

// ─────────────────────────── where the check runs ──────────────────────────

/// Case 4.7 — the two surfaces that take a free key refuse it end to end:
/// `set` and `write`. Deleting the call from `plan_document` fails the second.
#[test]
fn case_4_7_set_and_write_refuse_a_value_the_reader_cannot_interpret() {
    let dir = scratch();
    let root = dir.path();
    let before = bytes(root, "notes/one.md");
    assert!(refusal(&set(root, "status=nonsense")).contains("nonsense"));
    assert_eq!(bytes(root, "notes/one.md"), before);

    let path = root.join("fm.yaml");
    fs::write(&path, "title: Two\nstatus: nonsense\n").expect("the frontmatter");
    let (frontmatter, body) = (path.display().to_string(), root.join("body.md"));
    let (body, hash) = (body.display().to_string(), etag(root, "notes/one.md"));

    let overwritten = corpus(
        root,
        &[
            "write",
            "notes/one.md",
            "--frontmatter",
            &frontmatter,
            "--body",
            &body,
            "--overwrite",
            "--expect-hash",
            &hash,
            "--json",
        ],
    );
    assert!(refusal(&overwritten).contains("status"));
    assert_eq!(bytes(root, "notes/one.md"), before);
}

/// Case 4.7 — the four surfaces that build their own keys inherit the check,
/// and still write a document that already holds a refused value. Neither takes
/// a key from the caller, so the request is always valid: a write validates the
/// request, never the bytes.
#[test]
fn case_4_7_supersede_and_promote_keep_working_over_a_refused_value() {
    let dir = scratch();
    let root = dir.path();
    fs::write(
        root.join("primary/notes/one.md"),
        ONE.replace("2026-10-29", "not-a-date"),
    )
    .expect("the document");

    let hash = etag(root, "notes/one.md");
    let args = [
        "supersede",
        "notes/one.md",
        "notes/two.md",
        "--expect-hash",
        &hash,
        "--json",
    ];
    ok(&corpus(root, &args));
    let old = text(root, "notes/one.md");
    assert!(old.contains("status: superseded\n"), "{old}");
    assert!(
        old.contains("date: not-a-date\n"),
        "the bytes are untouched: {old}"
    );
    assert!(text(root, "notes/two.md").contains("supersedes: notes/one.md\n"));

    let (hash, rel) = (etag(root, "notes/one.md"), "context:decisions/postgres.md");
    let args = [
        "promote",
        rel,
        "--link-from",
        "notes/one.md",
        "--expect-hash",
        &hash,
        "--json",
    ];
    ok(&workspace(root, &args));
    assert!(text(root, "notes/one.md").contains("supersedes: context:decisions/postgres.md\n"));
    ok(&workspace(root, &["promote", rel, "--adopt", "--json"]));
    assert!(text(root, "decisions/postgres.md").contains("context:decisions/postgres.md"));
}
