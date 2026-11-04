//! `janus search` end to end: the §3.1 result contract, JSON output, staleness
//! and refusal.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::{fixture_vault, index, result_paths, run, run_ok, search_json};
use serde_json::Value;

/// Every file in `dir`, so "wrote nothing" can be asserted byte-for-byte.
fn dir_snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("the index directory exists") {
        let entry = entry.expect("index directory entry");
        files.insert(
            entry.file_name().to_string_lossy().into_owned(),
            fs::read(entry.path()).expect("read an index directory file"),
        );
    }
    files
}

/// A vault where one document owns many chunk scores and nine others match.
fn contract_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"contract\"\n\n[chunking]\nmax_chars = 120\noverlap = 0.0\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();

    let mut body = String::from("---\ntitle: Dominant\ndate: 2025-03-01\n---\n\n# Dominant\n\n");
    for _ in 0..20 {
        body.push_str("alpha beta gamma delta. alpha beta gamma delta.\n\n");
    }
    fs::write(root.join("notes/dominant.md"), body).unwrap();

    for i in 0..9 {
        let text = format!(
            "---\ntitle: Other {i}\ndate: 2025-03-{:02}\n---\n\n# Other {i}\n\nalpha once.\n",
            i + 2
        );
        fs::write(root.join(format!("notes/other-{i}.md")), text).unwrap();
    }
    dir
}

#[test]
fn limit_returns_documents_not_chunks_of_one_document() {
    let vault = contract_vault();
    index(vault.path());

    let report = search_json(vault.path(), &["alpha", "--limit", "10"]);
    let got = result_paths(&report);
    assert_eq!(got.len(), 10, "{report}");
    assert_eq!(
        got.iter().collect::<std::collections::BTreeSet<_>>().len(),
        10,
        "a document appeared twice: {got:?}"
    );
    assert_eq!(
        got.iter()
            .filter(|path| *path == "notes/dominant.md")
            .count(),
        1,
        "the dominant document must appear once: {got:?}"
    );

    // The dominant document really does own several chunks' worth of score.
    let chunks = common::chunks_json(vault.path(), &[]);
    let dominant = chunks["docs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|doc| doc["rel_path"] == "notes/dominant.md")
        .unwrap();
    assert!(
        dominant["chunks"].as_array().unwrap().len() > 1,
        "{dominant}"
    );

    assert_eq!(
        result_paths(&search_json(vault.path(), &["alpha", "--limit", "3"])).len(),
        3
    );
}

#[test]
fn search_reports_spans_breadcrumbs_and_the_breakdown_but_never_text() {
    let vault = fixture_vault();
    index(vault.path());
    let stdout = run_ok(
        vault.path(),
        &["search", "postgres", "--intent", "history", "--json"],
    );
    let report: Value = serde_json::from_str(&stdout).unwrap();
    let hit = &report["results"][0];

    assert_eq!(report["api"], 1);
    assert_eq!(report["intent"], "history");
    for field in [
        "semantic",
        "lexical",
        "relevance",
        "title_boost",
        "type_authority",
        "status_factor",
        "authority",
        "decay",
        "final",
    ] {
        assert!(
            hit["scores"][field].is_number(),
            "missing scores.{field}: {hit}"
        );
    }
    assert!(hit["span"][0].is_number(), "{hit}");
    assert!(hit["span"][1].is_number(), "{hit}");
    assert!(hit["breadcrumb"].is_array(), "{hit}");
    assert_eq!(hit["chunk_id"].as_str().unwrap().len(), 64, "{hit}");

    // §8.5: the snapshot has no body, so no chunk text can reach the report.
    assert!(
        !stdout.contains("First link in the supersession cycle"),
        "{stdout}"
    );
    assert!(!stdout.contains("\"text\""), "{stdout}");
}

#[test]
fn the_human_table_has_the_documented_columns() {
    let vault = fixture_vault();
    index(vault.path());
    let stdout = run_ok(vault.path(), &["search", "cache", "--intent", "history"]);

    for header in ["score", "authority", "status", "date", "path"] {
        assert!(stdout.contains(header), "missing {header}: {stdout}");
    }
    assert!(stdout.contains("result(s)"), "{stdout}");
}

#[test]
fn an_empty_corpus_returns_no_results() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"empty\"\n",
    )
    .unwrap();

    let report = search_json(dir.path(), &["anything"]);
    assert!(report["results"].as_array().unwrap().is_empty(), "{report}");
    assert!(dir.path().join(".janus/index.bin").exists());
}

// M4D P2 row 1: a `corpus_id` change is cold. A read refuses with one line that
// names the command, and `.janus/` keeps every byte it had.
#[test]
fn a_changed_corpus_id_refuses_with_one_line() {
    let vault = fixture_vault();
    index(vault.path());
    let before = dir_snapshot(&vault.path().join(".janus"));

    let config_path = vault.path().join("janus.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    let rewritten = config.replace("corpus_id = \"fixtures\"", "corpus_id = \"renamed\"");
    assert_ne!(config, rewritten, "fixture config must carry a corpus_id");
    fs::write(&config_path, rewritten).unwrap();

    let output = run(vault.path(), &["search", "cache"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "expected exactly one line, got: {stderr:?}");
    assert!(
        lines[0].contains("index needs a full build: run janus index"),
        "{stderr:?}"
    );
    assert!(lines[0].contains("corpus_id"), "{stderr:?}");
    assert!(lines[0].contains("renamed"), "{stderr:?}");
    assert_eq!(
        dir_snapshot(&vault.path().join(".janus")),
        before,
        "a refused read writes nothing"
    );

    // `janus index` is the cure, and the next read prints nothing.
    index(vault.path());
    let output = run(vault.path(), &["search", "cache"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn search_json_reports_a_fresh_index_with_no_stderr_line() {
    let vault = fixture_vault();
    index(vault.path());

    let output = run(vault.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(report["api"], 1, "the key is additive");
    assert_eq!(report["index"]["stale"], false, "{report}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "",
        "this phase adds no line"
    );
}

#[test]
fn an_edited_document_makes_search_json_report_stale() {
    let vault = fixture_vault();
    index(vault.path());
    let path = vault.path().join("notes/yaml-comments.md");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\nan edit made after the build\n");
    fs::write(&path, text).unwrap();

    let output = run(vault.path(), &["search", "cache", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert_eq!(report["index"]["stale"], true, "{report}");
    assert!(!stderr.contains("refresh:"), "{stderr}");
    assert!(!stderr.contains("stale:"), "{stderr}");

    // `janus index` writes the digest for the edited corpus.
    index(vault.path());
    let report = search_json(vault.path(), &["cache"]);
    assert_eq!(report["index"]["stale"], false, "{report}");
}

#[test]
fn an_edited_document_makes_facts_json_report_stale() {
    let vault = fixture_vault();
    index(vault.path());
    let path = vault.path().join("notes/yaml-comments.md");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\nan edit made after the build\n");
    fs::write(&path, text).unwrap();

    let report = common::facts_json(vault.path(), &[]);

    assert_eq!(report["index"]["stale"], true, "{report}");
}

#[test]
fn search_without_an_index_builds_one() {
    let vault = fixture_vault();
    let report = search_json(vault.path(), &["cache"]);

    assert!(!result_paths(&report).is_empty(), "{report}");
    assert!(vault.path().join(".janus/index.bin").exists());
}

#[test]
fn no_rebuild_on_a_never_built_vault_says_so_and_writes_nothing() {
    let vault = fixture_vault();
    let output = run(vault.path(), &["search", "cache", "--no-rebuild"]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("no index yet"), "{stderr}");
    assert!(stderr.contains("janus index"), "{stderr}");
    assert!(!stderr.contains("unstable"), "{stderr}");
    assert!(!vault.path().join(".janus/index.bin").exists());
    assert!(!vault.path().join(".janus/manifest.json").exists());
}

#[test]
fn no_rebuild_on_a_stale_index_refuses_without_writing() {
    let vault = fixture_vault();
    index(vault.path());
    let dir = vault.path().join(".janus");
    let index_bytes = fs::read(dir.join("index.bin")).unwrap();
    let manifest_bytes = fs::read(dir.join("manifest.json")).unwrap();

    let config_path = vault.path().join("janus.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        config.replace("corpus_id = \"fixtures\"", "corpus_id = \"renamed\""),
    )
    .unwrap();

    let output = run(vault.path(), &["search", "cache", "--no-rebuild"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("index is unstable"), "{stderr}");
    assert!(stderr.contains("index.bin"), "{stderr}");

    assert_eq!(fs::read(dir.join("index.bin")).unwrap(), index_bytes);
    assert_eq!(fs::read(dir.join("manifest.json")).unwrap(), manifest_bytes);
}

#[test]
fn no_rebuild_reads_a_valid_generation() {
    let vault = fixture_vault();
    index(vault.path());
    let report = search_json(vault.path(), &["cache", "--no-rebuild"]);
    assert!(!result_paths(&report).is_empty(), "{report}");
}

#[test]
fn two_identical_runs_are_identical() {
    let vault = fixture_vault();
    index(vault.path());

    let args = ["search", "cache", "--intent", "history", "--json"];
    let mut first: Value = serde_json::from_str(&run_ok(vault.path(), &args)).unwrap();
    let mut second: Value = serde_json::from_str(&run_ok(vault.path(), &args)).unwrap();
    first["taken_ms"] = Value::from(0);
    second["taken_ms"] = Value::from(0);
    assert_eq!(first, second);
}

#[test]
fn search_finds_the_corpus_from_a_subdirectory() {
    let vault = fixture_vault();
    index(vault.path());

    let output = common::janus()
        .env("RUST_LOG", "off")
        .current_dir(vault.path().join("decisions"))
        .args(["search", "cache", "--intent", "history"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("decisions/"));
}

#[test]
fn an_unknown_intent_is_a_usage_error() {
    let vault = fixture_vault();
    index(vault.path());
    let output = run(vault.path(), &["search", "cache", "--intent", "sometime"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unknown intent"), "{stderr}");
    assert!(stderr.contains("history"), "{stderr}");
}

/// A vault with four documents covering owning scopes, `shared_with`, and global.
fn scope_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"scopes\"\n",
    )
    .unwrap();

    let docs = [
        ("acme.md", "title: Acme\nscope: org:acme", "scope probe"),
        ("beta.md", "title: Beta\nscope: org:beta", "scope probe"),
        ("global.md", "title: Global", "scope probe"),
        (
            "shared.md",
            "title: Shared\nscope: org:beta\nshared_with: [shared:finance]",
            "scope probe",
        ),
    ];
    for (path, front, body) in docs {
        fs::write(
            root.join(path),
            format!("---\n{front}\n---\n\n# {path}\n\n{body}\n"),
        )
        .unwrap();
    }
    dir
}

/// A vault where status, type and decay disagree, all dated for determinism.
fn status_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"statuses\"\n",
    )
    .unwrap();

    let docs = [
        (
            "draft-decision.md",
            "type: decision\nstatus: draft\ndate: 2024-06-01",
        ),
        (
            "active-decision.md",
            "type: decision\nstatus: active\ndate: 2024-06-01",
        ),
        (
            "frozen-spec.md",
            "type: spec\nstatus: frozen\ndate: 2024-06-01",
        ),
        (
            "retired-principle.md",
            "type: principle\nstatus: deprecated\ndate: 2024-06-01",
        ),
        (
            "active-note.md",
            "type: note\nstatus: active\ndate: 2024-06-01",
        ),
    ];
    for (path, front) in docs {
        fs::write(
            root.join(path),
            format!("---\ntitle: {path}\n{front}\n---\n\n# {path}\n\nquorum rules.\n"),
        )
        .unwrap();
    }
    dir
}

fn paths(report: &Value) -> std::collections::BTreeSet<String> {
    result_paths(report).into_iter().collect()
}

#[test]
fn types_and_tags_filter_from_the_published_snapshot() {
    let vault = fixture_vault();
    index(vault.path());

    let by_alias = search_json(
        vault.path(),
        &["cache", "--intent", "history", "--types", "adr"],
    );
    assert_eq!(result_paths(&by_alias).len(), 3, "{by_alias}");
    for hit in by_alias["results"].as_array().unwrap() {
        assert_eq!(hit["type"], "decision", "{hit}");
    }

    let specs = search_json(vault.path(), &["crlf", "--types", "spec"]);
    assert!(!result_paths(&specs).is_empty(), "{specs}");
    for hit in specs["results"].as_array().unwrap() {
        assert_eq!(hit["type"], "spec", "{hit}");
    }

    let tagged = search_json(vault.path(), &["yaml", "--tags", "comments"]);
    assert_eq!(
        result_paths(&tagged),
        vec!["notes/yaml-comments.md".to_owned()],
        "{tagged}"
    );

    // Tag matching folds case; the frontmatter tag is `yaml`.
    let folded = search_json(vault.path(), &["yaml", "--tags", "YAML"]);
    assert_eq!(
        result_paths(&folded),
        vec!["notes/yaml-comments.md".to_owned()]
    );

    let excluded = search_json(
        vault.path(),
        &["yaml", "--tags", "comments", "--types", "spec"],
    );
    assert!(result_paths(&excluded).is_empty(), "{excluded}");
}

#[test]
fn scopes_filter_end_to_end() {
    let vault = scope_vault();
    index(vault.path());

    assert_eq!(paths(&search_json(vault.path(), &["probe"])).len(), 4);
    assert_eq!(
        paths(&search_json(
            vault.path(),
            &["probe", "--scope", "org:acme"]
        )),
        std::collections::BTreeSet::from(["acme.md".to_owned(), "global.md".to_owned()])
    );
    assert_eq!(
        paths(&search_json(
            vault.path(),
            &["probe", "--scope", "org:beta"]
        )),
        std::collections::BTreeSet::from([
            "beta.md".to_owned(),
            "global.md".to_owned(),
            "shared.md".to_owned(),
        ])
    );
    assert_eq!(
        paths(&search_json(
            vault.path(),
            &["probe", "--scope", "shared:finance"]
        )),
        std::collections::BTreeSet::from(["global.md".to_owned(), "shared.md".to_owned()])
    );
    assert_eq!(
        paths(&search_json(
            vault.path(),
            &["probe", "--scope", "org:acme", "--scope-only"]
        )),
        std::collections::BTreeSet::from(["acme.md".to_owned()])
    );
}

#[test]
fn scope_only_without_scope_is_a_usage_error() {
    let vault = scope_vault();
    index(vault.path());
    let output = run(vault.path(), &["search", "probe", "--scope-only"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--scope-only"), "{stderr}");
    assert!(stderr.contains("--scope"), "{stderr}");

    // The same rule protects `facts`.
    let output = run(vault.path(), &["facts", "--scope-only"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn include_superseded_widens_current_and_nothing_else() {
    let vault = fixture_vault();
    index(vault.path());

    let default = search_json(vault.path(), &["cache", "--limit", "20"]);
    assert!(
        default["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["status"] != "superseded"),
        "{default}"
    );

    let flagged = search_json(
        vault.path(),
        &["cache", "--include-superseded", "--limit", "20"],
    );
    let superseded: Vec<&Value> = flagged["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|hit| hit["status"] == "superseded")
        .collect();
    assert_eq!(superseded.len(), 3, "{flagged}");
    for hit in &superseded {
        assert_eq!(hit["scores"]["status_factor"], 1.0, "{hit}");
    }

    // Same chunk, same span, same breadcrumb as the history view.
    let history = search_json(
        vault.path(),
        &["cache", "--intent", "history", "--limit", "20"],
    );
    let by_path: std::collections::BTreeMap<String, &Value> = history["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| (hit["rel_path"].as_str().unwrap().to_owned(), hit))
        .collect();
    for hit in &superseded {
        let reference = by_path[hit["rel_path"].as_str().unwrap()];
        assert_eq!(hit["chunk_id"], reference["chunk_id"], "{hit}");
        assert_eq!(hit["span"], reference["span"], "{hit}");
        assert_eq!(hit["breadcrumb"], reference["breadcrumb"], "{hit}");
    }

    // A search override must never widen the facts predicate.
    let facts = common::facts_json(vault.path(), &["--query", "cache"]);
    assert!(result_paths(&facts).is_empty(), "{facts}");
}

#[test]
fn a_decay_override_changes_the_score_but_not_a_timeless_type() {
    let vault = status_vault();
    index(vault.path());

    // `decision`/`spec`/`principle` are timeless in the shipped config, so a
    // half-life override changes their decay not at all (§9 gap 9: the flag
    // overrides the parameter, never the exemption). A `note` does decay, and
    // decays harder with a one-day half-life.
    let plain = search_json(vault.path(), &["quorum", "--intent", "current"]);
    let overridden = search_json(
        vault.path(),
        &["quorum", "--intent", "current", "--decay-half-life", "1"],
    );
    let decay = |report: &Value, path: &str| -> f64 {
        report["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["rel_path"] == path)
            .unwrap_or_else(|| panic!("no hit for {path}: {report}"))["scores"]["decay"]
            .as_f64()
            .unwrap()
    };
    let note_plain = decay(&plain, "active-note.md");
    let note_overridden = decay(&overridden, "active-note.md");
    assert!(note_plain < 1.0, "{plain}");
    assert!(note_overridden < note_plain, "{overridden}");
    for hit in overridden["results"].as_array().unwrap() {
        if hit["type"] != "note" {
            assert_eq!(hit["scores"]["decay"], 1.0, "{hit}");
        }
    }
}

#[test]
fn facts_excludes_draft_and_unknown_while_current_includes_them() {
    let vault = status_vault();
    index(vault.path());

    let current = paths(&search_json(vault.path(), &["quorum"]));
    assert_eq!(
        current,
        std::collections::BTreeSet::from([
            "active-decision.md".to_owned(),
            "active-note.md".to_owned(),
            "draft-decision.md".to_owned(),
            "frozen-spec.md".to_owned(),
        ])
    );

    let facts = paths(&common::facts_json(vault.path(), &["--query", "quorum"]));
    assert_eq!(
        facts,
        std::collections::BTreeSet::from(["active-decision.md".to_owned()])
    );
}

#[test]
fn facts_lists_active_decisions_specs_and_principles_without_a_query() {
    let vault = fixture_vault();
    index(vault.path());
    let report = common::facts_json(vault.path(), &[]);

    assert!(!result_paths(&report).is_empty(), "{report}");
    for hit in report["results"].as_array().unwrap() {
        assert_eq!(hit["status"], "active", "{hit}");
        assert!(
            ["decision", "spec", "principle"].contains(&hit["type"].as_str().unwrap()),
            "{hit}"
        );
        assert_eq!(hit["scores"]["decay"], 1.0, "{hit}");
        assert_eq!(hit["scores"]["status_factor"], 1.0, "{hit}");
    }
}

#[test]
fn facts_ignores_the_superseded_search_override() {
    let vault = fixture_vault();
    index(vault.path());

    // `--include-superseded` does not exist on `facts`; the predicate is fixed.
    let output = run(
        vault.path(),
        &["facts", "--query", "cache", "--include-superseded"],
    );
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(output.status.code(), Some(2));
}

/// A vault with one document per date source, plus one that has no date at all.
///
/// Every dated document carries `tags: [dated]`, so one search can include the
/// undated document and another can exclude it from the same index.
fn provenance_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"provenance\"\n",
    )
    .unwrap();

    // Ordinary documents: the date is in the frontmatter or nowhere.
    fs::write(
        root.join("frontmatter.md"),
        "---\ntitle: Authored\ndate: 2025-05-06\ntags: [dated]\n---\n\nprobe\n",
    )
    .unwrap();
    fs::write(
        root.join("undated.md"),
        "---\ntitle: Undated\n---\n\nprobe\n",
    )
    .unwrap();

    // File-name dates at all three precisions.
    for name in ["2025-04-03-day.md", "2025-04-month.md", "2025-year.md"] {
        fs::write(
            root.join(name),
            format!("---\ntitle: {name}\ntags: [dated]\n---\n\nprobe\n"),
        )
        .unwrap();
    }

    // Folder dates at all three precisions.
    for (folder, file) in [
        ("2025.07.08_folder", "day.md"),
        ("2025-09_folder", "month.md"),
        ("2025_folder", "year.md"),
    ] {
        fs::create_dir_all(root.join(folder)).unwrap();
        fs::write(
            root.join(folder).join(file),
            format!("---\ntitle: {folder}/{file}\ntags: [dated]\n---\n\nprobe\n"),
        )
        .unwrap();
    }
    dir
}

/// Reads `date_source` for every result, keyed by `rel_path`.
fn date_sources(report: &Value) -> std::collections::BTreeMap<String, String> {
    report["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|hit| {
            (
                hit["rel_path"].as_str().expect("rel_path").to_owned(),
                hit["date_source"].as_str().expect("date_source").to_owned(),
            )
        })
        .collect()
}

#[test]
fn every_result_names_the_rung_that_produced_its_date() {
    let vault = provenance_vault();
    index(vault.path());
    let report = search_json(
        vault.path(),
        &["probe", "--intent", "history", "--limit", "20"],
    );

    // One entry per rung of the ladder, written by hand from the file tree.
    let expected = std::collections::BTreeMap::from([
        ("2025-04-03-day.md".to_owned(), "filename".to_owned()),
        ("2025-04-month.md".to_owned(), "filename_month".to_owned()),
        ("2025-year.md".to_owned(), "filename_year".to_owned()),
        ("2025.07.08_folder/day.md".to_owned(), "folder".to_owned()),
        (
            "2025-09_folder/month.md".to_owned(),
            "folder_month".to_owned(),
        ),
        ("2025_folder/year.md".to_owned(), "folder_year".to_owned()),
        ("frontmatter.md".to_owned(), "frontmatter".to_owned()),
        ("undated.md".to_owned(), "absent".to_owned()),
    ]);
    assert_eq!(date_sources(&report), expected, "{report}");

    // The field is additive: the envelope keeps its version.
    assert_eq!(report["api"], 1, "{report}");

    // Only the absent one is null: the source never outruns the date.
    for hit in report["results"].as_array().unwrap() {
        let absent = hit["date_source"] == "absent";
        assert_eq!(hit["date"].is_null(), absent, "{hit}");
    }
}

#[test]
fn the_undated_note_counts_only_returned_results() {
    let vault = provenance_vault();
    index(vault.path());

    let all = search_json(
        vault.path(),
        &["probe", "--intent", "history", "--limit", "20"],
    );
    let notes: Vec<&str> = all["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|note| note.as_str().expect("note"))
        .collect();
    assert!(
        notes.contains(&"1 result(s) have no date and do not decay"),
        "{all}"
    );

    // The undated document is the only one without the tag, so this search
    // returns none of it and must not carry the line.
    let dated = search_json(
        vault.path(),
        &[
            "probe", "--intent", "history", "--tags", "dated", "--limit", "20",
        ],
    );
    assert_eq!(date_sources(&dated).len(), 7, "{dated}");
    assert!(
        !dated["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("no date")),
        "{dated}"
    );
}

#[test]
fn facts_carries_the_date_source_too() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"facts\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("decisions")).unwrap();
    fs::create_dir_all(root.join("specs")).unwrap();
    fs::write(
        root.join("decisions/adr.md"),
        "---\ntitle: Authored\ntype: decision\ndate: 2025-01-02\n---\n\nprobe\n",
    )
    .unwrap();
    fs::write(
        root.join("specs/2025-02-03-named.md"),
        "---\ntitle: Named\ntype: spec\n---\n\nprobe\n",
    )
    .unwrap();
    index(root);

    let report = common::facts_json(root, &[]);
    assert_eq!(
        date_sources(&report),
        std::collections::BTreeMap::from([
            ("decisions/adr.md".to_owned(), "frontmatter".to_owned()),
            (
                "specs/2025-02-03-named.md".to_owned(),
                "filename".to_owned()
            ),
        ]),
        "{report}"
    );
}

/// A vault whose documents are all `note`: it declares no fact type at all.
fn no_fact_types_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"cold-start\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();

    // Newest first is 2025-07-02, 2025-03-04, then the undated document.
    for (name, date) in [
        ("2025-03-04-older.md", "2025-03-04"),
        ("2025-07-02-newer.md", "2025-07-02"),
    ] {
        fs::write(
            root.join("notes").join(name),
            format!("---\ntitle: {name}\ndate: {date}\n---\n\nquorum probe\n"),
        )
        .unwrap();
    }
    fs::write(
        root.join("notes/undated.md"),
        "---\ntitle: Undated\n---\n\nquorum probe\n",
    )
    .unwrap();
    dir
}

#[test]
fn facts_explains_a_corpus_without_fact_types() {
    let vault = no_fact_types_vault();
    index(vault.path());
    let report = common::facts_json(vault.path(), &[]);

    let notes: Vec<&str> = report["notes"]
        .as_array()
        .expect("notes")
        .iter()
        .map(|note| note.as_str().expect("note"))
        .collect();
    assert!(
        notes.contains(
            &"no document declares type decision, spec or principle; \
              showing the newest documents instead"
        ),
        "{report}"
    );
    assert!(
        !notes
            .iter()
            .any(|note| note.contains("ordered by type authority")),
        "the fallback list is not authority-ordered: {report}"
    );

    // Newest of any type, then the undated document last.
    assert_eq!(
        result_paths(&report),
        vec![
            "notes/2025-07-02-newer.md".to_owned(),
            "notes/2025-03-04-older.md".to_owned(),
            "notes/undated.md".to_owned(),
        ],
        "{report}"
    );
    for hit in report["results"].as_array().unwrap() {
        assert_eq!(hit["type"], "note", "{hit}");
        if hit["date_source"] == "absent" {
            // No anchor means no age, whatever today is.
            assert_eq!(hit["scores"]["decay"], 1.0, "{hit}");
        }
    }
}

#[test]
fn facts_keeps_the_fact_list_when_a_fact_type_exists() {
    // The fixture has decisions, so the note must not appear there.
    let vault = fixture_vault();
    index(vault.path());
    let report = common::facts_json(vault.path(), &[]);
    assert!(
        !report["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("no document declares")),
        "{report}"
    );
}
