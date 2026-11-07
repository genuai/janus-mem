//! The sync core behind every adapter: compute an answer, never print it (M6 §2.4).
//!
//! [`Engine::call`] answers one [`Call`] from one loaded generation. It writes
//! nothing to stdout and never exits: a failure is a [`Response`] with
//! `ok: false` and a stable code, and an adapter decides what to do with it. The
//! CLI, the rpc loop and the MCP server are thin adapters over this one method,
//! so no two surfaces can drift apart (§9.5).
//!
//! # The cache (M6-5)
//!
//! One process reads the payload once. Freshness is a fact of **every** call:
//! the walk and its digest are recomputed, so a stale corpus is never served
//! from the cache. The cache only spares the payload read.
//!
//! A cached load is reused when the manifest on disk equals the cached manifest
//! field by field **and** this call's walk digest equals `corpus_sig`. `index_sha`
//! alone is not enough: `vectors.f32` is published separately, so a vector-only
//! rewrite keeps `index_sha` and changes only `vectors_sha`.
//!
//! A declined refresh (a busy writer, `[index] refresh = "manual"`) and a
//! manifest that names another root are never cached, so the next call decides
//! again instead of repeating a stale answer forever.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Instant;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::api::dto::{FactsParams, LineageParams, ReadParams, SearchParams};
use crate::api::{ApiCode, Call, Failure, Tool};
use crate::clock::Clock;
use crate::config::{self, Config};
use crate::error::JanusError;
use crate::index::embed::Embedder;
use crate::index::refresh::Signature;
use crate::index::store::{self, Manifest};
use crate::lineage;
use crate::read;
use crate::search;
use crate::vault::{self, WalkOptions};

/// What one call returned: the envelope, and the code when it failed (M6 §2.3).
#[derive(Debug, Clone)]
pub struct Response {
    /// True when the call answered. An adapter branches on this field first.
    pub ok: bool,
    /// The whole envelope: the success object, or the failure object.
    pub json: Value,
    /// The stable failure code; `None` on a success (§2.2).
    pub code: Option<&'static str>,
}

/// The cache counters of this process (M6-19).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Calls answered from the cached load.
    pub hits: u64,
    /// Calls that read the payload from disk.
    pub misses: u64,
}

/// The generation this process loaded last, with the manifest it validated.
struct Cached {
    manifest: Manifest,
    prepared: search::Prepared,
}

/// One read-only surface over one corpus (M6 §2.4).
///
/// The engine is `Send + Sync`: it owns its clock, its embedder and one `RwLock`,
/// so two threads may call it at once. Case 4.3 is that test.
pub struct Engine {
    root: PathBuf,
    config: Config,
    clock: Box<dyn Clock>,
    embedder: Option<Box<dyn Embedder>>,
    cache: RwLock<Option<Arc<Cached>>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Engine {
    /// An engine over one corpus, with the clock and the embedder its caller
    /// resolved at the edge.
    #[must_use]
    pub fn new(
        root: PathBuf,
        config: Config,
        clock: Box<dyn Clock>,
        embedder: Option<Box<dyn Embedder>>,
    ) -> Self {
        Self {
            root,
            config,
            clock,
            embedder,
            cache: RwLock::new(None),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Answers one tool call. Never prints, never exits, never panics on a
    /// recoverable failure: the failure is the answer (§2.3, §9.5).
    #[must_use]
    pub fn call(&self, call: &Call) -> Response {
        match self.answer(call) {
            Ok(json) => Response {
                ok: true,
                json,
                code: None,
            },
            Err(error) => Response {
                ok: false,
                // The failure object serializes without borrowing anything, so a
                // failed encode can only mean the value is not encodable at all.
                json: serde_json::to_value(Failure::from_error(&error)).unwrap_or(Value::Null),
                code: Some(ApiCode::of(&error).code),
            },
        }
    }

    /// The cache counters of this process, for `janus_status` (M6-19).
    #[must_use]
    pub fn cache(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    /// The embedder, as the read path wants it.
    fn embedder(&self) -> Option<&dyn Embedder> {
        self.embedder.as_deref()
    }

    /// Answers one tool call, and returns the failure as an error.
    ///
    /// [`Engine::call`] wraps this in the response object. The CLI asks here
    /// instead, because it is the only surface that exits on a failure and the
    /// exit code belongs to the error (§9.1).
    pub fn answer(&self, call: &Call) -> Result<Value, JanusError> {
        let started = Instant::now();
        match call.tool {
            Tool::Search => {
                let options = search_options(&params(call)?)?;
                // A query the schema accepts but the engine rejects is a usage
                // failure, and it is decided before any read.
                search::check(&options)?;
                let loaded = self.load()?;
                let report = search::report(
                    &self.root,
                    &self.config,
                    self.clock.as_ref(),
                    self.embedder(),
                    &loaded.prepared,
                    &options,
                    started,
                )?;
                encode(&report)
            }
            Tool::AuthoritativeFacts => {
                let options = facts_options(&params(call)?);
                let loaded = self.load()?;
                let report = search::facts::compute(
                    &self.root,
                    &self.config,
                    self.clock.as_ref(),
                    self.embedder(),
                    &loaded.prepared,
                    &options,
                    started,
                )?;
                encode(&report)
            }
            Tool::Lineage => {
                let options = lineage_options(&params(call)?);
                // Exactly one of `path` and `topic`, decided before the read.
                lineage::validate(&options)?;
                let loaded = self.load()?;
                let report = lineage::compute(
                    &self.root,
                    &self.config,
                    self.clock.as_ref(),
                    self.embedder(),
                    &loaded.prepared,
                    &options,
                    started,
                )?;
                encode(&report)
            }
            Tool::ReadDocument => {
                let options = read_options(&params(call)?);
                // A whole-body read needs no generation, so it loads none (§3.3).
                let loaded = match options.chunk_id {
                    Some(_) => Some(self.load()?),
                    None => None,
                };
                let snapshot = loaded.as_ref().map(|load| &load.prepared.snapshot);
                let report = read::compute(&self.root, &self.config, &options, snapshot)?;
                encode(&report)
            }
        }
    }

    /// The cached load, when one is reusable for this call.
    fn cached(&self) -> Option<Arc<Cached>> {
        let cache = self.cache.read().unwrap_or_else(PoisonError::into_inner);
        cache.clone()
    }

    /// The generation to answer from: the cached one when it still describes
    /// this corpus, otherwise a fresh load (§2.4, M6-5).
    fn load(&self) -> Result<Arc<Cached>, JanusError> {
        let dir = config::resolve_index_dir(&self.root, &self.config.index_dir);
        // Freshness is decided by this call, never by the cache: one walk, one
        // digest, and a manifest read that costs no payload.
        let walk = vault::walk(&self.root, &self.config, WalkOptions::default())?;
        let digest = Signature::of(&walk.files).digest();
        if let Some(cached) = self.cached()
            && let Some(disk) = disk_manifest(&dir)
            && reusable(&cached.manifest, &disk, &digest)
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(cached);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        // One load serves every tool, so it asks for the rows any of them could
        // use: a lexical-only tool ignores what it did not need (M6-18).
        let want_vectors = self.config.embedding.alpha > 0.0 && self.embedder.is_some();
        let prepared = search::prepare(
            &self.root,
            &self.config,
            self.clock.as_ref(),
            self.embedder(),
            want_vectors,
            false,
            false,
        )?;
        let loaded = Arc::new(Cached {
            manifest: prepared.manifest.clone(),
            prepared,
        });
        if !loaded.prepared.stale && !loaded.prepared.repair_pending {
            let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
            *cache = Some(Arc::clone(&loaded));
        }
        Ok(loaded)
    }
}

/// True when the cached load still describes both the generation on disk and
/// the corpus this call walked (§2.4, M6-5, M6-17).
///
/// Every compared field is one fact the payload was validated against. A field
/// outside this list is either a diagnostic (`built_at`, `built_root`) or a
/// number derived from `index_sha` and `vectors_sha`.
fn reusable(cached: &Manifest, disk: &Manifest, digest: &str) -> bool {
    cached.format_version == disk.format_version
        && cached.schema_version == disk.schema_version
        && cached.corpus_id == disk.corpus_id
        && cached.index_sha == disk.index_sha
        && cached.vectors_sha == disk.vectors_sha
        && cached.tokenizer == disk.tokenizer
        && cached.chunker == disk.chunker
        && cached.embedder == disk.embedder
        && cached.corpus_sig == disk.corpus_sig
        && disk.corpus_sig.as_deref() == Some(digest)
}

/// The manifest on disk, or `None` when it is absent, torn or unreadable.
///
/// A manifest this cannot read is a miss, not an error: the load behind the
/// check answers the same question with the diagnostic the user needs.
fn disk_manifest(dir: &Path) -> Option<Manifest> {
    let bytes = fs::read(dir.join(store::MANIFEST_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Encodes one tool payload into the shared envelope.
fn encode(payload: impl Serialize) -> Result<Value, JanusError> {
    serde_json::to_value(crate::api::Ok::new(payload)).map_err(|error| {
        JanusError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cannot encode the response: {error}"),
        ))
    })
}

/// Deserializes one call's parameters into the shape its schema declares.
///
/// The structs deny unknown fields, so a parameter the schema does not list is a
/// usage failure and not a silent ignore (§2.5, case 7.6).
fn params<T: DeserializeOwned>(call: &Call) -> Result<T, JanusError> {
    serde_json::from_value(call.params.clone()).map_err(|error| JanusError::Usage {
        message: format!("{}: {error}", call.tool.wire_name()),
    })
}

/// One `search` call as the query engine's options.
fn search_options(params: &SearchParams) -> Result<search::Options, JanusError> {
    Ok(search::Options {
        query: params.query.clone(),
        intent: params.intent.clone(),
        scopes: params.scope.clone().unwrap_or_default(),
        scope_only: params.scope_only.unwrap_or_default(),
        limit: params.limit.unwrap_or(search::DEFAULT_LIMIT),
        types: params.types.clone().unwrap_or_default(),
        tags: params.tags.clone().unwrap_or_default(),
        decay_half_life: params.decay_half_life_days,
        include_superseded: params.include_superseded.unwrap_or_default(),
        // The CLI's own date parser, so rpc and MCP accept what the CLI accepts.
        as_of: params
            .as_of
            .as_deref()
            .map(search::parse_as_of)
            .transpose()?,
        no_rebuild: false,
        no_embeddings: false,
        embedding_reason: None,
        progress: false,
        json: false,
    })
}

/// One `get_authoritative_facts` call as the command's options.
fn facts_options(params: &FactsParams) -> search::facts::Options {
    search::facts::Options {
        query: params.query.clone(),
        tags: params.tags.clone().unwrap_or_default(),
        types: params.types.clone().unwrap_or_default(),
        scopes: params.scope.clone().unwrap_or_default(),
        scope_only: false,
        limit: params.limit.unwrap_or(search::DEFAULT_LIMIT),
        no_rebuild: false,
        no_embeddings: false,
        embedding_reason: None,
        progress: false,
        json: false,
    }
}

/// One `read_document` call as the read path's options.
fn read_options(params: &ReadParams) -> read::Options {
    read::Options {
        path: PathBuf::from(&params.path),
        max_chars: params.max_chars,
        chunk_id: params.chunk_id.clone(),
    }
}

/// One `get_lineage` call as the command's options.
fn lineage_options(params: &LineageParams) -> lineage::Options {
    lineage::Options {
        path: params.path.as_ref().map(PathBuf::from),
        topic: params.topic.clone(),
        json: false,
        no_rebuild: false,
        no_embeddings: false,
        progress: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{ChunkingFingerprint, embed::EmbedderId};
    use crate::tokenize::{Stemmer, TokenizerFingerprint};

    /// A manifest, with every compared field set to a value no other field uses.
    fn manifest() -> Manifest {
        Manifest {
            format_version: 6,
            schema_version: 1,
            corpus_id: "main".to_owned(),
            built_root: "/vault".to_owned(),
            built_at: "2026-01-01T00:00:00Z".to_owned(),
            docs: 3,
            chunks: 7,
            terms: 4,
            postings: 9,
            avgdl: 5.5,
            index_sha: "aa".to_owned(),
            index_len: 128,
            tokenizer: TokenizerFingerprint {
                algorithm: "v1",
                stemmer: Stemmer::None,
            },
            chunker: ChunkingFingerprint {
                max_chars: 1200,
                overlap: 0.5,
            },
            embedder: Some(EmbedderId {
                name: "hash-64".to_owned(),
                dim: 64,
                dtype: "f32".to_owned(),
                normalisation: "l2".to_owned(),
            }),
            vectors_sha: Some("bb".to_owned()),
            vectors_len: Some(256),
            corpus_sig: Some("cc".to_owned()),
        }
    }

    /// One changed manifest field: its name, and a change that names one fact.
    type Row = (&'static str, fn(&mut Manifest));

    /// The nine fields the key compares.
    fn compared() -> [Row; 9] {
        [
            ("index_sha", |m| m.index_sha = "00".to_owned()),
            ("vectors_sha", |m| m.vectors_sha = Some("00".to_owned())),
            ("corpus_sig", |m| m.corpus_sig = Some("00".to_owned())),
            ("format_version", |m| m.format_version = 99),
            ("schema_version", |m| m.schema_version = 99),
            ("corpus_id", |m| m.corpus_id = "other".to_owned()),
            ("tokenizer", |m| m.tokenizer.algorithm = "v9"),
            ("chunker", |m| m.chunker.max_chars = 1),
            ("embedder", |m| m.embedder = None),
        ]
    }

    #[test]
    fn case_4_6_one_differing_manifest_field_defeats_the_cache_key() {
        let base = manifest();
        let digest = "cc";
        assert!(
            reusable(&base, &base, digest),
            "an unchanged pair is reusable"
        );
        // The corpus this call walked is the second half of the key: a digest
        // that moved is a miss even when every byte of the manifest is equal.
        assert!(
            !reusable(&base, &base, "dd"),
            "another walk digest must not reuse the load"
        );

        for (field, change) in compared() {
            // Two directions: the cached side moved, and the on-disk side moved.
            let mut cached = base.clone();
            change(&mut cached);
            assert!(
                !reusable(&cached, &base, digest),
                "{field}: a cached manifest that differs must not be reusable"
            );
            let mut disk = base.clone();
            change(&mut disk);
            assert!(
                !reusable(&base, &disk, digest),
                "{field}: an on-disk manifest that differs must not be reusable"
            );
        }
    }

    #[test]
    fn case_4_6_every_manifest_field_is_compared_or_excused() {
        // A field this test does not name is a field the cache key cannot see,
        // so a new `Manifest` field must be added here or the test fails.
        const IGNORED: [&str; 9] = [
            "built_at",
            "built_root",
            "docs",
            "chunks",
            "terms",
            "postings",
            "avgdl",
            "index_len",
            "vectors_len",
        ];
        let value = serde_json::to_value(manifest()).expect("a manifest is JSON");
        let keys: std::collections::BTreeSet<&str> = value
            .as_object()
            .expect("the manifest serializes as an object")
            .keys()
            .map(String::as_str)
            .collect();
        let listed: std::collections::BTreeSet<&str> = compared()
            .iter()
            .map(|(field, _)| *field)
            .chain(IGNORED.iter().copied())
            .collect();
        assert_eq!(
            listed, keys,
            "a new Manifest field must be compared by the cache key or excused here"
        );
    }
}
