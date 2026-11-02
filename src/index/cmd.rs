//! `janus index`: build or refresh the lexical index (SPEC §9.1, §3.4, S-8).
//!
//! The only command that *writes* the cache. It runs the same pipeline as
//! [`crate::scan`] and [`crate::chunk::dump`] — walk, resolve, derive identity —
//! then plans an incremental build and publishes it under `.janus/lock` (§3.5).
//!
//! `--rebuild` discards the previous snapshot, so every chunk is tokenized
//! again. `PATHS` forces a re-tokenization of those documents only: the index
//! still covers the whole corpus, because a partial index would make every
//! later search silently incomplete. `--dry-run` reports the receipt and writes
//! nothing at all, which is what makes it safe on a large vault.
//!
//! One report, two modes, matching the other commands: a human summary or one
//! JSON receipt versioned with `"api": 1`.

use std::collections::BTreeSet;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::clock::Clock;
use crate::config::{self, Config};
use crate::document::Document;
use crate::error::JanusError;
use crate::id::DocId;
use crate::index::Snapshot;
use crate::index::build::{self, BuildPlan};
use crate::index::store::{self, INDEX_FILE, Publication};
use crate::scan::API_VERSION;
use crate::vault::{self, WalkOptions, identity, resolve};

/// How `janus index` should behave.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Ignore the previous snapshot: tokenize every chunk again.
    pub rebuild: bool,
    /// Report the receipt and write nothing.
    pub dry_run: bool,
    /// Emit one JSON receipt on stdout instead of the human summary.
    pub json: bool,
    /// Documents to re-tokenize even if their tokens could be reused.
    pub paths: Vec<PathBuf>,
}

/// Runs `janus index`.
///
/// # Errors
///
/// Propagates corpus, config, walk, path and publication failures. A file that
/// cannot be read is a skip, not an error, exactly as in `janus scan`.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    options: &Options,
) -> Result<(), JanusError> {
    let started = Instant::now();
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
    identity::derive_identity(&mut documents, config);

    let forced = forced_documents(root, &documents, &options.paths)?;
    let dir = config::resolve_index_dir(root, &config.index_dir);
    let previous = if options.rebuild {
        None
    } else {
        previous_snapshot(&dir)
    };
    let plan = build::plan_forced(config, &documents, previous.as_ref(), &forced);

    let written = if options.dry_run {
        None
    } else {
        let built_at = format!("{}T00:00:00Z", clock.today());
        Some(store::publish(&Publication {
            dir: &dir,
            root,
            snapshot: &plan.snapshot,
            chunker: plan.chunker,
            built_at: &built_at,
        })?)
    };

    for warning in &config.warnings {
        eprintln!("warning: {warning}");
    }

    let taken_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut out = io::stdout().lock();
    if options.json {
        let receipt = Receipt {
            api: API_VERSION,
            vault: root.display().to_string(),
            index: dir.join(INDEX_FILE).display().to_string(),
            dry_run: options.dry_run,
            docs: plan.snapshot.docs.len(),
            chunks: plan.snapshot.chunks.len(),
            terms: plan.snapshot.terms.len(),
            rebuilt: plan.stats.rebuilt_documents,
            reused: plan.stats.reused_documents,
            retokenized: plan.stats.retokenized_chunks,
            taken_ms,
            index_sha: written.as_ref().map(|manifest| manifest.index_sha.clone()),
        };
        let json = serde_json::to_string_pretty(&receipt)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // A closed pipe is not an index failure (`janus index --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(
            &mut out,
            root,
            &dir,
            &plan,
            written.as_ref(),
            options.dry_run,
            taken_ms,
        );
    }
    Ok(())
}

/// The previous generation if its bytes still decode: reuse needs tokens, not a
/// manifest (P-7). A corrupt file is simply not a reuse source.
fn previous_snapshot(dir: &Path) -> Option<Snapshot> {
    let bytes = std::fs::read(dir.join(INDEX_FILE)).ok()?;
    Snapshot::from_bytes(&bytes).ok()
}

/// Resolves `PATHS` to the documents they name, for a targeted re-tokenization.
///
/// Both readings of a relative path are tried — against the corpus root and
/// against the working directory — and a path that names no document in the
/// corpus is an error naming it, never a silent no-op.
fn forced_documents(
    root: &Path,
    documents: &[Document],
    paths: &[PathBuf],
) -> Result<BTreeSet<DocId>, JanusError> {
    let mut forced = BTreeSet::new();
    for given in paths {
        let mut found = false;
        for candidate in candidates(root, given) {
            let Ok(resolved) = vault::confine(root, &candidate) else {
                continue;
            };
            for document in documents {
                if document.path == resolved {
                    forced.insert(document.id);
                    found = true;
                }
            }
        }
        if !found {
            return Err(JanusError::DocumentNotInCorpus {
                path: given.clone(),
            });
        }
    }
    Ok(forced)
}

fn candidates(root: &Path, given: &Path) -> Vec<PathBuf> {
    if given.is_absolute() {
        return vec![given.to_path_buf()];
    }
    let mut candidates = vec![root.join(given)];
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(given));
    }
    candidates
}

// ── JSON ────────────────────────────────────────────────────────────────────

/// The `--json` receipt: what the build did, not what the documents said.
#[derive(Serialize)]
struct Receipt {
    api: u32,
    vault: String,
    index: String,
    dry_run: bool,
    docs: usize,
    chunks: usize,
    terms: usize,
    rebuilt: usize,
    reused: usize,
    retokenized: usize,
    taken_ms: u64,
    /// The published generation id; absent on `--dry-run`.
    #[serde(skip_serializing_if = "Option::is_none")]
    index_sha: Option<String>,
}

// ── human ───────────────────────────────────────────────────────────────────

fn write_human(
    out: &mut impl io::Write,
    root: &Path,
    dir: &Path,
    plan: &BuildPlan,
    written: Option<&store::Manifest>,
    dry_run: bool,
    taken_ms: u64,
) {
    let _ = writeln!(out, "corpus:  {}", root.display());
    let _ = writeln!(out, "index:   {}", dir.join(INDEX_FILE).display());
    let _ = writeln!(
        out,
        "{} document(s), {} chunk(s), {} term(s)",
        plan.snapshot.docs.len(),
        plan.snapshot.chunks.len(),
        plan.snapshot.terms.len()
    );
    let _ = writeln!(
        out,
        "rebuilt {}, reused {}, retokenized {} chunk(s)",
        plan.stats.rebuilt_documents, plan.stats.reused_documents, plan.stats.retokenized_chunks
    );
    match written {
        Some(manifest) => {
            let _ = writeln!(out, "published {} in {taken_ms} ms", manifest.index_sha);
        }
        None if dry_run => {
            let _ = writeln!(out, "dry run: nothing written ({taken_ms} ms)");
        }
        None => {
            let _ = writeln!(out, "nothing written ({taken_ms} ms)");
        }
    }
}
