//! The config hash in the manifest (M7 plan §2.5, M7-24 to M7-26).
//!
//! The manifest recorded the tokenizer, the chunker, the embedder and the corpus
//! signature, but nothing about the config whose values the index caches. This
//! part pins the field itself: a build records one, and two builds of an
//! unchanged corpus record the same one. Cases 8.1–8.5 need the freshness
//! comparison and the reports, which the next part adds.
//!
//! Every expected value is derived by hand from the corpus each test writes. No
//! test reads the wall clock, sleeps, or opens the network.

mod common;

use std::fs;
use std::path::Path;

use serde_json::Value;

/// The config a scratch corpus starts with: one path hint, so the document's
/// type comes from `[paths]` and from nowhere else.
fn config_text(paths_type: &str) -> String {
    format!(
        "index_dir = \".janus\"\ncorpus_id = \"cfg\"\n\n[paths]\n\"README.md\" = \"{paths_type}\"\n"
    )
}

/// Writes the config, replacing whatever was there.
fn write_config(root: &Path, paths_type: &str) {
    fs::write(root.join("janus.toml"), config_text(paths_type)).expect("write the config");
}

/// A corpus with one frontmatter-less document whose type `[paths]` decides.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "spec");
    fs::write(
        dir.path().join("README.md"),
        "# Readme\n\nWidgets and their prose.\n",
    )
    .expect("write the document");
    dir
}

/// The manifest a build published.
fn manifest(root: &Path) -> Value {
    let bytes = fs::read(root.join(".janus/manifest.json")).expect("the manifest");
    serde_json::from_slice(&bytes).expect("a manifest is JSON")
}

#[test]
fn two_runs_of_index_record_the_same_config_hash() {
    let dir = scratch();
    let root = dir.path();
    common::index(root);
    let first = manifest(root);
    common::index(root);
    let second = manifest(root);

    let hash = first["config_hash"]
        .as_str()
        .expect("the build records a config hash");
    assert_eq!(second["config_hash"].as_str(), Some(hash));
}
