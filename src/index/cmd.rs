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

use chrono::NaiveDate;
use serde::Serialize;

use crate::clock::Clock;
use crate::config::{self, Config};
use crate::error::JanusError;
use crate::graph::Graph;
use crate::id::DocId;
use crate::index::Snapshot;
use crate::index::build::{self, BuildPlan};
use crate::index::embed::Embedder;
use crate::index::store::{self, INDEX_FILE};
use crate::progress::Printer;
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
    /// Report embedding progress on stderr. The edge resolved the gate, because
    /// this crate never reads the terminal (M4D §3.6).
    pub progress: bool,
    /// Documents to re-tokenize even if their tokens could be reused.
    pub paths: Vec<PathBuf>,
}

/// Runs `janus index`.
///
/// `embedder` is the one resolved at the binary edge (V-25): `None` publishes a
/// lexical generation and drops any existing payload, and every chunk of every
/// document is embedded when it is `Some` (P1b has no reuse; P4 adds it).
///
/// # Errors
///
/// Propagates corpus, config, walk, path, embedding and publication failures. A
/// file that cannot be read is a skip, not an error, exactly as in `janus scan`.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    embedder: Option<&dyn Embedder>,
    options: &Options,
) -> Result<(), JanusError> {
    let started = Instant::now();
    let today = clock.today();
    let walk = vault::walk(root, config, WalkOptions::default())?;
    let dir = config::resolve_index_dir(root, &config.index_dir);
    // One basis for both the forced-path lookup and the build, so a targeted
    // `janus index PATH` forces the ids the build will publish (§3.1, R-6).
    let basis = store::previous_snapshot(&dir);
    let forced = forced_documents(root, config, &walk, basis.as_ref(), today, &options.paths)?;
    let printer = Printer::new(options.progress);
    let sink = |done: u64, total: u64| printer.report(done, total);

    // A dry run plans and writes nothing; it takes no lock, because a lock is
    // held to publish (§3.7, D-9).
    let (plan, written, passes) = if options.dry_run {
        let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
        let previous = if options.rebuild {
            None
        } else {
            store::previous_generation(&dir)
        };
        // A dry run embeds nothing, so it reports nothing. It resolves identity
        // exactly as a write would, so the receipt describes the real build.
        let plan = build::plan_forced_dry_run(
            config,
            &mut documents,
            previous.as_ref(),
            basis.as_ref(),
            today,
            &forced,
            embedder,
            &build::no_progress,
        )?;
        (plan, None, 0)
    } else {
        // The command owns the lock and passes it, so the write loop publishes
        // under one lock and never tries to take it twice (§3.2, D-14).
        let lock = store::Lock::acquire(&dir, store::LOCK_TIMEOUT)?;
        let outcome = store::build_until_stable(
            &dir,
            root,
            config,
            embedder,
            today,
            walk,
            basis.as_ref(),
            &forced,
            options.rebuild,
            Some(&sink),
            &lock,
        )?;
        (outcome.plan, Some(outcome.manifest), outcome.passes)
    };

    for warning in &config.warnings {
        eprintln!("warning: {warning}");
    }

    let taken_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // One graph for the whole receipt, built by the same function a read calls.
    // The counts below and the answers a reader gives can then never disagree
    // about what a node is (§3.4, §3.5).
    let graph = Graph::for_snapshot(&plan.snapshot, config);
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
            dim: plan.vector_id.as_ref().map(|id| id.dim),
            embedded: plan.stats.embedded_chunks,
            reused_vectors: plan.stats.reused_vector_chunks,
            vectors_bytes: plan.vectors_bytes(),
            edges: plan.snapshot.edges.len(),
            dangling_links: plan.snapshot.dangling_links.len(),
            cycle_breaks: graph.ignored().len(),
            inert_links: graph.inert().len(),
            passes,
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

/// Resolves `PATHS` to the documents they name, for a targeted re-tokenization.
///
/// Both readings of a relative path are tried — against the corpus root and
/// against the working directory — and a path that names no document in the
/// corpus is an error naming it, never a silent no-op. With no path there is
/// nothing to resolve: the write loop resolves the walk itself.
#[allow(clippy::too_many_arguments)]
fn forced_documents(
    root: &Path,
    config: &Config,
    walk: &vault::WalkResult,
    basis: Option<&Snapshot>,
    today: NaiveDate,
    paths: &[PathBuf],
) -> Result<BTreeSet<DocId>, JanusError> {
    if paths.is_empty() {
        return Ok(BTreeSet::new());
    }
    let (mut documents, _skips) = resolve::resolve_all(&walk.files, config);
    // The same basis and the same date the build uses, or the forced set names
    // an id the published generation does not hold.
    identity::resolve(&mut documents, basis, config, today);
    let mut forced = BTreeSet::new();
    for given in paths {
        let mut found = false;
        for candidate in candidates(root, given) {
            let Ok(resolved) = vault::confine(root, &candidate) else {
                continue;
            };
            for document in &documents {
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
    /// The row width of the published (or, on a dry run, planned) payload.
    dim: Option<u32>,
    /// Chunks that went through the embedder; zero on a full reuse.
    embedded: usize,
    /// Rows copied from the previous generation.
    reused_vectors: usize,
    /// The byte length `vectors.f32` has (or would have).
    vectors_bytes: Option<u64>,
    /// Links that resolved to two document ids.
    edges: usize,
    /// Links that resolved to nothing.
    dangling_links: usize,
    /// Edges the cycle breaker ignored.
    cycle_breaks: usize,
    /// Edges stored but inert because their scopes do not intersect.
    inert_links: usize,
    /// How many build passes ran: one on a quiet corpus, more when a document
    /// changed while the build ran (§3.2). Zero on `--dry-run`.
    passes: u32,
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
    let _ = writeln!(
        out,
        "embedded {}, reused {} vector row(s)",
        plan.stats.embedded_chunks, plan.stats.reused_vector_chunks
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
