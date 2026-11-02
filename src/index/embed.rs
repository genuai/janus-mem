//! Embedder identity, the embedder trait, and the deterministic test double
//! (SPEC §7.2, §8.2, §8.3; M4 §3.2).
//!
//! [`Embedder`] is the runtime seam from chunk text to a `vectors.f32` row. It is
//! `Send + Sync` so a later phase can share one instance across threads, and
//! call sites use `&dyn Embedder` because the choice depends on config, the
//! compiled feature set, and model availability, not on a generic parameter.
//!
//! [`EmbedderId`] records the model, dimension, dtype, and normalisation but
//! never hashes the weights (V-20): hashing a ~90 MB ONNX file on every start
//! would break the startup budget to detect a failure that already requires a
//! user action. [`EmbedderId::to_bytes`] is the one stable encoding of that
//! identity (V-22).
//!
//! `HashEmbedder` is a model-free test double (V-6). It tokenizes with the same
//! [`Tokenizer`] as the index, maps every term to a signed bucket with
//! `blake3(term)`, accumulates integer-valued counts into `dim` positions, and
//! L2-normalises the row. Its cosine tracks BM25 because it is a bag of tokens;
//! no quality number in M4 may be produced with it.
//!
//! Identical text produces a bit-identical row across runs and platforms: every
//! count is an integer-valued `f32` (exact below 2²⁴, so summation order cannot
//! change a bit), the bucket and sign come only from `blake3(term)`, and the norm
//! is one pass over `0..dim` in index order. **If a later change adds weights**
//! (tf-idf, position decay, sublinear scaling), the counts stop being exact and
//! P1b's byte-identical claim silently becomes false; that is a deliberate
//! invariant, not a detail to re-derive.
//!
//! [`resolve`] reads only the paths in [`ModelDirs`]. It never opens a socket,
//! downloads, or blocks on a model cache, and an unavailable model returns
//! [`Resolved::Degraded`] with the reason it looked for. The `embeddings` build
//! may fetch ONNX Runtime; runtime never fetches anything (§3.2).

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{Config, ConfigWarning};
use crate::tokenize::Tokenizer;

use super::vectors::normalize;

/// What an embedder is, for the manifest and the reuse keys (§8.3, §3.4).
///
/// `dtype` is `"f32"` and `normalisation` is `"l2"` for every embedder M4 can
/// write, but they are recorded rather than assumed: a future dtype or a
/// different row rule is a rebuild, not a silent reinterpretation of old bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedderId {
    /// Model name, e.g. `"hash-64"` or `"AllMiniLML6V2"`.
    pub name: String,
    /// Row width in `f32` values.
    pub dim: u32,
    /// Element type, `"f32"` in M4.
    pub dtype: String,
    /// Row normalisation, `"l2"` in M4.
    pub normalisation: String,
}

impl EmbedderId {
    /// Encodes the identity for a cache key or manifest comparison.
    ///
    /// Every field is length-prefixed with a little-endian `u64`, in declaration
    /// order. The prefix is what keeps `name = "hash"`/`dtype = "-64"` distinct
    /// from `name = "hash-"`/`dtype = "64"`; without it the two spellings would
    /// concatenate to the same bytes and reuse the wrong vectors (V-22). The
    /// width matches the existing text prefix in `chunk_id`, and the `dim`
    /// field is prefixed too so adding a field later cannot shift an old decode.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes =
            Vec::with_capacity(self.name.len() + self.dtype.len() + self.normalisation.len() + 32);
        push_field(&mut bytes, self.name.as_bytes());
        push_field(&mut bytes, &self.dim.to_le_bytes());
        push_field(&mut bytes, self.dtype.as_bytes());
        push_field(&mut bytes, self.normalisation.as_bytes());
        bytes
    }

    /// The configuration warning this identity earns, if any.
    ///
    /// `hash-<n>` is selectable in production but is a test double. The warning
    /// is emitted once by [`resolve`], so a user who configured it sees why its
    /// vectors are bag-of-words rather than semantic (§3.2).
    #[must_use]
    pub fn config_warning(&self) -> Option<ConfigWarning> {
        hash_model_dim(&self.name).map(|_| ConfigWarning::TestDoubleEmbedder {
            model: self.name.clone(),
        })
    }
}

/// A batch embedder.
///
/// Implementors return one row per input, in order, each of `id().dim` finite
/// values. The writer normalises and validates the rows again (§3.2 V-7), so a
/// violation of that contract fails the build rather than reaching disk.
pub trait Embedder: Send + Sync {
    /// The identity recorded in the manifest and hashed into reuse keys.
    fn id(&self) -> &EmbedderId;

    /// Embeds every text in `batch`, preserving order.
    ///
    /// # Errors
    ///
    /// [`EmbedError`] when the implementation cannot produce the batch. The
    /// caller fails the build; it never substitutes a zero row.
    fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// A failed embedding batch.
///
/// The fields are public because a build error reports them directly: the
/// embedder name says which model failed, the batch index says where in the
/// caller's sequence it failed, and the reason is the implementation's own
/// diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("embedder `{embedder}` failed on batch {batch}: {reason}")]
pub struct EmbedError {
    /// The resolved embedder's name.
    pub embedder: String,
    /// Index of the batch in the caller's sequence.
    pub batch: usize,
    /// What went wrong, in the implementation's words.
    pub reason: String,
}

/// Feature-hashed tokenizer terms: deterministic, model-free, offline (V-6).
///
/// Construct it with [`HashEmbedder::new`]. The dimension is the only knob; the
/// name is derived from it, so `new(64)` reports `hash-64` in the manifest and
/// in any warning. The exact row for a fixed input is pinned by a literal test:
/// any change to the bucket or sign is a different embedder and must change the
/// name, or P4's reuse key would accept vectors from the old scheme.
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    id: EmbedderId,
    tokenizer: Tokenizer,
}

impl HashEmbedder {
    /// Creates a hash embedder with `dim` signed feature buckets.
    ///
    /// Returns `None` for `dim == 0`: a zero-width row cannot describe a
    /// `vectors.f32` generation (§3.1), so the only useful outcome is refusal
    /// rather than an embedder whose output every writer rejects.
    #[must_use]
    pub fn new(dim: u32) -> Option<Self> {
        if dim == 0 {
            return None;
        }

        Some(Self {
            id: EmbedderId {
                name: format!("hash-{dim}"),
                dim,
                dtype: "f32".to_owned(),
                normalisation: "l2".to_owned(),
            },
            tokenizer: Tokenizer::new(),
        })
    }
}

impl Embedder for HashEmbedder {
    fn id(&self) -> &EmbedderId {
        &self.id
    }

    fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let width = self.id.dim as usize;
        let mut rows = Vec::with_capacity(batch.len());

        for text in batch {
            let mut row = vec![0.0f32; width];
            let tokens = self.tokenizer.tokenize(text);
            for term in tokens.iter() {
                let digest = blake3::hash(term.as_bytes());
                let bytes = digest.as_bytes();

                let mut low = [0u8; 8];
                low.copy_from_slice(&bytes[..8]);
                let bucket = (u64::from_le_bytes(low) % self.id.dim as u64) as usize;

                // The sign comes from a byte the bucket does not use, so a
                // bucket choice cannot also flip an otherwise-neutral term.
                let sign = if bytes[8] & 1 == 0 { 1.0 } else { -1.0 };
                row[bucket] += sign;
            }

            let _ = normalize(&mut row);
            rows.push(row);
        }

        Ok(rows)
    }
}

/// The directories `main` may consult when resolving a model.
///
/// They are an argument rather than ambient state, so nothing below the binary
/// edge reads `std::env` (V-25). Precedence is decided by the caller: config
/// directory, then environment override, then the model cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDirs {
    /// `[embedding] model_dir`, when configured.
    pub config: Option<PathBuf>,
    /// `JANUS_MODEL_DIR`, when set at the edge.
    pub env: Option<PathBuf>,
    /// The model cache directory, always present.
    pub cache: PathBuf,
}

/// The outcome of resolving config, feature, and model availability.
///
/// [`Resolved::Degraded`] carries a short reason plus a detail string. The
/// reason is stable enough to branch on; the detail names a model or directory
/// for the user. Resolution never returns an error: a missing model is a
/// degraded run, not a failed command (§3.6).
pub enum Resolved {
    /// An embedder is ready to use.
    Ready(Box<dyn Embedder>),
    /// No embedder is available, so the caller runs lexical-only.
    Degraded(&'static str, String),
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(embedder) => f.debug_tuple("Ready").field(embedder.id()).finish(),
            Self::Degraded(reason, detail) => f
                .debug_tuple("Degraded")
                .field(reason)
                .field(detail)
                .finish(),
        }
    }
}

/// Resolves `[embedding]` plus the compiled feature set into an embedder or a
/// reason.
///
/// This is §3.2's dispatch table. The `hash-` and `enabled` rows do not depend
/// on the feature; the third row does:
///
/// | `[embedding] model` | feature off | feature on, files present | feature on, files absent |
/// |---|---|---|---|
/// | `hash-<n>`, `n > 0` | [`Resolved::Ready`] with a [`HashEmbedder`] | same | same |
/// | any other name | `Degraded("not compiled")` | `Ready` with a `FastEmbed` | `Degraded("model not found")` |
/// | `enabled = false` | `Degraded("disabled")` | same | same |
///
/// Resolution never opens a socket, downloads, or blocks (§3.2, V-13). The
/// feature-on arm reads five file names per candidate directory and loads the
/// model from disk only when all five exist; anything else degrades with a
/// reason that names the directories searched, because embedding is never
/// required for correctness (§3.6 row 1).
#[must_use]
pub fn resolve(config: &Config, dirs: &ModelDirs) -> Resolved {
    if !config.embedding.enabled {
        return Resolved::Degraded(
            "disabled",
            "disabled in janus.toml; set `[embedding] enabled = true` to embed".to_owned(),
        );
    }

    if config.embedding.model.starts_with("hash-") {
        if let Some(dim) = hash_model_dim(&config.embedding.model).filter(|dim| *dim > 0)
            && let Some(embedder) = HashEmbedder::new(dim)
        {
            if let Some(warning) = embedder.id.config_warning() {
                tracing::warn!("{warning}");
            }
            return Resolved::Ready(Box::new(embedder));
        }
        return Resolved::Degraded(
            "invalid model",
            format!(
                "model `{}` is not a valid hash-<n> dimension; use n >= 1",
                config.embedding.model
            ),
        );
    }

    model_arm(config, dirs)
}

/// The non-`hash-` arm of [`resolve`], with the `embeddings` feature compiled
/// out: a model cannot be loaded, so the reason names the feature (V-13).
#[cfg(not(feature = "embeddings"))]
fn model_arm(config: &Config, dirs: &ModelDirs) -> Resolved {
    Resolved::Degraded(
        "not compiled",
        format!(
            "model `{}` needs the `embeddings` feature; model directories: {dirs:?}",
            config.embedding.model
        ),
    )
}

/// The non-`hash-` arm of [`resolve`], with the `embeddings` feature compiled
/// in.
///
/// Three outcomes, none of them fatal (§3.2's dispatch table, §3.6 row 1): a
/// directory holding every model file loads and is [`Resolved::Ready`]; no such
/// directory is `Degraded("model not found")` naming what was searched; a
/// present but unreadable or unusable model is `Degraded("model unusable")`
/// carrying the path and the loader's own diagnostic. A hard failure here would
/// turn "the model is not installed" into "the command does not work", which
/// §3.6 exists to prevent.
#[cfg(feature = "embeddings")]
fn model_arm(config: &Config, dirs: &ModelDirs) -> Resolved {
    match fastembed::model_dir(dirs) {
        Some(dir) => match fastembed::FastEmbed::load(&dir, &config.embedding.model) {
            Ok(embedder) => Resolved::Ready(Box::new(embedder)),
            Err(detail) => Resolved::Degraded("model unusable", detail),
        },
        None => Resolved::Degraded(
            "model not found",
            format!(
                "`{}` searched {}; expected {}",
                config.embedding.model,
                fastembed::searched(dirs),
                fastembed::MODEL_FILES.join(", ")
            ),
        ),
    }
}

/// Parses `hash-<n>` into its dimension, including the invalid zero case.
///
/// Zero is returned rather than filtered so [`resolve`] can report the specific
/// reason instead of treating `hash-0` as an unrecognised model name.
fn hash_model_dim(model: &str) -> Option<u32> {
    model.strip_prefix("hash-")?.parse::<u32>().ok()
}

/// The ONNX sentence-transformer behind the `embeddings` feature (§3.2, V-13).
///
/// The module exists only when the feature is compiled, because `fastembed`
/// pulls `ort`, which fetches a prebuilt ONNX Runtime at **build** time and
/// cannot be cross-compiled from macOS to Windows. A lexical-only build
/// compiles none of this and resolves a non-`hash-` model as `"not compiled"`;
/// runtime never fetches anything.
///
/// **Nothing here downloads.** [`fastembed::FastEmbed::load`] reads the files in
/// a model directory the user placed and hands the bytes to `fastembed`'s
/// user-defined loader, which is the entry point that takes files rather than a
/// hub repository. A missing file is an error naming the path, and [`resolve`]
/// turns any load error into [`Resolved::Degraded`].
#[cfg(feature = "embeddings")]
pub mod fastembed {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use ::fastembed::{
        InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
    };

    use super::{EmbedError, Embedder, EmbedderId, ModelDirs};

    /// Row width of the sentence-transformer M4 ships (§3.2).
    ///
    /// `all-MiniLM-L6-v2` emits 384 values. The id records the width so the
    /// writer can validate every row and the reader can shape the payload
    /// without a model; a directory holding a model of a different width fails
    /// the build's width check rather than silently writing a payload the
    /// manifest misdescribes.
    pub const DIM: u32 = 384;

    /// The files a model directory must contain, in the order
    /// [`FastEmbed::load`] reads them.
    ///
    /// This list is the contract for "the model is present": [`model_dir`]
    /// probes exactly these names, so a directory with an ONNX file and no
    /// tokenizer degrades as `model not found` instead of failing later with a
    /// less useful message.
    pub const MODEL_FILES: [&str; 5] = [
        "model.onnx",
        "tokenizer.json",
        "config.json",
        "special_tokens_map.json",
        "tokenizer_config.json",
    ];

    /// The identity recorded for a model loaded from the configured name.
    ///
    /// `name` is the `[embedding] model` string, not a file name: the id is
    /// metadata about the model (V-20), so a user who swaps the weights under
    /// the same name must rebuild for the vectors to be recomputed.
    #[must_use]
    pub fn id_for(name: &str) -> EmbedderId {
        EmbedderId {
            name: name.to_owned(),
            dim: DIM,
            dtype: "f32".to_owned(),
            normalisation: "l2".to_owned(),
        }
    }

    /// The first directory in precedence order that holds every model file.
    ///
    /// Read-only and model-free: five `is_file` probes per candidate, no
    /// listing, no load, no socket. Precedence is `[embedding] model_dir`, then
    /// `JANUS_MODEL_DIR`, then the cache directory (V-25); `None` is what
    /// [`resolve`](super::resolve) reports as `model not found`.
    #[must_use]
    pub fn model_dir(dirs: &ModelDirs) -> Option<PathBuf> {
        search_roots(dirs)
            .into_iter()
            .find(|root| MODEL_FILES.iter().all(|name| root.join(name).is_file()))
    }

    /// The candidate directories, rendered for a `Degraded` detail.
    ///
    /// Every directory [`model_dir`] probed, in the same order, so the message
    /// tells the user where the tool actually looked rather than where it might
    /// have.
    #[must_use]
    pub fn searched(dirs: &ModelDirs) -> String {
        search_roots(dirs)
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The candidate directories in precedence order, without duplicates.
    ///
    /// De-duplication is why a user who points both `model_dir` and
    /// `JANUS_MODEL_DIR` at one directory sees it once in the message.
    fn search_roots(dirs: &ModelDirs) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::with_capacity(3);
        for root in [dirs.config.as_ref(), dirs.env.as_ref(), Some(&dirs.cache)]
            .into_iter()
            .flatten()
        {
            if !roots.iter().any(|seen| seen == root) {
                roots.push(root.clone());
            }
        }
        roots
    }

    /// A sentence-transformer loaded from disk through ONNX Runtime.
    ///
    /// The model is held behind a [`Mutex`] because `fastembed` needs
    /// `&mut self` while [`Embedder::embed`] takes `&self` and one instance is
    /// shared. A poisoned lock is a failed batch, never a panic.
    ///
    /// [`EmbedError::batch`] is always `0` from this implementation: the caller
    /// has already split the corpus, so this type only ever sees one batch and
    /// the caller's index is the caller's to report.
    pub struct FastEmbed {
        id: EmbedderId,
        model: Mutex<TextEmbedding>,
    }

    impl FastEmbed {
        /// Loads the model files in `dir`, or explains why it could not.
        ///
        /// Never downloads: the five [`MODEL_FILES`] are read with
        /// [`std::fs::read`] and handed to the user-defined loader. A read
        /// failure and a construction failure both return the path plus the
        /// underlying diagnostic, because [`resolve`](super::resolve) turns
        /// either into a degraded run — embedding is never required for
        /// correctness.
        ///
        /// # Errors
        ///
        /// A `String` naming the offending path and why it could not be used.
        pub fn load(dir: &Path, name: &str) -> Result<Self, String> {
            let tokenizer_files = TokenizerFiles {
                tokenizer_file: read_file(dir, "tokenizer.json")?,
                config_file: read_file(dir, "config.json")?,
                special_tokens_map_file: read_file(dir, "special_tokens_map.json")?,
                tokenizer_config_file: read_file(dir, "tokenizer_config.json")?,
            };
            let onnx_file = read_file(dir, "model.onnx")?;
            let mut model = UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files);
            // all-MiniLM-L6-v2 pools the mean of its token vectors. fastembed's
            // precedence table would pick the same method for this output shape,
            // but naming it keeps the row a property of the id rather than of a
            // dependency's default.
            model.pooling = Some(Pooling::Mean);

            let embedding =
                TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::new())
                    .map_err(|error| format!("{}: {error}", dir.display()))?;

            Ok(Self {
                id: id_for(name),
                model: Mutex::new(embedding),
            })
        }
    }

    impl Embedder for FastEmbed {
        fn id(&self) -> &EmbedderId {
            &self.id
        }

        fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
            if batch.is_empty() {
                return Ok(Vec::new());
            }

            let mut model = self.model.lock().map_err(|_| EmbedError {
                embedder: self.id.name.clone(),
                batch: 0,
                reason: "the model lock is poisoned".to_owned(),
            })?;

            // One call, one batch: `build` already split by
            // `[embedding] batch_size`, and a second split inside `fastembed`
            // (its own default is 256) would make the configured size untrue.
            model
                .embed(batch, Some(batch.len()))
                .map_err(|error| EmbedError {
                    embedder: self.id.name.clone(),
                    batch: 0,
                    reason: format!("{} text(s): {error}", batch.len()),
                })
        }
    }

    /// Reads one model file, naming the path when it cannot be read.
    fn read_file(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
        let path = dir.join(name);
        std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Appends one length-prefixed field to an identity encoding.
fn push_field(bytes: &mut Vec<u8>, field: &[u8]) {
    bytes.extend_from_slice(&(field.len() as u64).to_le_bytes());
    bytes.extend_from_slice(field);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::index::vectors::dot;

    fn config(text: &str) -> Config {
        Config::from_toml(text, Path::new("<embed-test>")).expect("inline config parses")
    }

    fn dirs(cache: &str) -> ModelDirs {
        ModelDirs {
            config: None,
            env: None,
            cache: PathBuf::from(cache),
        }
    }

    fn ready(resolved: Resolved) -> Box<dyn Embedder> {
        match resolved {
            Resolved::Ready(embedder) => embedder,
            Resolved::Degraded(reason, detail) => panic!("expected Ready, got {reason}: {detail}"),
        }
    }

    fn cosine(left: &[f32], right: &[f32]) -> f32 {
        dot(left, right)
    }

    #[test]
    fn embedder_id_records_the_test_double_contract() {
        let embedder = HashEmbedder::new(64).expect("64 is a valid dimension");
        let id = embedder.id();

        assert_eq!(id.name, "hash-64");
        assert_eq!(id.dim, 64);
        assert_eq!(id.dtype, "f32");
        assert_eq!(id.normalisation, "l2");
    }

    #[test]
    fn to_bytes_separates_a_field_boundary() {
        let split_left = EmbedderId {
            name: "hash".to_owned(),
            dim: 64,
            dtype: "-64".to_owned(),
            normalisation: "l2".to_owned(),
        };
        let split_right = EmbedderId {
            name: "hash-".to_owned(),
            dim: 64,
            dtype: "64".to_owned(),
            normalisation: "l2".to_owned(),
        };

        assert_ne!(split_left.to_bytes(), split_right.to_bytes());
    }

    #[test]
    fn to_bytes_has_a_hand_derived_encoding() {
        let id = EmbedderId {
            name: "a".to_owned(),
            dim: 2,
            dtype: "b".to_owned(),
            normalisation: "c".to_owned(),
        };

        let mut expected = Vec::new();
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(b"a");
        expected.extend_from_slice(&4u64.to_le_bytes());
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(b"b");
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(b"c");

        assert_eq!(id.to_bytes(), expected);
    }

    #[test]
    fn hash_model_warning_names_the_test_double() {
        let hash = HashEmbedder::new(64).expect("64 is valid");
        let warning = hash
            .id()
            .config_warning()
            .expect("hash-64 is a test double");
        assert_eq!(
            warning,
            ConfigWarning::TestDoubleEmbedder {
                model: "hash-64".to_owned()
            }
        );
        assert!(warning.to_string().contains("hash-64"), "{warning}");
        assert!(warning.to_string().contains("test double"), "{warning}");

        let semantic = EmbedderId {
            name: "AllMiniLML6V2".to_owned(),
            dim: 384,
            dtype: "f32".to_owned(),
            normalisation: "l2".to_owned(),
        };
        assert_eq!(semantic.config_warning(), None);
    }

    #[test]
    fn hash_embedder_is_bit_identical_across_calls() {
        let embedder = HashEmbedder::new(64).expect("64 is valid");
        let batch = ["cache decisions", "redis cache", ""];

        let first = embedder.embed(&batch).expect("hashing cannot fail");
        let second = embedder.embed(&batch).expect("hashing cannot fail");
        assert_eq!(first, second);

        let first_bits: Vec<Vec<u32>> = first
            .iter()
            .map(|row| row.iter().map(|value| value.to_bits()).collect())
            .collect();
        let second_bits: Vec<Vec<u32>> = second
            .iter()
            .map(|row| row.iter().map(|value| value.to_bits()).collect())
            .collect();
        assert_eq!(first_bits, second_bits);
    }

    #[test]
    fn hash_embedder_output_is_pinned_to_a_literal_row() {
        let embedder = HashEmbedder::new(4).expect("4 is valid");
        let rows = embedder.embed(&["a b"]).expect("hashing cannot fail");

        // blake3("a")[0..8] LE = 6027058585768261143, % 4 = 3; byte 8 bit 0 = 0, so +1.
        // blake3("b")[0..8] LE = 11479545966530389264, % 4 = 0; byte 8 bit 0 = 0, so +1.
        // Counts [1, 0, 0, 1]; norm sqrt(2); row [1/sqrt(2), 0, 0, 1/sqrt(2)].
        assert_eq!(rows[0], vec![0.707_106_77, 0.0, 0.0, 0.707_106_77]);
    }

    #[test]
    fn hash_embedder_returns_the_requested_shape() {
        let embedder = HashEmbedder::new(7).expect("7 is valid");
        let rows = embedder
            .embed(&["alpha", "alpha beta", ""])
            .expect("hashing cannot fail");

        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert_eq!(row.len(), 7);
        }
    }

    #[test]
    fn empty_text_is_an_all_zero_row() {
        let embedder = HashEmbedder::new(64).expect("64 is valid");
        let rows = embedder
            .embed(&["", " \n\t ", "🌍"])
            .expect("hashing cannot fail");

        assert_eq!(rows[0], vec![0.0; 64]);
        assert_eq!(rows[1], vec![0.0; 64]);
        assert_eq!(rows[2], vec![0.0; 64]);
    }

    #[test]
    fn non_empty_text_has_a_unit_norm_and_cosine_one_with_itself() {
        let embedder = HashEmbedder::new(64).expect("64 is valid");
        let rows = embedder
            .embed(&["JANUS_WRITE cache decisions", "redis"])
            .expect("hashing cannot fail");

        for row in &rows {
            assert!((cosine(row, row) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn a_repeated_term_keeps_the_same_direction() {
        // Signed counts are multiplied by three and then normalised, so the
        // bag-of-words direction is unchanged. That is the documented double.
        let embedder = HashEmbedder::new(64).expect("64 is valid");
        let rows = embedder
            .embed(&["redis", "redis redis redis"])
            .expect("hashing cannot fail");

        assert!((cosine(&rows[0], &rows[1]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_shared_term_scores_above_a_disjoint_text() {
        let embedder = HashEmbedder::new(64).expect("64 is valid");
        let rows = embedder
            .embed(&[
                "cache decision",
                "cache decision redis",
                "unrelated vocabulary entirely",
            ])
            .expect("hashing cannot fail");
        let (anchor, shares, disjoint) = (&rows[0], &rows[1], &rows[2]);

        assert!(
            cosine(anchor, shares) > cosine(anchor, disjoint),
            "shared terms must dominate: {} vs {}",
            cosine(anchor, shares),
            cosine(anchor, disjoint)
        );
    }

    #[test]
    fn zero_dimensions_are_refused() {
        assert!(HashEmbedder::new(0).is_none());
    }

    #[test]
    fn resolve_returns_a_hash_embedder_for_any_positive_hash_dimension() {
        let config = config("[embedding]\nmodel = \"hash-128\"\n");
        let embedder = ready(resolve(&config, &dirs("/no/such/model/cache")));

        assert_eq!(embedder.id().name, "hash-128");
        assert_eq!(embedder.id().dim, 128);
    }

    #[test]
    fn resolve_degrades_when_embedding_is_disabled() {
        let config = config("[embedding]\nenabled = false\nmodel = \"hash-64\"\n");

        match resolve(&config, &dirs("/no/such/model/cache")) {
            Resolved::Degraded(reason, detail) => {
                assert_eq!(reason, "disabled");
                assert!(detail.contains("janus.toml"), "{detail}");
            }
            Resolved::Ready(_) => panic!("disabled embedding must not resolve"),
        }
    }

    #[test]
    #[cfg(not(feature = "embeddings"))]
    fn resolve_degrades_an_absent_model_without_touching_the_directory() {
        // The feature-off column of §3.2's table. With the feature on the same
        // config is `model not found` instead of `not compiled`, which is the
        // next column and is asserted by its own test below.
        let config = config("[embedding]\nmodel = \"AllMiniLML6V2\"\n");

        match resolve(&config, &dirs("/definitely/not/a/model/cache")) {
            Resolved::Degraded(reason, detail) => {
                assert_eq!(reason, "not compiled");
                assert!(detail.contains("AllMiniLML6V2"), "{detail}");
                assert!(detail.contains("/definitely/not/a/model/cache"), "{detail}");
            }
            Resolved::Ready(_) => panic!("the feature is off, so no model can load"),
        }
    }

    #[test]
    fn resolve_degrades_hash_zero_instead_of_building_an_unusable_id() {
        let config = config("[embedding]\nmodel = \"hash-0\"\n");

        match resolve(&config, &dirs("/tmp")) {
            Resolved::Degraded(reason, detail) => {
                assert_eq!(reason, "invalid model");
                assert!(detail.contains("hash-0"), "{detail}");
            }
            Resolved::Ready(_) => panic!("zero dimensions cannot describe vectors.f32"),
        }
    }

    #[test]
    fn resolve_degrades_malformed_hash_names_as_invalid_models() {
        for model in ["hash-", "hash-abc", "hash-999999999999999999999"] {
            let config = config(&format!("[embedding]\nmodel = \"{model}\"\n"));
            match resolve(&config, &dirs("/tmp")) {
                Resolved::Degraded(reason, detail) => {
                    assert_eq!(reason, "invalid model");
                    assert!(detail.contains(model), "{detail}");
                }
                Resolved::Ready(_) => panic!("`{model}` is not a valid hash dimension"),
            }
        }
    }

    // ── the feature-on rows of §3.2's dispatch table ──────────────────────
    //
    // No test here loads a model: the fixtures are a directory of the right
    // file names and one of the wrong contents, so the assertions cover the
    // file-presence rule, the identity, and the degradation path. A `Ready`
    // with real weights needs the model itself and is exercised by the CI
    // feature job's `check`, not by a test with 90 MB of fixtures.

    /// Writes a model directory holding every file in `MODEL_FILES`.
    #[cfg(feature = "embeddings")]
    fn model_fixture(contents: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in fastembed::MODEL_FILES {
            std::fs::write(dir.path().join(name), contents).expect("write model file");
        }
        dir
    }

    #[test]
    #[cfg(feature = "embeddings")]
    fn the_model_identity_is_384_dimensional_and_l2() {
        // The `Ready` half of "model present" that needs no weights: the id a
        // load would record.
        let id = fastembed::id_for("AllMiniLML6V2");

        assert_eq!(id.name, "AllMiniLML6V2");
        assert_eq!(id.dim, 384);
        assert_eq!(id.dtype, "f32");
        assert_eq!(id.normalisation, "l2");
        assert_eq!(
            id.config_warning(),
            None,
            "a real model is not a test double"
        );
    }

    #[test]
    #[cfg(feature = "embeddings")]
    fn a_directory_with_every_model_file_is_found() {
        let vault = model_fixture(b"not a model");
        let dirs = ModelDirs {
            config: Some(vault.path().to_path_buf()),
            env: None,
            cache: PathBuf::from("/no/such/model/cache"),
        };

        assert_eq!(
            fastembed::model_dir(&dirs),
            Some(vault.path().to_path_buf())
        );
    }

    #[test]
    #[cfg(feature = "embeddings")]
    fn a_directory_missing_one_model_file_is_not_found() {
        let vault = model_fixture(b"not a model");
        std::fs::remove_file(vault.path().join("tokenizer.json")).expect("remove one file");
        let dirs = ModelDirs {
            config: None,
            env: None,
            cache: vault.path().to_path_buf(),
        };

        assert_eq!(fastembed::model_dir(&dirs), None);
    }

    #[test]
    #[cfg(feature = "embeddings")]
    fn the_configured_directory_wins_over_the_cache() {
        // V-25's precedence: config, then the environment, then the cache.
        let configured = model_fixture(b"not a model");
        let cached = model_fixture(b"not a model either");
        let dirs = ModelDirs {
            config: Some(configured.path().to_path_buf()),
            env: None,
            cache: cached.path().to_path_buf(),
        };

        assert_eq!(
            fastembed::model_dir(&dirs),
            Some(configured.path().to_path_buf())
        );
    }

    #[test]
    #[cfg(feature = "embeddings")]
    fn resolve_degrades_a_missing_model_and_names_the_directory() {
        let config = config("[embedding]\nmodel = \"AllMiniLML6V2\"\n");
        let missing = "/definitely/not/a/model/cache";

        match resolve(&config, &dirs(missing)) {
            Resolved::Degraded(reason, detail) => {
                assert_eq!(reason, "model not found");
                assert!(detail.contains(missing), "{detail}");
                assert!(detail.contains("AllMiniLML6V2"), "{detail}");
            }
            Resolved::Ready(_) => panic!("there is no model in {missing}"),
        }
    }

    #[test]
    #[cfg(feature = "embeddings")]
    fn resolve_degrades_present_but_unusable_model_files_with_the_path() {
        // The files exist, so the presence rule passes and the loader runs; the
        // contents cannot be a model, so the run degrades instead of failing.
        let vault = model_fixture(b"not a model");
        let config = config("[embedding]\nmodel = \"AllMiniLML6V2\"\n");
        let dirs = ModelDirs {
            config: Some(vault.path().to_path_buf()),
            env: None,
            cache: PathBuf::from("/no/such/model/cache"),
        };

        match resolve(&config, &dirs) {
            Resolved::Degraded(reason, detail) => {
                assert_eq!(reason, "model unusable");
                assert!(
                    detail.contains(&vault.path().display().to_string()),
                    "{detail}"
                );
            }
            Resolved::Ready(_) => panic!("five files of garbage cannot be a model"),
        }
    }

    #[test]
    #[cfg(feature = "embeddings")]
    fn the_feature_does_not_change_the_hash_rows_of_the_table() {
        // `hash-64` is compiled unconditionally and must stay `Ready` with the
        // feature on, or a featured build would lose the deterministic double.
        let config = config("[embedding]\nmodel = \"hash-64\"\n");
        let embedder = ready(resolve(&config, &dirs("/no/such/model/cache")));

        assert_eq!(embedder.id().name, "hash-64");
    }
}
