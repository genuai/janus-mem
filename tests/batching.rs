//! Batching by `[embedding] batch_size` (§5 P3).
//!
//! `build::plan` has split chunk texts by `batch_size` since P1b, but nothing
//! exercised it: `HashEmbedder` is trivial, so a wrong split changed no result.
//! These tests make the split observable. `RecordingEmbedder` keeps every batch
//! it is handed, and the assertions are about the *sequence* of calls — the same
//! texts, in the same order, each exactly once, in batches of the configured
//! size — rather than about the vectors, which are the hash double's business.
//!
//! No model is loaded and nothing sleeps: the recorder is a `Mutex<Vec<_>>`, and
//! the reference order comes from a second build of the same vault with a batch
//! size large enough to send everything in one call.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use janus_mem::config::Config;
use janus_mem::document::Document;
use janus_mem::index::build::{self, BuildPlan};
use janus_mem::index::embed::{EmbedError, Embedder, EmbedderId, HashEmbedder};
use janus_mem::vault::{self, WalkOptions, identity, resolve};

/// A batch size larger than any test vault, so the reference build makes one call.
const ONE_CALL: usize = 1000;

/// Records every batch it is asked to embed, then delegates to the hash double.
struct RecordingEmbedder {
    inner: HashEmbedder,
    batches: Mutex<Vec<Vec<String>>>,
}

impl RecordingEmbedder {
    fn new() -> Self {
        Self {
            inner: HashEmbedder::new(64).expect("64 is a valid dimension"),
            batches: Mutex::new(Vec::new()),
        }
    }

    fn batches(&self) -> Vec<Vec<String>> {
        self.batches.lock().expect("the recorder lock").clone()
    }

    /// Every text in call order, flattened.
    fn texts(&self) -> Vec<String> {
        self.batches().into_iter().flatten().collect()
    }

    fn sizes(&self) -> Vec<usize> {
        self.batches().iter().map(Vec::len).collect()
    }
}

impl Embedder for RecordingEmbedder {
    fn id(&self) -> &EmbedderId {
        self.inner.id()
    }

    fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.batches
            .lock()
            .expect("the recorder lock")
            .push(batch.iter().map(|text| (*text).to_owned()).collect());
        self.inner.embed(batch)
    }
}

/// A vault of `docs` documents, each with its own title and body.
fn vault(docs: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("janus.toml"),
        "index_dir = \".janus\"\ncorpus_id = \"batching\"\n\n[embedding]\nmodel = \"hash-64\"\n",
    )
    .expect("write config");
    for n in 0..docs {
        fs::write(
            dir.path().join(format!("note-{n}.md")),
            format!(
                "---\ntitle: Note {n}\ndate: 2025-01-01\ntype: note\nstatus: active\n---\n\n\
                 # Note {n}\n\nsection {n} cache decision redis alpha\n"
            ),
        )
        .expect("write document");
    }
    dir
}

/// The config for one batch size; the vault on disk is the same for all of them.
fn config(root: &Path, batch_size: usize) -> Config {
    Config::from_toml(
        &format!(
            "index_dir = \".janus\"\ncorpus_id = \"batching\"\n\n\
             [embedding]\nmodel = \"hash-64\"\nbatch_size = {batch_size}\n"
        ),
        &root.join("janus.toml"),
    )
    .expect("the inline config parses")
}

fn documents(root: &Path) -> Vec<Document> {
    let config = config(root, 1);
    let walk = vault::walk(root, &config, WalkOptions::default()).expect("walk the vault");
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, &config);
    identity::derive_identity(&mut documents, &config);
    documents
}

fn plan(root: &Path, batch_size: usize, embedder: &dyn Embedder) -> BuildPlan {
    build::plan(
        &config(root, batch_size),
        &documents(root),
        None,
        Some(embedder),
    )
    .expect("the test vault builds")
}

#[test]
fn every_chunk_is_embedded_once_in_order_in_batches_of_the_configured_size() {
    let dir = vault(7);

    // The oracle: one call, so its order is the build's own chunk order.
    let reference = RecordingEmbedder::new();
    let reference_plan = plan(dir.path(), ONE_CALL, &reference);
    let expected = reference.texts();
    assert_eq!(reference.sizes(), vec![expected.len()]);
    assert!(
        expected.len() >= 7,
        "seven documents yield at least seven chunks, got {}",
        expected.len()
    );

    let batched = RecordingEmbedder::new();
    let batched_plan = plan(dir.path(), 3, &batched);

    // The same texts, in the same order, each exactly once: batching reorders
    // nothing and drops nothing.
    assert_eq!(batched.texts(), expected);
    let mut unique = expected.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), expected.len(), "no chunk is embedded twice");

    // Batches of the configured size, with a short final batch.
    let sizes = batched.sizes();
    let full = expected.len() / 3;
    assert_eq!(
        sizes,
        [vec![3; full], vec![expected.len() % 3]]
            .concat()
            .into_iter()
            .filter(|size| *size > 0)
            .collect::<Vec<_>>()
    );

    // The counters agree with what the embedder actually saw.
    assert_eq!(reference_plan.stats.embedded_chunks, expected.len());
    assert_eq!(batched_plan.stats.embedded_chunks, expected.len());
    assert_eq!(batched_plan.vectors, reference_plan.vectors);
}

#[test]
fn a_batch_size_of_zero_is_clamped_to_one_call_per_chunk() {
    // `[embedding] batch_size` is not validated at parse time, so the build
    // clamps it. Zero must not mean "no embedding" or "one call with nothing".
    let dir = vault(3);

    let reference = RecordingEmbedder::new();
    plan(dir.path(), ONE_CALL, &reference);
    let expected = reference.texts();

    let zero = RecordingEmbedder::new();
    let zero_plan = plan(dir.path(), 0, &zero);

    assert_eq!(zero.texts(), expected);
    assert_eq!(zero.sizes(), vec![1; expected.len()]);
    assert_eq!(zero_plan.stats.embedded_chunks, expected.len());
}

#[test]
fn a_vault_with_no_documents_makes_no_embedder_call() {
    // An empty corpus is not a batch of zero texts: the loop must not call the
    // embedder at all, or a real model would pay a session warm-up for nothing.
    let dir = vault(0);

    let embedder = RecordingEmbedder::new();
    let plan = plan(dir.path(), 3, &embedder);

    assert!(embedder.batches().is_empty(), "{:?}", embedder.batches());
    assert_eq!(plan.stats.embedded_chunks, 0);
    assert!(
        plan.vectors.is_some(),
        "an embedder was resolved, so rows exist"
    );
}
