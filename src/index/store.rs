//! Snapshot persistence, the manifest, and load-time validation (§3.5, §8.3).
//!
//! Phase 4 writes plain files; phase 5 adds locking and publication. Loading
//! validates every id, range and offset before use (S-18).

use std::fs;
use std::path::Path;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::tokenize::{Stemmer, Tokenizer, TokenizerFingerprint};

use super::{ChunkMeta, ChunkingFingerprint, DocMeta, FORMAT_VERSION, LexicalIndex, Snapshot};

macro_rules! corrupt {
    ($($arg:tt)*) => {
        return Err(Corrupt::new(format!($($arg)*)))
    };
}

/// The human-readable index manifest (`.janus/manifest.json`, §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The snapshot layout version.
    pub format_version: u16,
    /// The frontmatter schema this build understands.
    pub schema_version: u16,
    /// The declared corpus identity (§6.11).
    pub corpus_id: String,
    /// The canonical root; diagnostic only.
    pub built_root: String,
    /// RFC 3339 UTC build time; diagnostic only.
    pub built_at: String,
    /// Number of documents.
    pub docs: usize,
    /// Number of chunk occurrences.
    pub chunks: usize,
    /// Number of terms.
    pub terms: usize,
    /// Number of postings.
    pub postings: usize,
    /// Mean chunk length in tokens.
    pub avgdl: f64,
    /// blake3 of `index.bin`, the generation id (S-17).
    pub index_sha: String,
    /// Byte length of `index.bin`.
    pub index_len: u64,
    /// The tokenizer that produced the postings.
    pub tokenizer: TokenizerFingerprint,
    /// The chunker parameters the index was built with.
    pub chunker: ChunkingFingerprint,
    /// The embedder id; `None` in M3.
    pub embedder: Option<String>,
}

impl Manifest {
    /// Assembles the manifest for an already-encoded snapshot (I-3).
    #[must_use]
    pub fn new(
        root: &Path,
        snapshot: &Snapshot,
        chunker: ChunkingFingerprint,
        index_sha: String,
        index_len: u64,
        built_at: &str,
    ) -> Self {
        let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Self {
            format_version: snapshot.format_version,
            schema_version: 1,
            corpus_id: snapshot.corpus_id.clone(),
            built_root: root.to_string_lossy().replace('\\', "/"),
            built_at: built_at.to_owned(),
            docs: snapshot.docs.len(),
            chunks: snapshot.chunks.len(),
            terms: snapshot.terms.len(),
            postings: snapshot.postings.len(),
            avgdl: snapshot.avgdl,
            index_sha,
            index_len,
            tokenizer: snapshot.tokenizer,
            chunker,
            embedder: None,
        }
    }
}

/// A snapshot that failed validation, decoding, or IO. A rebuild condition (S-18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corrupt {
    /// Human-readable cause, naming the offending field or path.
    pub reason: String,
}

impl Corrupt {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl Snapshot {
    /// Builds a snapshot around an in-memory index and freshly rebuilt metadata.
    #[must_use]
    pub fn from_index(
        index: &LexicalIndex,
        corpus_id: String,
        tokenizer: TokenizerFingerprint,
        docs: Vec<DocMeta>,
        chunks: Vec<ChunkMeta>,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            corpus_id,
            tokenizer,
            strings: index.pool().clone(),
            docs,
            chunks,
            terms: index.terms().to_vec(),
            postings: index.postings().to_vec(),
            avgdl: index.avgdl(),
        }
    }

    /// Rebuilds the index after validating every offset it uses.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when any id, range, index or `avgdl` is invalid.
    pub fn to_index(&self) -> Result<LexicalIndex, Corrupt> {
        self.validate()?;
        let lengths = self.chunks.iter().map(|chunk| chunk.tokens).collect();
        Ok(LexicalIndex::from_parts(
            self.strings.clone(),
            self.terms.clone(),
            self.postings.clone(),
            lengths,
            self.avgdl,
        ))
    }

    /// Encodes the snapshot after validating it.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when the snapshot is invalid or bincode cannot encode it.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Corrupt> {
        self.validate()?;
        bincode::serialize(self)
            .map_err(|error| Corrupt::new(format!("cannot encode index.bin: {error}")))
    }

    /// Decodes and validates a snapshot. A `format_version` mismatch is a rebuild.
    ///
    /// # Errors
    ///
    /// [`Corrupt`] when bincode rejects the bytes or validation fails.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Corrupt> {
        let snapshot: Self = bincode::deserialize(bytes)
            .map_err(|error| Corrupt::new(format!("cannot decode index.bin: {error}")))?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Checks every value read from disk before it is used (§3.5, S-18).
    ///
    /// # Errors
    ///
    /// [`Corrupt`] naming the first violation found.
    pub fn validate(&self) -> Result<(), Corrupt> {
        if self.format_version != FORMAT_VERSION {
            corrupt!("format_version {} != {FORMAT_VERSION}", self.format_version);
        }
        if !self.avgdl.is_finite() || self.avgdl < 0.0 {
            corrupt!("avgdl {} is invalid", self.avgdl);
        }
        let strings = self.strings.len();
        if self.terms.len() > strings {
            corrupt!(
                "term count {} exceeds string pool {strings}",
                self.terms.len()
            );
        }
        let mut previous = 0;
        for (index, term) in self.terms.iter().enumerate() {
            let (start, end) = (term.postings.0 as usize, term.postings.1 as usize);
            if start > end || end > self.postings.len() {
                corrupt!(
                    "term {index} postings {start}..{end} outside 0..{}",
                    self.postings.len()
                );
            }
            if start < previous {
                corrupt!("term {index} start {start} before {previous}");
            }
            if term.df as usize != end - start {
                corrupt!("term {index} df {} != {}", term.df, end - start);
            }
            previous = end;
        }
        let mut previous = 0;
        for (index, doc) in self.docs.iter().enumerate() {
            for (field, id) in [
                ("rel_path", doc.rel_path),
                ("title", doc.title),
                ("type_name", doc.type_name),
                ("scope", doc.scope),
            ] {
                if id.as_usize() >= strings {
                    corrupt!("doc {index} {field} StringId {} >= {strings}", id.as_u32());
                }
            }
            for id in doc.shared_with.iter().chain(&doc.tags) {
                if id.as_usize() >= strings {
                    corrupt!("doc {index} tag StringId {} >= {strings}", id.as_u32());
                }
            }
            let (start, end) = (doc.chunk_range.0 as usize, doc.chunk_range.1 as usize);
            if start > end || end > self.chunks.len() {
                corrupt!(
                    "doc {index} chunk_range {start}..{end} outside 0..{}",
                    self.chunks.len()
                );
            }
            if start < previous {
                corrupt!("doc {index} chunk_range start {start} before {previous}");
            }
            previous = end;
        }
        for (index, chunk) in self.chunks.iter().enumerate() {
            if chunk.doc as usize >= self.docs.len() {
                corrupt!("chunk {index} doc {} >= {}", chunk.doc, self.docs.len());
            }
            for id in &chunk.heading_path {
                if id.as_usize() >= strings {
                    corrupt!(
                        "chunk {index} heading StringId {} >= {strings}",
                        id.as_u32()
                    );
                }
            }
        }
        for (index, posting) in self.postings.iter().enumerate() {
            if posting.chunk as usize >= self.chunks.len() {
                corrupt!(
                    "posting {index} chunk {} >= {}",
                    posting.chunk,
                    self.chunks.len()
                );
            }
        }
        Ok(())
    }
}

/// Writes `index.bin` and then `manifest.json` as plain phase-4 files.
///
/// # Errors
///
/// [`Corrupt`] when encoding fails or either file cannot be written.
pub fn write_generation(
    dir: &Path,
    snapshot: &Snapshot,
    manifest: &Manifest,
) -> Result<(), Corrupt> {
    let bytes = snapshot.to_bytes()?;
    fs::create_dir_all(dir)
        .map_err(|error| Corrupt::new(format!("cannot create {}: {error}", dir.display())))?;
    // TODO(phase 5): replace with lock, unique temp files, fsync and renames.
    fs::write(dir.join("index.bin"), &bytes)
        .map_err(|error| Corrupt::new(format!("cannot write index.bin: {error}")))?;
    let manifest_json = serde_json::to_vec_pretty(manifest)
        .map_err(|error| Corrupt::new(format!("cannot encode manifest.json: {error}")))?;
    fs::write(dir.join("manifest.json"), manifest_json)
        .map_err(|error| Corrupt::new(format!("cannot write manifest.json: {error}")))?;
    Ok(())
}

/// The persisted fingerprint names only known algorithms and stemmers (I-1).
impl Serialize for TokenizerFingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (self.algorithm, "none").serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TokenizerFingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (algorithm, stemmer) = <(String, String)>::deserialize(deserializer)?;
        if algorithm != "v1" || stemmer != "none" {
            return Err(D::Error::custom(format!(
                "unknown tokenizer fingerprint {algorithm}/{stemmer}"
            )));
        }
        Ok(Self {
            algorithm: Tokenizer::new().algorithm(),
            stemmer: Stemmer::None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{BLAKE3_LEN, Blake3Hash, ChunkId, DOC_ID_LEN, DocId};
    use crate::index::{ChunkingFingerprint, StatusCode, StringId};
    use crate::tokenize::Tokenizer;
    use proptest::prelude::*;

    fn sample_snapshot() -> Snapshot {
        let index = LexicalIndex::build(&["alpha beta", "beta"]);
        let doc = DocMeta {
            id: DocId::from_bytes([1; DOC_ID_LEN]),
            rel_path: StringId(0),
            title: StringId(1),
            type_name: StringId(0),
            status: StatusCode::Active,
            scope: StringId(0),
            shared_with: Vec::new(),
            tags: Vec::new(),
            date: None,
            effective_from: None,
            timeless: false,
            content_hash: Blake3Hash::of_bytes(b"body"),
            lex_key: [0; 32],
            chunk_range: (0, 2),
        };
        let chunks = vec![
            ChunkMeta {
                id: ChunkId::from_bytes([1; BLAKE3_LEN]),
                doc: 0,
                span: (0, 5),
                heading_path: Vec::new(),
                tokens: 2,
            },
            ChunkMeta {
                id: ChunkId::from_bytes([2; BLAKE3_LEN]),
                doc: 0,
                span: (6, 10),
                heading_path: Vec::new(),
                tokens: 1,
            },
        ];
        Snapshot::from_index(
            &index,
            "main".to_owned(),
            Tokenizer::new().fingerprint(),
            vec![doc],
            chunks,
        )
    }

    fn reject(snapshot: &Snapshot, needle: &str) {
        let error = snapshot.validate().expect_err("snapshot must be rejected");
        assert!(
            error.reason.contains(needle),
            "reason {:?} did not name {needle:?}",
            error.reason
        );
    }

    #[test]
    fn snapshot_bytes_round_trip_and_keep_scores() {
        let snapshot = sample_snapshot();
        let before = snapshot.to_index().expect("sample is valid");
        let bytes = snapshot.to_bytes().expect("encode");
        let decoded = Snapshot::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, snapshot);

        let query = ["alpha", "beta"];
        assert_eq!(
            before.score_query(&query),
            decoded.to_index().expect("valid").score_query(&query)
        );
    }

    #[test]
    fn bad_string_id_is_rejected() {
        let mut snapshot = sample_snapshot();
        snapshot.docs[0].title = StringId(99);
        reject(&snapshot, "StringId 99");
    }

    #[test]
    fn inverted_postings_range_is_rejected() {
        let mut snapshot = sample_snapshot();
        snapshot.terms[0].postings = (1, 0);
        reject(&snapshot, "postings 1..0");
    }

    #[test]
    fn out_of_order_chunk_range_is_rejected() {
        let mut snapshot = sample_snapshot();
        let mut second = snapshot.docs[0].clone();
        second.id = DocId::from_bytes([2; DOC_ID_LEN]);
        second.chunk_range = (0, 1);
        snapshot.docs.push(second);
        reject(&snapshot, "chunk_range start 0 before 2");
    }

    #[test]
    fn non_finite_avgdl_is_rejected() {
        let mut snapshot = sample_snapshot();
        snapshot.avgdl = f64::NAN;
        reject(&snapshot, "avgdl NaN");
    }

    #[test]
    fn wrong_format_version_is_rejected() {
        let mut snapshot = sample_snapshot();
        snapshot.format_version = FORMAT_VERSION + 1;
        reject(&snapshot, "format_version");
    }

    #[test]
    fn manifest_is_human_readable_and_a_generation_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snapshot = sample_snapshot();
        let manifest = Manifest::new(
            dir.path(),
            &snapshot,
            ChunkingFingerprint {
                max_chars: 1800,
                overlap: 0.15,
            },
            blake3::hash(&snapshot.to_bytes().expect("encode"))
                .to_hex()
                .to_string(),
            snapshot.to_bytes().expect("encode").len() as u64,
            "2025-04-01T00:00:00Z",
        );
        let index_dir = dir.path().join(".janus");
        write_generation(&index_dir, &snapshot, &manifest).expect("write");

        let json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(index_dir.join("manifest.json")).expect("read manifest"),
        )
        .expect("manifest is JSON");
        assert_eq!(json["format_version"], FORMAT_VERSION);
        assert_eq!(json["docs"], 1);
        assert_eq!(json["index_sha"], manifest.index_sha);

        let read = fs::read(index_dir.join("index.bin")).expect("read index");
        assert_eq!(Snapshot::from_bytes(&read).expect("decode"), snapshot);
    }

    proptest! {
        #[test]
        fn arbitrary_term_content_survives_the_round_trip(
            chunks in prop::collection::vec(
                prop::collection::vec("[a-z]{1,8}", 1..5),
                1..6,
            )
        ) {
            let texts: Vec<String> = chunks
                .iter()
                .map(|words| words.join(" "))
                .collect();
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let index = LexicalIndex::build(&refs);
            let docs = vec![DocMeta {
                id: DocId::from_bytes([1; DOC_ID_LEN]),
                rel_path: StringId(0),
                title: StringId(0),
                type_name: StringId(0),
                status: StatusCode::Active,
                scope: StringId(0),
                shared_with: Vec::new(),
                tags: Vec::new(),
                date: None,
                effective_from: None,
                timeless: false,
                content_hash: Blake3Hash::of_bytes(b"body"),
                lex_key: [0; 32],
                chunk_range: (0, index.len() as u32),
            }];
            let chunks_meta = (0..index.len())
                .map(|chunk| ChunkMeta {
                    id: ChunkId::from_bytes([chunk as u8; BLAKE3_LEN]),
                    doc: 0,
                    span: (0, 1),
                    heading_path: Vec::new(),
                    tokens: index.lengths()[chunk],
                })
                .collect();
            let snapshot = Snapshot::from_index(
                &index,
                "main".to_owned(),
                Tokenizer::new().fingerprint(),
                docs,
                chunks_meta,
            );
            let bytes = snapshot.to_bytes().expect("encode valid snapshot");
            let decoded = Snapshot::from_bytes(&bytes).expect("decode valid snapshot");
            prop_assert_eq!(decoded, snapshot);
        }
    }
}
