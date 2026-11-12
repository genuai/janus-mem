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
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::Instant;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::api::dto::{
    FactsParams, LineageParams, PromoteMode, PromoteParams, PromoteReceipt, ReadParams,
    SearchParams, StatusParams, SupersedeParams, SupersedeReceipt, UpdateParams, WriteMode,
    WriteParams,
};
use crate::api::{ApiCode, Call, Failure, Receipt, Tool, Writes};
use crate::clock::Clock;
use crate::config::{self, Config, ConfigWarning};
use crate::error::JanusError;
use crate::index::embed::Embedder;
use crate::index::refresh::Signature;
use crate::index::store::{self, Manifest};
use crate::lineage;
use crate::link::{self, Corpora};
use crate::read;
use crate::search;
use crate::search::federate::{self, Choice, Target};
use crate::status;
use crate::vault::{self, WalkOptions};
use crate::workspace::{Corpus, Role, Workspace};
use crate::write::{self, CommitError, CommitOptions, Document, NoHooks, Patch, PatchError, Plan};

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

impl Response {
    /// One computed answer, or the failure object that answered instead.
    ///
    /// The failure serializes without borrowing anything, so a failed encode can
    /// only mean the value is not encodable at all.
    fn of(result: Result<Value, JanusError>) -> Self {
        match result {
            Ok(json) => Self {
                ok: true,
                json,
                code: None,
            },
            Err(error) => Self {
                ok: false,
                json: serde_json::to_value(Failure::from_error(&error)).unwrap_or(Value::Null),
                code: Some(ApiCode::of(&error).code),
            },
        }
    }
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
    /// The workspace's name for this corpus, when a workspace named it. A
    /// refusal reports this name and the declared `corpus_id`, because they can
    /// differ and the caller typed the first one.
    name: Option<String>,
    /// What this surface may write here, and whether the corpus allows it
    /// (M7-15, M7-16). The only write switch.
    writes: Writes,
    /// Shared with every engine a [`Fleet`] builds, so two corpora of one query
    /// cannot disagree about today (§7.2).
    clock: Arc<dyn Clock>,
    embedder: Option<Box<dyn Embedder>>,
    /// Why the embedder is absent, when the caller resolved none (§2.6).
    degraded: Option<String>,
    cache: RwLock<Option<Arc<Cached>>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Engine {
    /// An engine over one corpus, with the clock and the embedder its caller
    /// resolved at the edge.
    ///
    /// Writes are off until [`Engine::with_writes`] says otherwise, so a caller
    /// that forgets the gate gets refusals and not a write.
    #[must_use]
    pub fn new(
        root: PathBuf,
        config: Config,
        clock: Box<dyn Clock>,
        embedder: Option<Box<dyn Embedder>>,
        degraded: Option<String>,
    ) -> Self {
        Self {
            root,
            config,
            name: None,
            writes: Writes::off("this engine"),
            clock: Arc::from(clock),
            embedder,
            degraded,
            cache: RwLock::new(None),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// The same engine, with the writes one surface of one corpus accepts.
    #[must_use]
    pub fn with_writes(mut self, writes: Writes) -> Self {
        self.writes = writes;
        self
    }

    /// What this engine may write, for the adapters' shared refusal.
    #[must_use]
    pub const fn writes(&self) -> Writes {
        self.writes
    }

    /// Answers one tool call. Never prints, never exits, never panics on a
    /// recoverable failure: the failure is the answer (§2.3, §9.5).
    #[must_use]
    pub fn call(&self, call: &Call) -> Response {
        Response::of(self.answer(call))
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
                let params: SearchParams = params(call)?;
                // One engine serves one corpus and holds one load cache, so a
                // selection that needs a workspace is refused here rather than
                // answered from the wrong corpus (M6b §2.3, M6b-12).
                selection(params.corpus.as_deref())?;
                let options = search_options(&params)?;
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
                let params: FactsParams = params(call)?;
                selection(params.corpus.as_deref())?;
                let options = facts_options(&params)?;
                let loaded = self.load()?;
                let listing = search::facts::compute(
                    &self.root,
                    &self.config,
                    self.clock.as_ref(),
                    self.embedder(),
                    &loaded.prepared,
                    &options,
                    started,
                )?;
                encode(&listing.report)
            }
            Tool::Lineage => {
                let options = lineage_options(&params(call)?);
                // Exactly one of `path` and `topic`, decided before the read.
                lineage::validate(&options)?;
                self.lineage(&options, &Corpora::none())
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
            Tool::Status => {
                // A status call reads the manifest and walks the corpus; it
                // builds nothing and takes no lock (M6-6). The `params` type is
                // empty, so an unknown parameter is a usage failure.
                let _: StatusParams = params(call)?;
                encode(self.status_report()?)
            }
            Tool::UpdateFrontmatter => {
                let params: UpdateParams = params(call)?;
                encode(self.update(&params)?)
            }
            Tool::WriteDocument => {
                let params: WriteParams = params(call)?;
                encode(self.write_document(&params)?)
            }
            Tool::SupersedeDocument => {
                let params: SupersedeParams = params(call)?;
                encode(self.supersede(&params)?)
            }
            // A promotion reads a source in another corpus, so one engine
            // cannot serve it: the fleet resolves the source and calls
            // [`Engine::promote`] (M7-37).
            Tool::PromoteDocument => Err(usage(
                "promote_document needs a workspace: the source it names lives in another corpus",
            )),
        }
    }

    /// One `get_lineage` answer, for a chain that may reach a corpus the query
    /// selected (M7-37).
    ///
    /// The graph is built here, from this generation and the reachable ones, so
    /// a qualified link is resolved on every query and a policy flip needs no
    /// rebuild (case 7.7).
    fn lineage(
        &self,
        options: &lineage::Options,
        corpora: &Corpora<'_>,
    ) -> Result<Value, JanusError> {
        let started = Instant::now();
        let loaded = self.load()?;
        let report = lineage::compute(
            &self.root,
            &self.config,
            self.clock.as_ref(),
            self.embedder(),
            &loaded.prepared,
            options,
            started,
            corpora,
        )?;
        encode(&report)
    }

    /// One `promote_document` call: a link into another corpus, or a copy of it
    /// (§6.11, M7-23).
    ///
    /// The gate runs first, so a read-only corpus answers before the source is
    /// touched, and the source is only ever read: the fleet resolved it in
    /// another corpus and refused the case where the two are one.
    fn promote(
        &self,
        params: &PromoteParams,
        source: &write::Source,
    ) -> Result<PromoteReceipt, JanusError> {
        let named = self.name.as_deref().unwrap_or(&self.config.corpus_id);
        if let Some(refusal) = self.writes.refusal(named, &self.config.corpus_id) {
            return Err(refusal);
        }
        let receipt = match params.mode {
            PromoteMode::Link => self.link_to(params, source)?,
            PromoteMode::Adopt => self.adopt(params, source)?,
        };
        Ok(PromoteReceipt {
            receipt,
            source_corpus: source.corpus.clone(),
            source: source.qualified(),
        })
    }

    /// The link half: patch the target's `supersedes` and nothing else (§3.7).
    ///
    /// The document is read before the patch is planned, because a link that
    /// would replace another one is refused (case 7.1). The patch then goes
    /// through the merged writer, so the etag, the lock and the receipt are the
    /// ones every other patch has.
    fn link_to(
        &self,
        params: &PromoteParams,
        source: &write::Source,
    ) -> Result<Receipt, JanusError> {
        let Some(given) = params.target_path.as_deref() else {
            return Err(usage(
                "mode link needs target_path: the document that gains the link",
            ));
        };
        let named = PathBuf::from(given);
        let found = target(&self.root, given)?;
        if !found.file_name().is_some_and(vault::is_markdown) {
            return Err(usage(format!(
                "{} is not Markdown; a write needs a .md or .markdown document",
                named.display()
            )));
        }
        let bytes = fs::read(&found).map_err(|error| JanusError::Write {
            path: named.clone(),
            source: error,
        })?;
        let text = String::from_utf8(bytes).map_err(|_| JanusError::DocumentNotInCorpus {
            path: named.clone(),
        })?;
        let qualified = source.qualified();
        write::check_link(&text, &self.config, &qualified).map_err(|error| match error {
            write::PromoteError::AlreadyLinked { current } => JanusError::PromoteLinked {
                path: named.clone(),
                current,
            },
        })?;
        let dry_run = params.dry_run.unwrap_or(false);
        let patch = Patch {
            path: named.clone(),
            keys: vec![("supersedes".to_owned(), Some(qualified))],
            expected_hash: params.expected_hash.clone(),
            create_frontmatter: false,
            dry_run,
        };
        self.apply(&named, &found, &text, patch, dry_run)
    }

    /// The adopt half: create a primary document that carries the source's text
    /// (case 7.2).
    ///
    /// The create path is the one `write_document` uses, so an existing target is
    /// `path_exists` and the receipt describes the whole file (M7-27).
    fn adopt(&self, params: &PromoteParams, source: &write::Source) -> Result<Receipt, JanusError> {
        if params.expected_hash.is_some() {
            return Err(usage(
                "mode adopt creates a document, so expected_hash has nothing to guard",
            ));
        }
        // A copy is written where the caller says, and otherwise at the path the
        // source holds. That default makes the two documents easy to pair by
        // eye, and a path that is taken is refused like any other create.
        let given = params
            .target_path
            .clone()
            .unwrap_or_else(|| source.rel_path.clone());
        self.write_document(&WriteParams {
            path: given,
            frontmatter: write::adopted(source),
            body: source.body.clone(),
            mode: WriteMode::Create,
            expected_hash: None,
            dry_run: params.dry_run,
        })
    }

    /// One `update_frontmatter` call: the gate, then plan, then commit (§9.3).
    ///
    /// The order is the contract. The gate runs first, so no surface reaches a
    /// file without it (M7-15, M7-36). The etag decides next, so a refused write
    /// touches nothing (cases 2.1, 2.2). Plan and commit are the merged writer,
    /// so this function adds no second splice (M7-11).
    fn update(&self, params: &UpdateParams) -> Result<Receipt, JanusError> {
        let named = self.name.as_deref().unwrap_or(&self.config.corpus_id);
        if let Some(refusal) = self.writes.refusal(named, &self.config.corpus_id) {
            return Err(refusal);
        }
        // `named` is the caller's path, for a message a reader can act on;
        // `found` is the canonical file, for the read, the lock and the rename.
        let named = PathBuf::from(&params.path);
        let found = target(&self.root, &params.path)?;
        // A write patches Markdown frontmatter, and a caller can compute an
        // etag for any file, so the extension is what keeps a write on a
        // document. The walk admits a document by the same rule, which is why a
        // file `[index] exclude` keeps out of search stays writable: it is still
        // a document the user may edit (M7-52).
        if !found.file_name().is_some_and(vault::is_markdown) {
            return Err(usage(format!(
                "{} is not Markdown; a write needs a .md or .markdown document",
                named.display()
            )));
        }
        let bytes = fs::read(&found).map_err(|source| JanusError::Write {
            path: named.clone(),
            source,
        })?;
        // A write never invents bytes: a file that is not UTF-8 is not a
        // document this corpus can patch.
        let text = String::from_utf8(bytes).map_err(|_| JanusError::DocumentNotInCorpus {
            path: named.clone(),
        })?;
        let dry_run = params.dry_run.unwrap_or(false);
        let patch = Patch {
            path: named.clone(),
            keys: params.keys(),
            expected_hash: params.expected_hash.clone(),
            create_frontmatter: false,
            dry_run,
        };
        self.apply(&named, &found, &text, patch, dry_run)
    }

    /// The half every patch shares: plan the bytes, check the etag, commit.
    ///
    /// `text` is the bytes the caller read, so a promote that must inspect the
    /// document before it patches it reads them once (M7-11).
    fn apply(
        &self,
        named: &Path,
        found: &Path,
        text: &str,
        patch: Patch,
        dry_run: bool,
    ) -> Result<Receipt, JanusError> {
        let expected_hash = patch.expected_hash.clone();
        let plan = write::plan(text, &patch).map_err(|error| patch_refusal(named, error))?;
        check_etag(
            named,
            expected_hash.as_deref(),
            Some(&plan.old_hash),
            &plan.changed_keys,
            dry_run,
        )?;
        let receipt = Receipt {
            path: relative(&self.root, found),
            changed_keys: plan.changed_keys.clone(),
            appended_keys: plan.appended_keys.clone(),
            old_hash: Some(plan.old_hash.clone()),
            new_hash: plan.new_hash.clone(),
            dry_run,
            bytes_before: plan.bytes_before,
            bytes_after: plan.bytes_after,
        };
        if dry_run {
            return Ok(receipt);
        }
        self.commit(found, named, &plan)?;
        Ok(receipt)
    }

    /// One `write_document` call: the gate, then the whole file, then a receipt
    /// (§9.3, M7-5).
    ///
    /// A create never renames: [`write::create_document`] opens the target with
    /// `create_new`, so an existing file answers `path_exists` and keeps its
    /// bytes (M7-27). An overwrite shares the checked commit the patch uses, so
    /// it verifies the etag twice and holds the corpus lock.
    fn write_document(&self, params: &WriteParams) -> Result<Receipt, JanusError> {
        let named = self.name.as_deref().unwrap_or(&self.config.corpus_id);
        if let Some(refusal) = self.writes.refusal(named, &self.config.corpus_id) {
            return Err(refusal);
        }
        let named = PathBuf::from(&params.path);
        // The extension is the corpus's own admission rule, and a create has no
        // file to resolve yet, so it is checked before either mode resolves one
        // (M7-52).
        if !named.file_name().is_some_and(vault::is_markdown) {
            return Err(usage(format!(
                "{} is not Markdown; a write needs a .md or .markdown document",
                named.display()
            )));
        }
        let dry_run = params.dry_run.unwrap_or(false);
        match params.mode {
            WriteMode::Create => self.create_document(&named, params, dry_run),
            WriteMode::Overwrite => self.overwrite_document(&named, params, dry_run),
        }
    }

    /// The create half: a resolved new path, the whole file, and a receipt.
    fn create_document(
        &self,
        named: &Path,
        params: &WriteParams,
        dry_run: bool,
    ) -> Result<Receipt, JanusError> {
        let found = write::document_target(&self.root, named)
            .map_err(|error| commit_refusal(named, error))?;
        let document = write::plan_document(&params.frontmatter, &params.body, None)
            .map_err(|error| patch_refusal(named, error))?;
        let receipt = document_receipt(&self.root, &found, &document, dry_run);
        // A preview of a request that cannot succeed is misleading, so an
        // existing path is refused in both modes (case 5.2).
        if dry_run {
            if fs::symlink_metadata(&found).is_ok() {
                return Err(JanusError::PathExists {
                    path: named.to_path_buf(),
                });
            }
            return Ok(receipt);
        }
        let options = self.options(&found);
        write::create_document(&found, &document, &options).map_err(|error| {
            if let CommitError::Exists { .. } = error {
                JanusError::PathExists {
                    path: named.to_path_buf(),
                }
            } else {
                commit_refusal(named, error)
            }
        })?;
        Ok(receipt)
    }

    /// The overwrite half: the old bytes, the whole file, and a receipt.
    fn overwrite_document(
        &self,
        named: &Path,
        params: &WriteParams,
        dry_run: bool,
    ) -> Result<Receipt, JanusError> {
        let found = target(&self.root, &params.path)?;
        let bytes = fs::read(&found).map_err(|source| JanusError::Write {
            path: named.to_path_buf(),
            source,
        })?;
        let document = write::plan_document(&params.frontmatter, &params.body, Some(&bytes))
            .map_err(|error| patch_refusal(named, error))?;
        check_etag(
            named,
            params.expected_hash.as_deref(),
            document.old_hash.as_deref(),
            &document.changed_keys,
            dry_run,
        )?;
        let receipt = document_receipt(&self.root, &found, &document, dry_run);
        if dry_run {
            return Ok(receipt);
        }
        let options = self.options(&found);
        write::overwrite_document(&document, &options)
            .map_err(|error| commit_refusal(named, error))?;
        Ok(receipt)
    }

    /// One `supersede_document` call: the gate, the cycle check, then the
    /// two-file transaction (§7.4, M7-6).
    ///
    /// Both documents are planned before either is staged, so a refused patch
    /// writes neither (case 6.3). The cycle check reads frontmatter on disk, so
    /// it runs on a corpus that was never indexed (M7-34). An existing
    /// `new_path` is refused before anything is staged (M7-35).
    fn supersede(&self, params: &SupersedeParams) -> Result<SupersedeReceipt, JanusError> {
        let named = self.name.as_deref().unwrap_or(&self.config.corpus_id);
        if let Some(refusal) = self.writes.refusal(named, &self.config.corpus_id) {
            return Err(refusal);
        }
        let old_named = PathBuf::from(&params.old_path);
        let new_named = PathBuf::from(&params.new_path);
        for path in [&old_named, &new_named] {
            if !path.file_name().is_some_and(vault::is_markdown) {
                return Err(usage(format!(
                    "{} is not Markdown; a write needs a .md or .markdown document",
                    path.display()
                )));
            }
        }

        let old_found = target(&self.root, &params.old_path)?;
        let new_found = write::document_target(&self.root, &new_named)
            .map_err(|error| commit_refusal(&new_named, error))?;
        if fs::symlink_metadata(&new_found).is_ok() {
            return Err(JanusError::PathExists { path: new_named });
        }
        let old_rel = relative(&self.root, &old_found);
        let new_rel = relative(&self.root, &new_found);
        let dry_run = params.dry_run.unwrap_or(false);
        let append_only = params.append_only.unwrap_or(false);

        // The check reads the files, not the index: a write never refreshes the
        // index, so a snapshot would be the one this call invalidates (M7-17).
        let chain = write::Chain::read(&self.root, &self.config, &old_rel);
        if let Some(cycle) = chain.cycle(&old_rel, &new_rel) {
            return Err(JanusError::SupersessionCycle {
                cycle: cycle.join(" -> "),
            });
        }

        let old_bytes = fs::read(&old_found).map_err(|source| JanusError::Write {
            path: old_named.clone(),
            source,
        })?;
        let old_text =
            String::from_utf8(old_bytes).map_err(|_| JanusError::DocumentNotInCorpus {
                path: old_named.clone(),
            })?;
        let old_plan = (!append_only)
            .then(|| {
                let patch = Patch {
                    path: old_named.clone(),
                    keys: vec![
                        ("status".to_owned(), Some("superseded".to_owned())),
                        ("superseded_by".to_owned(), Some(new_rel.clone())),
                    ],
                    expected_hash: params.expected_hash.clone(),
                    create_frontmatter: false,
                    dry_run,
                };
                write::plan(&old_text, &patch).map_err(|error| patch_refusal(&old_named, error))
            })
            .transpose()?;

        let mut frontmatter: std::collections::BTreeMap<String, Option<String>> =
            std::collections::BTreeMap::new();
        if let Some(title) = &params.new_title {
            frontmatter.insert("title".to_owned(), Some(title.clone()));
        }
        frontmatter.insert("supersedes".to_owned(), Some(old_rel.clone()));
        let new_document = write::plan_document(&frontmatter, "", None)
            .map_err(|error| patch_refusal(&new_named, error))?;

        let old_hash = old_plan.as_ref().map_or_else(
            || crate::id::Blake3Hash::of_bytes(old_text.as_bytes()).to_hex(),
            |plan| plan.old_hash.clone(),
        );
        let old_keys = old_plan
            .as_ref()
            .map_or(&[][..], |plan| plan.changed_keys.as_slice());
        check_etag(
            &old_named,
            params.expected_hash.as_deref(),
            Some(&old_hash),
            old_keys,
            dry_run,
        )?;

        let new_receipt = document_receipt(&self.root, &new_found, &new_document, dry_run);
        let old_receipt = old_plan.as_ref().map(|plan| Receipt {
            path: old_rel.clone(),
            changed_keys: plan.changed_keys.clone(),
            appended_keys: plan.appended_keys.clone(),
            old_hash: Some(plan.old_hash.clone()),
            new_hash: plan.new_hash.clone(),
            dry_run,
            bytes_before: plan.bytes_before,
            bytes_after: plan.bytes_after,
        });
        let (primary, receipts) = match old_receipt {
            Some(receipt) => (receipt.clone(), vec![receipt, new_receipt.clone()]),
            None => (new_receipt.clone(), vec![new_receipt]),
        };
        let receipt = SupersedeReceipt {
            receipt: primary,
            receipts,
            chain: chain.ordered(&old_rel, &new_rel),
        };
        if dry_run {
            return Ok(receipt);
        }

        let options = self.options(&old_found);
        if let Some(plan) = &old_plan {
            write::commit_pair(plan, &new_found, new_document.text.as_bytes(), &options)
                .map_err(|error| commit_refusal(&old_named, error))?;
        } else {
            write::create_document(&new_found, &new_document, &options)
                .map_err(|error| commit_refusal(&new_named, error))?;
        }
        Ok(receipt)
    }

    /// The options every write of this corpus shares.
    fn options<'a>(&'a self, found: &Path) -> CommitOptions<'a> {
        CommitOptions {
            path: found.to_path_buf(),
            root: self.root.clone(),
            config: &self.config,
            timeout: store::LOCK_TIMEOUT,
            hooks: &NoHooks,
        }
    }

    /// Replaces the document under the corpus lock, or refuses (M7-3, M7-9).
    fn commit(&self, path: &Path, named: &Path, plan: &Plan) -> Result<(), JanusError> {
        write::commit(plan, &self.options(path))
            .map(|_| ())
            .map_err(|error| commit_refusal(named, error))
    }

    /// One status answer, unrendered: the same value the `status` arm encodes.
    fn status_report(&self) -> Result<status::Report, JanusError> {
        status::compute(
            &self.root,
            &self.config,
            self.writes.enabled(),
            self.embedder.is_some(),
            self.degraded.clone(),
            self.cache(),
        )
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

/// The corpora one surface answers from (M6b §3.3, §3.5).
///
/// No workspace resolved means one corpus, and every answer is the one this
/// crate gave before federation existed. A workspace means every corpus it
/// names, so a `corpus` parameter can name any of them on the command line, over
/// rpc and over MCP alike (§9.5).
///
/// One [`Engine`] per corpus, built on first use: a query that reads the primary
/// only must not resolve an embedding model for an archived corpus it left out.
pub struct Fleet {
    /// The corpora in workspace order; exactly one in single-corpus mode.
    members: Vec<Member>,
    /// The workspace these corpora came from, when one named them.
    workspace: Option<Workspace>,
    /// The clock every engine of this fleet shares, so two corpora of one query
    /// cannot disagree about today (§7.2).
    clock: Arc<dyn Clock>,
    /// What this surface may write, without a corpus: the switch every write
    /// check reads (M7-15, M7-36). Each member adds its corpus's `read_only`.
    writes: Writes,
}

/// One corpus a query read, with the generation a qualified link resolves
/// against (M7-37).
///
/// The load is kept alive beside the two borrowed facts, because the graph reads
/// the snapshot inside it and a link must not reopen a corpus.
struct Reached<'a> {
    /// The workspace's id for this corpus; the qualifier a link names.
    id: &'a str,
    /// The config the corpus's own rows are read with (§6.11).
    config: &'a Config,
    /// The generation this query already holds.
    loaded: Arc<Cached>,
}

/// How one corpus resolves its embedder, as the edge decided it.
type Resolver = Arc<dyn Fn(&Config) -> (Option<Box<dyn Embedder>>, Option<String>) + Send + Sync>;

/// One corpus a fleet can answer from.
struct Member {
    /// The workspace's name for this corpus; `None` in single-corpus mode.
    id: Option<String>,
    root: PathBuf,
    config: Config,
    /// The writes this surface may make to this corpus (M7-16).
    writes: Writes,
    /// The clock and the embedder resolver every engine here shares.
    clock: Arc<dyn Clock>,
    resolve: Resolver,
    engine: OnceLock<Engine>,
}

impl Member {
    /// The engine over this corpus, built on first use.
    ///
    /// The embedder is resolved here rather than at construction, so a query
    /// that reads one corpus pays for one corpus.
    fn engine(&self) -> &Engine {
        self.engine.get_or_init(|| {
            let (embedder, degraded) = (self.resolve)(&self.config);
            Engine {
                root: self.root.clone(),
                config: self.config.clone(),
                name: self.id.clone(),
                writes: self.writes,
                clock: Arc::clone(&self.clock),
                embedder,
                degraded,
                cache: RwLock::new(None),
                hits: AtomicU64::new(0),
                misses: AtomicU64::new(0),
            }
        })
    }
}

impl Fleet {
    /// One corpus, with the engine its caller already built.
    ///
    /// The engine already carries the writes its surface accepts, so the fleet
    /// copies them rather than asking for a second switch.
    #[must_use]
    pub fn single(engine: Engine) -> Self {
        let writes = engine.writes();
        Self {
            clock: Arc::clone(&engine.clock),
            members: vec![Member {
                id: engine.name.clone(),
                root: engine.root.clone(),
                config: engine.config.clone(),
                writes,
                clock: Arc::clone(&engine.clock),
                resolve: Arc::new(|_| (None, None)),
                engine: OnceLock::from(engine),
            }],
            workspace: None,
            writes,
        }
    }

    /// Every corpus one workspace names (M6b-1).
    ///
    /// The workspace owns each corpus's index location (M6b-11), and the plan
    /// owns the weight of an archived corpus, so both are read from one place.
    /// `writes` is the surface's answer to `--allow-write`; each member adds its
    /// corpus's `read_only`, so a read-only corpus refuses every write whatever
    /// the flag says (M7-16, case 2.7a).
    ///
    /// # Errors
    ///
    /// A corpus `janus.toml` that cannot be loaded.
    pub fn of(
        found: &Workspace,
        clock: Arc<dyn Clock>,
        resolve: Resolver,
        writes: Writes,
    ) -> Result<Self, JanusError> {
        let plan = federate::plan(found, &Choice::All)?;
        let mut members = Vec::with_capacity(plan.corpora.len());
        for (corpus, _factor) in &plan.corpora {
            // The workspace's own prunes come with the config, so every engine
            // walks what its corpus owns and nothing else (M6b-26).
            let mut config = found.config_of(corpus)?;
            // A workspace's `index_dir` is the index location of a corpus it
            // names; the corpus config's own value applies in single-corpus
            // mode, where no workspace exists (M6b-11).
            config.index_dir = corpus.index_dir.clone();
            members.push(Member {
                id: Some(corpus.id.clone()),
                root: corpus.root.clone(),
                config,
                writes: Writes::new(writes.surface, writes.allowed, corpus.read_only),
                clock: Arc::clone(&clock),
                resolve: Arc::clone(&resolve),
                engine: OnceLock::new(),
            });
        }
        // The fleet's own value describes the primary, because that is the
        // corpus an unqualified write names and the one `tools/list` describes.
        // A capability the list advertises must be one that corpus accepts, or
        // every call to the advertised tool answers `read_only_corpus` (M7-16,
        // M7-49).
        let writes = found
            .corpora
            .iter()
            .find(|corpus| corpus.role == Role::Primary)
            .map_or(writes, |corpus| {
                Writes::new(writes.surface, writes.allowed, corpus.read_only)
            });
        Ok(Self {
            members,
            workspace: Some(found.clone()),
            clock,
            writes,
        })
    }

    /// What this surface may write, for [`crate::api::write_refusal`].
    #[must_use]
    pub const fn writes(&self) -> Writes {
        self.writes
    }

    /// Every config warning the corpora of this fleet carry, in corpus order.
    ///
    /// `janus mcp` prints these once at startup, because a server has no other
    /// moment to tell a client about its own config (case 4.6).
    #[must_use]
    pub fn config_warnings(&self) -> Vec<&ConfigWarning> {
        self.members
            .iter()
            .flat_map(|member| member.config.warnings.iter())
            .collect()
    }

    /// The corpora one selection reads, and the note about the ones it left out.
    ///
    /// # Errors
    ///
    /// [`JanusError::Usage`] when the selection names no corpus of this
    /// workspace; the message lists the ids (§2.3).
    pub fn scope(&self, choice: &Choice) -> Result<(Vec<Target<'_>>, Vec<String>), JanusError> {
        let found = self.found()?;
        let plan = federate::plan(found, choice)?;
        let notes: Vec<String> = plan.excluded_note().into_iter().collect();
        let mut targets = Vec::with_capacity(plan.corpora.len());
        for (corpus, factor) in &plan.corpora {
            let engine = self.engine_of(&corpus.id)?;
            targets.push(Target {
                id: corpus.id.as_str(),
                root: &engine.root,
                config: &engine.config,
                embedder: engine.embedder.as_deref(),
                embedding_reason: engine.degraded.as_deref(),
                factor: *factor,
            });
        }
        Ok((targets, notes))
    }

    /// Answers one tool call. Never prints and never exits: the failure is the
    /// answer (§2.3, §9.5).
    #[must_use]
    pub fn call(&self, call: &Call) -> Response {
        Response::of(self.answer(call))
    }

    /// Answers one tool call, and returns the failure as an error.
    ///
    /// Every surface asks here, so the `corpus` parameter means one thing and
    /// the answers are one answer (§9.5).
    pub fn answer(&self, call: &Call) -> Result<Value, JanusError> {
        let mut fields = call.params.clone();
        let choice = self.selection(call.tool, &mut fields)?;
        let Some(found) = &self.workspace else {
            // One corpus answers exactly as it always has: no `corpora`, no
            // per-row `corpus`, no weight (M6b §3.5).
            if call.tool.names_a_path() {
                self.single_selection(&choice)?;
            }
            return self.members[0]
                .engine()
                .answer(&Call::new(call.tool, fields));
        };
        match call.tool {
            Tool::Search => {
                let options = search_options(&params::<SearchParams>(call)?)?;
                let (targets, notes) = self.scope(&choice)?;
                encode(federate::report_search(
                    &targets,
                    self.clock.as_ref(),
                    options,
                    notes,
                )?)
            }
            Tool::AuthoritativeFacts => {
                let options = facts_options(&params::<FactsParams>(call)?)?;
                let (targets, notes) = self.scope(&choice)?;
                encode(federate::report_facts(
                    &targets,
                    self.clock.as_ref(),
                    &options,
                    notes,
                )?)
            }
            Tool::ReadDocument => {
                let member = self.one(found, &choice)?;
                let mut answer = member.engine().answer(&Call::new(call.tool, fields))?;
                // The one place a read names its corpus (case 3.1). Absent in
                // single-corpus mode, where there is only one to name.
                if let Some(fields) = answer.as_object_mut() {
                    fields.insert("corpus".to_owned(), Value::String(member.name().to_owned()));
                }
                Ok(answer)
            }
            Tool::Status => {
                let member = self.one(found, &choice)?;
                encode(member.engine().status_report()?.in_workspace(found))
            }
            // A write names one corpus's file, so it answers for one corpus,
            // and the qualifier or `corpus` parameter decides which (M7-38).
            Tool::UpdateFrontmatter | Tool::WriteDocument | Tool::SupersedeDocument => {
                let member = self.one(found, &choice)?;
                member.engine().answer(&Call::new(call.tool, fields))
            }
            // A promotion writes in the corpus the call names and reads a
            // source in another one, so the fleet resolves the source and
            // refuses a source the target corpus holds (M7-23, M7-37).
            Tool::PromoteDocument => {
                let params: PromoteParams = params(&Call::new(call.tool, fields))?;
                let member = self.one(found, &choice)?;
                let source = self.source(found, member, &params.source)?;
                encode(member.engine().promote(&params, &source)?)
            }
            // A supersession chain is one corpus's graph, and it may reach the
            // corpora this query read: a promoted link is readable here or
            // nowhere (M7-37, case 7.6).
            Tool::Lineage => {
                let options = lineage_options(&params(call)?);
                lineage::validate(&options)?;
                // A chain is read in one corpus and may reach others. `all`
                // names the reach: the chain stays in the primary, which owns an
                // unqualified path, and a qualified path selects its corpus
                // through `choice` already (M7-37, case 7.6).
                let member = self.one(
                    found,
                    if choice == Choice::All {
                        &Choice::Primary
                    } else {
                        &choice
                    },
                )?;
                let loads = self.reach(found, &choice)?;
                let selected: Vec<link::Corpus<'_>> = loads
                    .iter()
                    .map(|found| link::Corpus {
                        id: found.id,
                        snapshot: &found.loaded.prepared.snapshot,
                        config: found.config,
                    })
                    .collect();
                let known: Vec<&str> = found
                    .corpora
                    .iter()
                    .map(|corpus| corpus.id.as_str())
                    .collect();
                let corpora = Corpora {
                    allowed: found.search.allow_cross_corpus_links,
                    selected,
                    known,
                };
                member.engine().lineage(&options, &corpora)
            }
        }
    }

    /// The selection one call asks for, and its parameters with any
    /// `corpus:path` qualifier removed (§2.3, M6b-6).
    ///
    /// A qualifier names a corpus the same way `corpus` does, so one call cannot
    /// ask for two.
    fn selection(&self, tool: Tool, params: &mut Value) -> Result<Choice, JanusError> {
        let named = params
            .get("corpus")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if !tool.names_a_path() {
            return Ok(Choice::parse(named.as_deref()));
        }
        // A write names its corpus on the file it writes: `path` for the tools
        // that patch one document, `target_path` for a promotion (M7-38).
        let field = if tool == Tool::PromoteDocument {
            "target_path"
        } else {
            "path"
        };
        let path = params
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some((head, rest)) = link::split_qualifier(&path) else {
            return Ok(Choice::parse(named.as_deref()));
        };
        if let Some(named) = named
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            && named != head
        {
            return Err(usage(format!(
                "the path names corpus {head:?} and `corpus` names {named:?}; pass one"
            )));
        }
        params[field] = Value::String(rest.to_owned());
        Ok(Choice::Id(head.to_owned()))
    }

    /// Refuses a selection that a single corpus cannot serve.
    ///
    /// A read that serves one corpus answers for it and for nothing else. The
    /// message names the id this read serves, so a misspelt name cannot look
    /// like a name that was silently ignored (M6b-12, case 3.2).
    fn single_selection(&self, choice: &Choice) -> Result<(), JanusError> {
        let declared = &self.members[0].config.corpus_id;
        match choice {
            Choice::Primary => Ok(()),
            Choice::Id(id) if id == declared => Ok(()),
            Choice::All => Err(usage(
                "corpus \"all\" needs a workspace; this read serves one corpus",
            )),
            Choice::Id(id) => Err(usage(format!(
                "unknown corpus {id:?}; this read serves {declared:?} and no workspace names another"
            ))),
        }
    }

    /// One promotion's source: the corpus its qualifier names, and the document
    /// it resolves to there (§6.11, M7-23).
    ///
    /// The source is read and never written. A target in the source corpus would
    /// be a write to the archive, so it is refused as a read-only corpus before
    /// a byte is read: the roles of the workspace cannot grant what the archive
    /// must never allow (case 7.3).
    fn source(
        &self,
        found: &Workspace,
        member: &Member,
        text: &str,
    ) -> Result<write::Source, JanusError> {
        let text = text.trim();
        let Some((corpus_id, path)) = link::split_qualifier(text) else {
            return Err(usage(format!(
                "`{text}` names no corpus; a promote source is corpus:path"
            )));
        };
        let corpus: &Corpus =
            found
                .corpus(corpus_id)
                .ok_or_else(|| JanusError::CorpusNotInWorkspace {
                    corpus: corpus_id.to_owned(),
                    known: found
                        .corpora
                        .iter()
                        .map(|corpus| corpus.id.clone())
                        .collect(),
                })?;
        if corpus.id == member.name() {
            return Err(JanusError::ReadOnlyCorpus {
                corpus: corpus.id.clone(),
            });
        }
        let named = PathBuf::from(path);
        if !named.file_name().is_some_and(vault::is_markdown) {
            return Err(usage(format!(
                "{text} is not Markdown; a promotion needs a .md or .markdown document"
            )));
        }
        let config = found.config_of(corpus)?;
        let file = target(&corpus.root, path)?;
        let bytes = fs::read(&file).map_err(|error| JanusError::Write {
            path: named.clone(),
            source: error,
        })?;
        let text = String::from_utf8(bytes).map_err(|_| JanusError::DocumentNotInCorpus {
            path: named.clone(),
        })?;
        let read = vault::frontmatter::read(&text, &config);
        Ok(write::Source {
            corpus: corpus.id.clone(),
            rel_path: relative(&corpus.root, &file),
            body: read.body.to_owned(),
            title: read.frontmatter.title.clone(),
        })
    }

    /// The generation of every corpus this query reads, with the workspace id
    /// and the config a qualified link needs (M7-37).
    ///
    /// The plan owns the selection, so a corpus the query excluded is not
    /// loaded: a read never opens an index the user left out (case 7.8).
    fn reach(&self, found: &Workspace, choice: &Choice) -> Result<Vec<Reached<'_>>, JanusError> {
        let plan = federate::plan(found, choice)?;
        let mut out = Vec::with_capacity(plan.corpora.len());
        for (corpus, _factor) in &plan.corpora {
            let member = self
                .members
                .iter()
                .find(|member| member.id.as_deref() == Some(corpus.id.as_str()))
                .ok_or_else(|| usage(format!("corpus {:?} is not loaded", corpus.id)))?;
            out.push(Reached {
                id: member.name(),
                config: &member.config,
                loaded: member.engine().load()?,
            });
        }
        Ok(out)
    }

    /// The workspace, for the arms that need it.
    fn found(&self) -> Result<&Workspace, JanusError> {
        self.workspace.as_ref().ok_or_else(|| {
            usage("no workspace is in play; a corpus selection needs one (--workspace)")
        })
    }

    /// The one member a selection reads, for a tool that answers for one corpus.
    ///
    /// The plan owns the selection, so an unknown id is the same usage failure
    /// with the same list of ids as a query's (case 3.2).
    fn one<'a>(&'a self, found: &Workspace, choice: &Choice) -> Result<&'a Member, JanusError> {
        let plan = federate::plan(found, choice)?;
        let [only] = plan.corpora.as_slice() else {
            return Err(usage(
                "this tool answers for one corpus; pass \"primary\" or a corpus id",
            ));
        };
        self.members
            .iter()
            .find(|member| member.id.as_deref() == Some(only.0.id.as_str()))
            .ok_or_else(|| usage(format!("corpus {:?} is not loaded", only.0.id)))
    }

    /// The engine over one corpus, by the workspace's name for it.
    fn engine_of(&self, id: &str) -> Result<&Engine, JanusError> {
        self.members
            .iter()
            .find(|member| member.id.as_deref() == Some(id))
            .map(Member::engine)
            .ok_or_else(|| usage(format!("corpus {id:?} is not loaded")))
    }
}

impl Member {
    /// The name a row or an answer carries for this corpus.
    fn name(&self) -> &str {
        self.id.as_deref().unwrap_or(&self.config.corpus_id)
    }
}

/// True when the cached load still describes both the generation on disk and
/// the corpus this call walked (§2.4, M6-5, M6-17).
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
        && cached.config_hash == disk.config_hash
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

/// The selection one call asks for, refused when this engine cannot serve it.
///
/// One engine holds one corpus and one load cache, so `all` and a named id are
/// refused here rather than answered from the wrong corpus (M6b §2.3, M6b-12).
fn selection(corpus: Option<&str>) -> Result<Choice, JanusError> {
    let choice = Choice::parse(corpus);
    choice.require_single()?;
    Ok(choice)
}

/// A usage error: understood, but rejected before any read (§9.1 code 2).
fn usage(message: impl Into<String>) -> JanusError {
    JanusError::Usage {
        message: message.into(),
    }
}

// ─────────────────────────────── the write path ───────────────────────────

/// The file a write may touch: `given` inside `root` (case 2.8).
///
/// The corpus rule is the one every read uses ([`vault::confine`]), so `.git/`,
/// `.janus/` and a path outside the root answer `path_escape` here exactly as
/// they do on the read path. A relative path is the corpus's, never the working
/// directory's: the receipt names a corpus path, and a write is the last place
/// to guess which file was meant.
fn target(root: &Path, given: &str) -> Result<PathBuf, JanusError> {
    let candidate = if Path::new(given).is_absolute() {
        PathBuf::from(given)
    } else {
        root.join(given)
    };
    match vault::confine(root, &candidate) {
        Ok(resolved) => Ok(resolved),
        Err(JanusError::PathEscape { path }) => Err(JanusError::PathEscape { path }),
        // A path that cannot be resolved inside the corpus names no document in
        // it, which is the answer a caller can act on.
        Err(_) => Err(JanusError::DocumentNotInCorpus {
            path: PathBuf::from(given),
        }),
    }
}

/// Refuses a write whose etag is stale or absent (cases 2.1, 2.2).
///
/// Neither answer writes anything: the file is byte-identical afterwards. A dry
/// run needs no etag, because it writes nothing, and its receipt is the full
/// preview M7-13 points at.
fn check_etag(
    path: &Path,
    expected: Option<&str>,
    old_hash: Option<&str>,
    changed_keys: &[String],
    dry_run: bool,
) -> Result<(), JanusError> {
    match expected {
        Some(expected) if Some(expected) != old_hash => Err(JanusError::Conflict {
            path: path.to_path_buf(),
            expected: expected.to_owned(),
            current: old_hash.unwrap_or_default().to_owned(),
        }),
        Some(_) => Ok(()),
        None if dry_run => Ok(()),
        None => Err(JanusError::PreconditionRequired {
            path: path.to_path_buf(),
            preview: preview(changed_keys),
        }),
    }
}

/// The one line a write with no etag offers instead of a preview (M7-13).
fn preview(changed_keys: &[String]) -> String {
    let keys = match changed_keys {
        [] => "no key would change".to_owned(),
        keys => format!("would change: {}", keys.join(", ")),
    };
    format!(
        "{keys}; read the document and pass its content_hash as expected_hash, or pass \
         dry_run for the full preview"
    )
}

/// The receipt for one whole-file write (§2.3, M7-14).
fn document_receipt(root: &Path, path: &Path, document: &Document, dry_run: bool) -> Receipt {
    Receipt {
        path: relative(root, path),
        changed_keys: document.changed_keys.clone(),
        appended_keys: document.appended_keys.clone(),
        old_hash: document.old_hash.clone(),
        new_hash: document.new_hash(),
        dry_run,
        bytes_before: document.bytes_before,
        bytes_after: document.text.len(),
    }
}

/// The corpus-relative path a receipt names, with `/` separators (§2.3, §8.6).
fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The refusal a [`PatchError`] becomes, with the document named (§2.4, case 2.8).
///
/// A request that names no key, or one key twice, is `usage`: the caller can fix
/// the request before retrying. Everything else is a refusal of the document's
/// shape, which a retry without a change of request cannot cure.
fn patch_refusal(path: &Path, error: PatchError) -> JanusError {
    match error {
        PatchError::EmptyRequest | PatchError::DuplicateRequestKey { .. } => {
            usage(error.to_string())
        }
        PatchError::KeyShape { key } => JanusError::KeyShape {
            path: path.to_path_buf(),
            key,
        },
        PatchError::UnsupportedShape { key, reason } => JanusError::UnsupportedPatch {
            path: path.to_path_buf(),
            key,
            reason,
        },
        PatchError::NoFrontmatter => JanusError::NoFrontmatter {
            path: path.to_path_buf(),
        },
    }
}

/// The failure a [`CommitError`] becomes (§2.4).
///
/// A busy corpus is the lock timeout this crate already reports (M7-9). A link
/// that resolves outside the corpus is the same refusal the read path gives. A
/// write that landed with an unsynced directory is a runtime failure and not a
/// refusal, because the bytes are on disk.
fn commit_refusal(path: &Path, error: CommitError) -> JanusError {
    match error {
        CommitError::Conflict { expected, current } => JanusError::Conflict {
            path: path.to_path_buf(),
            expected,
            current,
        },
        CommitError::Busy { path } => JanusError::LockTimeout { path },
        CommitError::OutsideCorpus { path } => JanusError::PathEscape { path },
        CommitError::Exists { path } => JanusError::PathExists { path },
        CommitError::RolledBack {
            failed,
            restored,
            cause,
        } => JanusError::TransactionRolledBack {
            failed,
            restored,
            cause: cause.to_string(),
        },
        CommitError::Inconsistent {
            failed,
            old,
            cause,
            restore,
        } => JanusError::TransactionInconsistent {
            failed,
            old,
            cause: cause.to_string(),
            restore: restore.to_string(),
        },
        CommitError::Unsynced { path, source } | CommitError::Io { path, source } => {
            JanusError::Write { path, source }
        }
    }
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
fn facts_options(params: &FactsParams) -> Result<search::facts::Options, JanusError> {
    Ok(search::facts::Options {
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
    })
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
            config_hash: Some("dd".to_owned()),
        }
    }

    /// One changed manifest field: its name, and a change that names one fact.
    type Row = (&'static str, fn(&mut Manifest));

    /// The ten fields the key compares.
    fn compared() -> [Row; 10] {
        [
            ("index_sha", |m| m.index_sha = "00".to_owned()),
            ("vectors_sha", |m| m.vectors_sha = Some("00".to_owned())),
            ("corpus_sig", |m| m.corpus_sig = Some("00".to_owned())),
            ("config_hash", |m| m.config_hash = Some("00".to_owned())),
            ("format_version", |m| m.format_version = 99),
            ("schema_version", |m| m.schema_version = 99),
            ("corpus_id", |m| m.corpus_id = "other".to_owned()),
            ("tokenizer", |m| m.tokenizer.algorithm = "v9"),
            ("chunker", |m| m.chunker.max_chars = 1),
            ("embedder", |m| m.embedder = None),
        ]
    }

    #[test]
    fn a_corpus_selection_this_surface_cannot_serve_is_a_usage_failure() {
        // One engine holds one corpus and one cache, so `all` and a named id are
        // refused instead of answered from the wrong corpus (M6b §2.3). The
        // default and `primary` are the same selection this surface serves.
        for served in [None, Some("primary")] {
            assert_eq!(selection(served).expect("served"), Choice::Primary);
        }
        for refused in ["all", "context"] {
            let error = selection(Some(refused)).expect_err("needs a workspace");
            assert!(error.to_string().contains(refused), "{refused}: {error}");
            assert!(matches!(error, JanusError::Usage { .. }), "{refused}");
        }
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
