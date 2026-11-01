//! `janus chunks` end-to-end: the report, path selection, exit codes, and the
//! promise that the command is a read. The chunk *rules* live in
//! `tests/chunk_split.rs` and the chunker's unit tests; this file asserts the
//! command surface only (§9.1, §9.4, §8.5).

mod common;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use common::{LARGE_REL, NON_UTF8_REL, chunks_json, fixture_vault, janus};
use predicates::prelude::*;
use serde_json::Value;

/// A vault with one document whose only block cannot be split, so chunking has
/// something to note.
fn note_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\n\n[chunking]\nmax_chars = 80\noverlap = 0.0\n",
    )
    .unwrap();
    let fence = format!(
        "# Fenced\n\n```text\n{}\n```\n",
        "an indivisible line of code ".repeat(10)
    );
    fs::write(root.join("fenced.md"), fence).unwrap();
    dir
}

/// Runs `janus --vault <root> chunks` with extra arguments.
fn chunks(root: &Path, extra: &[&str]) -> Command {
    let mut command = janus();
    command.arg("--vault").arg(root).arg("chunks");
    command.args(extra);
    command
}

// ───────────────────────────────── the report ──────────────────────────────

#[test]
fn chunks_reports_every_document_with_its_chunks() {
    let vault = fixture_vault();

    chunks(vault.path(), &[])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "decisions/2024-11-02-postgres-cache.md",
        ))
        .stdout(predicate::str::contains("notes/undated.md"))
        .stdout(predicate::str::contains("chunking: max_chars = 1800"))
        // The excluded, hidden and ignored trees stay out, exactly as for `scan`.
        .stdout(predicate::str::contains("archive/private/secret.md").not())
        .stdout(predicate::str::contains(".scratch.md").not())
        .stdout(predicate::str::contains("node_modules").not());
}

#[test]
fn chunks_json_is_a_versioned_metadata_only_report() {
    let vault = fixture_vault();
    let report = chunks_json(vault.path(), &[]);

    assert_eq!(report["api"], 1);
    assert_eq!(report["corpus_id"], "fixtures");
    assert_eq!(report["chunking"]["max_chars"], 1800);
    assert_eq!(report["chunking"]["overlap"], 0.15);

    let docs = report["docs"].as_array().expect("docs");
    assert!(!docs.is_empty());

    let mut total = 0;
    for doc in docs {
        assert_eq!(doc["id"].as_str().expect("doc id").len(), 32);
        assert!(doc["rel_path"].is_string());
        assert!(doc["title"].is_string());
        assert!(doc["notes"].is_array());
        // No body, no frontmatter, no timestamps (§8.5, §13).
        assert!(doc.get("body").is_none(), "{doc}");
        assert!(doc.get("path").is_none(), "{doc}");
        assert!(doc.get("mtime").is_none(), "{doc}");

        let chunks = doc["chunks"].as_array().expect("chunks");
        for (ordinal, chunk) in chunks.iter().enumerate() {
            total += 1;
            assert_eq!(chunk["id"].as_str().expect("chunk id").len(), 64);
            assert_eq!(chunk["ordinal"], ordinal as u64);
            assert!(chunk["start"].as_u64().expect("start") <= chunk["end"].as_u64().expect("end"));
            assert!(chunk["chars"].is_u64());
            assert!(chunk["heading_path"].is_array());
            // `--text` was not passed, so no content may appear.
            assert!(chunk.get("text").is_none(), "{chunk}");
        }
    }

    assert_eq!(report["total_chunks"], total);
    assert!(total > docs.len(), "at least one document splits");
}

#[test]
fn chunks_text_is_the_only_way_content_appears() {
    let vault = fixture_vault();
    // A sentence that exists only in a document body, never in frontmatter.
    const BODY: &str = "Everything else in this fixture is ugly";

    let hidden = chunks_json(vault.path(), &[]);
    assert!(!hidden.to_string().contains(BODY), "{hidden}");

    chunks(vault.path(), &[])
        .assert()
        .success()
        .stdout(predicate::str::contains(BODY).not());

    let shown = chunks_json(vault.path(), &["--text"]);
    assert!(shown.to_string().contains(BODY), "{}", shown["docs"][0]);

    chunks(vault.path(), &["--text", "2025-01-05-corpus-notes.md"])
        .assert()
        .success()
        .stdout(predicate::str::contains(BODY));
}

#[test]
fn chunks_surfaces_a_chunk_note_on_the_document() {
    let vault = note_vault();
    let report = chunks_json(vault.path(), &[]);
    let doc = &report["docs"][0];

    // The fence is one indivisible chunk larger than the limit: the note says so
    // instead of the splitter cutting it (CH-6, CH-13).
    let codes: Vec<&str> = doc["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|note| note["code"].as_str().expect("code"))
        .collect();
    assert_eq!(codes, ["oversized_block"]);

    // Kept whole rather than cut: exactly one chunk is longer than the limit.
    let oversized = doc["chunks"]
        .as_array()
        .expect("chunks")
        .iter()
        .filter(|chunk| chunk["chars"].as_u64().is_some_and(|chars| chars > 80))
        .count();
    assert_eq!(oversized, 1, "{doc}");

    chunks(vault.path(), &[])
        .assert()
        .success()
        .stdout(predicate::str::contains("! oversized_block:"));
}

#[test]
fn chunks_json_is_deterministic_between_runs() {
    let vault = fixture_vault();

    // Chunk ids are content-addressed and the DTO carries no clock, so unlike
    // `scan --json` there is nothing to redact: the two runs must be identical.
    assert_eq!(
        chunks_json(vault.path(), &["--limit", "2"]),
        chunks_json(vault.path(), &["--limit", "2"])
    );
}

// ─────────────────────────────── path selection ────────────────────────────

#[test]
fn a_path_argument_selects_exactly_one_document() {
    let vault = fixture_vault();
    let path = vault
        .path()
        .join("decisions")
        .join("2024-11-02-postgres-cache.md");

    let report = chunks_json(vault.path(), &[path.to_str().unwrap()]);

    let docs = report["docs"].as_array().expect("docs");
    assert_eq!(docs.len(), 1, "{report}");
    assert_eq!(
        docs[0]["rel_path"],
        "decisions/2024-11-02-postgres-cache.md"
    );
}

#[test]
fn a_corpus_relative_path_works_from_any_directory() {
    let vault = fixture_vault();
    let nested = vault.path().join("decisions");

    // `rel_path` is what a report hands a caller: naming it must not depend on
    // the working directory.
    janus()
        .current_dir(&nested)
        .arg("--vault")
        .arg(vault.path())
        .arg("chunks")
        .arg("notes/undated.md")
        .assert()
        .success()
        .stdout(predicate::str::contains("notes/undated.md"))
        .stdout(predicate::str::contains("decisions/2024-11-02-postgres-cache.md").not());
}

#[test]
fn two_path_arguments_are_reported_in_rel_path_order() {
    let vault = fixture_vault();
    let a = vault.path().join("notes").join("undated.md");
    let b = vault
        .path()
        .join("decisions")
        .join("2024-11-02-postgres-cache.md");

    let report = chunks_json(vault.path(), &[a.to_str().unwrap(), b.to_str().unwrap()]);
    let paths: Vec<&str> = report["docs"]
        .as_array()
        .expect("docs")
        .iter()
        .map(|doc| doc["rel_path"].as_str().expect("rel_path"))
        .collect();

    assert_eq!(
        paths,
        ["decisions/2024-11-02-postgres-cache.md", "notes/undated.md"]
    );
}

#[test]
fn a_path_outside_the_vault_is_refused() {
    let vault = fixture_vault();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("elsewhere.md");
    fs::write(&file, "# elsewhere\n").unwrap();

    chunks(vault.path(), &[file.to_str().unwrap()])
        .assert()
        .code(4)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("refusing path outside the corpus"));
}

#[test]
fn a_path_that_is_not_a_document_is_a_named_error() {
    let vault = fixture_vault();

    // Absent.
    chunks(vault.path(), &["notes/nope.md"])
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("notes/nope.md"));

    // Present but excluded by `[index] exclude`: a different answer from an
    // escape, because the path is legal.
    chunks(vault.path(), &["archive/private/secret.md"])
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("archive/private/secret.md"))
        .stderr(predicate::str::contains("no document at"));
}

#[test]
fn chunks_rejects_an_unknown_flag_with_the_usage_exit_code() {
    let vault = fixture_vault();

    chunks(vault.path(), &["--nonsense"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
}

// ────────────────────────────────── limits ─────────────────────────────────

#[test]
fn limit_truncates_per_document_and_says_so() {
    let vault = fixture_vault();

    let full = chunks_json(vault.path(), &[]);
    let capped = chunks_json(vault.path(), &["--limit", "1"]);

    let docs = capped["docs"].as_array().expect("docs");
    assert!(docs.iter().all(|doc| {
        let len = doc["chunks"].as_array().expect("chunks").len();
        len <= 1 && len > 0
    }));

    // The 2 MB document has many chunks, so it is the one that must be flagged;
    // a one-chunk document must not grow a `truncated` key at all.
    let large = docs
        .iter()
        .find(|doc| doc["rel_path"] == LARGE_REL)
        .expect("the generated large document");
    assert_eq!(large["truncated"], true);
    assert_eq!(large["chunks"].as_array().expect("chunks").len(), 1);

    let small = docs
        .iter()
        .find(|doc| doc["rel_path"] == "notes/undated.md")
        .expect("a single-chunk document");
    assert!(small.get("truncated").is_none(), "{small}");

    assert_eq!(capped["total_chunks"], docs.len());
    assert!(
        full["total_chunks"].as_u64().expect("total")
            > capped["total_chunks"].as_u64().expect("total")
    );
}

// ──────────────────────────────── invariants ───────────────────────────────

#[test]
fn chunks_reports_a_non_utf8_file_as_a_read_skip_and_never_chunks_it() {
    let vault = fixture_vault();
    let report = chunks_json(vault.path(), &[]);

    let skips = report["read_skips"].as_array().expect("read_skips");
    assert_eq!(skips.len(), 1, "{report}");
    assert_eq!(skips[0]["rel_path"], NON_UTF8_REL);
    assert_eq!(skips[0]["reason"], "non_utf8_content");

    assert!(
        report["docs"]
            .as_array()
            .expect("docs")
            .iter()
            .all(|doc| doc["rel_path"] != NON_UTF8_REL)
    );
}

#[test]
fn chunks_never_writes_to_the_vault() {
    let vault = fixture_vault();
    let before = tree(vault.path());

    chunks(vault.path(), &["--text"]).assert().success();

    assert_eq!(
        before,
        tree(vault.path()),
        "chunks must not mutate the vault"
    );
}

/// A cheap recursive listing: every path plus its length.
fn tree(root: &Path) -> Vec<(String, u64)> {
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

/// Guards the shape the golden snapshot depends on: `docs` must be an array even
/// for a report with no documents at all.
#[test]
fn an_empty_corpus_produces_an_empty_but_well_formed_report() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("janus.toml"), "index_dir = \".janus\"\n").unwrap();

    let report: Value = chunks_json(dir.path(), &[]);
    assert_eq!(report["docs"], Value::Array(Vec::new()));
    assert_eq!(report["total_chunks"], 0);
    assert_eq!(report["read_skips"], Value::Array(Vec::new()));

    chunks(dir.path(), &[])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 chunk(s) in 0 document(s)"));
}
