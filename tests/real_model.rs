//! The one test that loads real weights (M4 P3, P5's acceptance step).
//!
//! Every other test in this crate is model-free on purpose: a fixture ONNX
//! export is ~90 MB, and a suite that needs it cannot run on a fresh checkout.
//! The cost of that discipline is that `FastEmbed::load`'s happy path is never
//! executed automatically, and P5's α sweep is the first thing that needs it.
//! This test is where that path is exercised — deliberately, by a person who has
//! a model on disk, never in CI.
//!
//! Run it with a model directory:
//!
//! ```text
//! JANUS_MODEL_DIR=/path/to/model \
//!   cargo test --features embeddings --test real_model -- --ignored --nocapture
//! ```
//!
//! The directory must hold the five files in
//! `janus_mem::index::embed::fastembed::MODEL_FILES` — an `all-MiniLM-L6-v2`
//! ONNX export (`model.onnx`, `tokenizer.json`, `config.json`,
//! `special_tokens_map.json`, `tokenizer_config.json`).
//!
//! Nothing here downloads: `resolve` reads the directory or degrades. A missing
//! `JANUS_MODEL_DIR`, or a directory without a model, **fails** rather than
//! returning early — `#[ignore]` already means "run me on purpose", so a silent
//! skip would be a test that cannot fail.

#![cfg(feature = "embeddings")]

use std::env;
use std::path::{Path, PathBuf};

use janus_mem::config::Config;
use janus_mem::index::embed::{ModelDirs, Resolved};

/// A config that asks for the model this crate ships support for.
fn config() -> Config {
    Config::from_toml(
        "[embedding]\nmodel = \"AllMiniLML6V2\"\n",
        Path::new("<real-model-test>"),
    )
    .expect("the inline config parses")
}

/// `ModelDirs` with `JANUS_MODEL_DIR` as the environment override.
///
/// The cache candidate is a path that cannot exist, so a model left in the
/// cache cannot make a run that forgot the variable look like a success.
fn dirs() -> ModelDirs {
    let env = env::var_os("JANUS_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "JANUS_MODEL_DIR is not set; point it at a directory holding {}",
                janus_mem::index::embed::fastembed::MODEL_FILES.join(", ")
            )
        });

    ModelDirs {
        config: None,
        env: Some(env),
        cache: PathBuf::from("/no/such/model/cache"),
    }
}

/// The Euclidean norm, in `f64` so the assertion is not about `f32` rounding.
fn norm(row: &[f32]) -> f64 {
    row.iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt()
}

#[test]
#[ignore = "loads ~90 MB of real weights; P5's acceptance step, never CI"]
fn a_real_model_embeds_384_finite_values() {
    let embedder = match janus_mem::index::embed::resolve(&config(), &dirs()) {
        Resolved::Ready(embedder) => embedder,
        Resolved::Degraded(reason, detail) => {
            panic!("expected a loaded model, got `{reason}`: {detail}")
        }
    };

    // The identity the manifest and the reuse keys will record.
    assert_eq!(embedder.id().name, "AllMiniLML6V2");
    assert_eq!(embedder.id().dim, 384);
    assert_eq!(embedder.id().dtype, "f32");
    assert_eq!(embedder.id().normalisation, "l2");

    let batch = [
        "cache decisions expire after an hour",
        "redis eviction policy",
        "a sentence with no shared vocabulary at all",
    ];
    let rows = embedder.embed(&batch).expect("a loaded model embeds");

    // One row per input, in order, each of the declared width.
    assert_eq!(rows.len(), batch.len());
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(
            row.len(),
            384,
            "row {index} is {} values, not 384",
            row.len()
        );
        assert!(
            row.iter().all(|value| value.is_finite()),
            "row {index} has a non-finite value"
        );
        // The writer normalises again (V-7), so this asserts the model's own
        // output is already unit length — a wrong pooling mode would not be.
        let norm = norm(row);
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "row {index} has norm {norm}, expected 1.0"
        );
    }

    // Two different texts must not be the same vector: a model that returned a
    // constant row would pass every check above.
    assert_ne!(rows[0], rows[1]);
}
