//! `janus lineage`: the supersession chain that contains one document (M5 §3.9).
//!
//! The graph owns the relations and the traversal. This module turns one chain
//! into the report a reader asks for: root first, one row per document with its
//! window and its status, the terminal state of every leaf, and one note per
//! cycle the breaker had to cut.
//!
//! A read, like `search`: it refreshes when the corpus changed, honours
//! `--no-rebuild`, and never writes a vault file. A retired document renders
//! from its tombstone, so a document that left the corpus keeps its row.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::clock::Clock;
use crate::config::Config;
use crate::error::JanusError;
use crate::graph::{ChainNode, EffectiveStatus, Endpoint, Graph, Window};
use crate::id::DocId;
use crate::index::TombstoneMeta;
use crate::index::embed::Embedder;
use crate::score::ScopeFilter;
use crate::search::{self, DocRow};
use crate::vault;

/// How `janus lineage` should be rendered.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The corpus path of the document whose chain to render.
    pub path: Option<PathBuf>,
    /// A `current` query whose top result the chain is rendered for.
    pub topic: Option<String>,
    /// Emit one JSON object on stdout instead of the table.
    pub json: bool,
    /// Refuse instead of rebuilding; a read stays a read.
    pub no_rebuild: bool,
    /// Read without resolving or loading an embedding model.
    pub no_embeddings: bool,
    /// Report embedding progress on stderr.
    pub progress: bool,
}

/// Runs `janus lineage`.
///
/// # Errors
///
/// [`JanusError::Usage`] when neither or both of `PATH` and `--topic` is given,
/// [`JanusError::DocumentNotInCorpus`] when a path names no live and no retired
/// document, [`JanusError::TopicNotFound`] for a query with no `current` result,
/// and every failure a read of the generation can produce.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    embedder: Option<&dyn Embedder>,
    options: Options,
) -> Result<(), JanusError> {
    let started = Instant::now();
    // The argument check runs before any read, so a usage error builds nothing.
    validate(&options)?;
    // A read passes the embedder so a rebuild it triggers still produces the
    // generation this process would build (V-21 row 5).
    let embedder = if options.no_embeddings {
        None
    } else {
        embedder
    };
    let want_vectors = embedder.is_some() && config.embedding.alpha > 0.0;
    let prepared = search::prepare(
        root,
        config,
        clock,
        embedder,
        want_vectors,
        options.no_rebuild,
        options.progress,
    )?;
    let report = compute(root, config, clock, embedder, &prepared, &options, started)?;
    render(&report, options.json)
}

/// Answers `janus lineage` without printing it (M6 §2.4).
///
/// # Errors
///
/// Exactly [`run`]'s.
pub(crate) fn compute(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    embedder: Option<&dyn Embedder>,
    prepared: &search::Prepared,
    options: &Options,
    started: Instant,
) -> Result<Report, JanusError> {
    let (id, topic) = locate(root, config, clock, embedder, prepared, options)?;
    let graph = Graph::for_snapshot(&prepared.snapshot, config);
    let rows = search::doc_rows(&prepared.snapshot, config);
    let scopes = ScopeFilter::new(&[], false);
    let tables = Tables {
        graph: &graph,
        rows: rows.iter().map(|row| (row.id, row)).collect(),
        tombstones: prepared
            .snapshot
            .tombstones
            .iter()
            .map(|meta| (meta.id, meta))
            .collect(),
        // One window and one status per node for this whole answer, never one
        // per row (M5-33).
        windows: graph.windows(&scopes),
        statuses: graph.effective_statuses(&scopes),
    };
    let chain = graph.chain(id);
    let mut notes = Vec::new();
    let cycle_breaks = tables.cycle_breaks(&chain, &mut notes);
    let terminal = tables.terminal(&chain, &mut notes);
    Ok(Report {
        vault: root.display().to_string(),
        corpus_id: config.corpus_id.clone(),
        topic,
        path: tables.path(id),
        taken_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        notes,
        cycle_breaks,
        terminal,
        results: chain.iter().map(|node| tables.node(node, id)).collect(),
    })
}

/// Writes the chain in the command's selected mode.
fn render(report: &Report, json: bool) -> Result<(), JanusError> {
    let mut out = io::stdout().lock();
    if json {
        let json = serde_json::to_string_pretty(&crate::api::Ok::new(report))
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // A closed pipe is not a command failure (`… --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(&mut out, report);
    }
    Ok(())
}

/// Which document the caller named (§3.9).
enum Target {
    /// A corpus path, live or retired.
    Path(PathBuf),
    /// A `current` query whose top result the chain belongs to.
    Topic(String),
}

/// Reads the one argument the caller gave; neither or both is a usage error.
fn target(options: &Options) -> Result<Target, JanusError> {
    match (&options.path, &options.topic) {
        (None, None) => Err(search::usage(
            "lineage needs a PATH or --topic QUERY: give exactly one",
        )),
        (Some(_), Some(_)) => Err(search::usage(
            "lineage takes a PATH or --topic QUERY, not both",
        )),
        (Some(given), None) => Ok(Target::Path(given.clone())),
        (None, Some(topic)) => Ok(Target::Topic(topic.clone())),
    }
}

/// The argument check on its own, for a caller that reads before it answers.
///
/// # Errors
///
/// [`JanusError::Usage`] when neither or both of `path` and `topic` is given.
pub(crate) fn validate(options: &Options) -> Result<(), JanusError> {
    target(options).map(|_| ())
}

/// Resolves the one argument into a document of the loaded generation.
fn locate(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    embedder: Option<&dyn Embedder>,
    prepared: &search::Prepared,
    options: &Options,
) -> Result<(DocId, Option<String>), JanusError> {
    match target(options)? {
        Target::Path(given) => {
            let graph = Graph::for_snapshot(&prepared.snapshot, config);
            // One lookup answers for a live and a retired path, because a
            // tombstone keeps the last path it held (M5-38).
            let Some(id) = corpus_path(&given, root)
                .ok()
                .and_then(|rel| graph.by_path(&rel))
            else {
                return Err(JanusError::DocumentNotInCorpus { path: given });
            };
            Ok((id, None))
        }
        Target::Topic(topic) => {
            // The chain of the top `current` result, ranked by the one search
            // path, so `lineage --topic` cannot disagree with `search` (M5-39).
            let ranked = search::rank(
                config,
                clock,
                embedder,
                prepared,
                &topic_options(&topic, options),
            )?;
            // `hit.doc_index` indexes `selected` by construction. The lookup is
            // total rather than indexed, because a panic in a read command is
            // never the right answer; an impossible mismatch reads as no result.
            let row = ranked
                .hits
                .first()
                .and_then(|hit| ranked.selected.get(hit.doc_index).copied())
                .and_then(|index| ranked.rows.get(index));
            let Some(row) = row else {
                return Err(JanusError::TopicNotFound { topic });
            };
            Ok((row.id, Some(topic)))
        }
    }
}

/// The `search` query a `--topic` chain is built for: one `current` result.
fn topic_options(topic: &str, options: &Options) -> search::Options {
    search::Options {
        query: topic.to_owned(),
        intent: Some("current".to_owned()),
        scopes: Vec::new(),
        scope_only: false,
        limit: 1,
        types: Vec::new(),
        tags: Vec::new(),
        decay_half_life: None,
        include_superseded: false,
        as_of: None,
        no_rebuild: options.no_rebuild,
        no_embeddings: options.no_embeddings,
        embedding_reason: None,
        progress: options.progress,
        json: false,
    }
}

/// The corpus-relative form of a `PATH` argument, read without touching a file.
///
/// A relative argument is already corpus-relative. An absolute one is accepted
/// when it resolves inside the corpus, so a reader can paste the path a search
/// result prints; a retired path has no file, so the lexical form is tried too.
fn corpus_path(given: &Path, root: &Path) -> Result<String, JanusError> {
    let absolute = if given.is_absolute() {
        given.to_path_buf()
    } else {
        root.join(given)
    };
    let canonical = dunce::canonicalize(&absolute).ok();
    [Some(absolute.as_path()), canonical.as_deref()]
        .into_iter()
        .flatten()
        .find_map(|path| vault::rel_path_of(root, path))
        .ok_or_else(|| JanusError::DocumentNotInCorpus {
            path: given.to_path_buf(),
        })
}

// ────────────────────────────── the tables ─────────────────────────────────

/// Everything one node row reads, gathered once per answer.
struct Tables<'a> {
    graph: &'a Graph,
    rows: BTreeMap<DocId, &'a DocRow>,
    tombstones: BTreeMap<DocId, &'a TombstoneMeta>,
    windows: BTreeMap<DocId, Window>,
    statuses: BTreeMap<DocId, EffectiveStatus>,
}

impl Tables<'_> {
    /// One node's path, empty when the generation holds no facts for the id.
    fn path(&self, id: DocId) -> String {
        self.graph
            .endpoint(id)
            .map(|endpoint| endpoint.facts().rel_path.clone())
            .unwrap_or_default()
    }

    /// One row. A live node reads its metadata row and the window this query
    /// resolved; a retired node reports what its tombstone holds and `null` for
    /// everything else, because nothing is invented (R4-1).
    fn node(&self, chain: &ChainNode, target: DocId) -> NodeDto {
        let id = chain.id;
        let mut node = NodeDto {
            path: self.path(id),
            id: id.to_hex(),
            content_hash: None,
            title: String::new(),
            type_name: None,
            date: None,
            declared_status: None,
            effective_status: None,
            valid_from: None,
            valid_to: None,
            depth: chain.depth,
            is_self: id == target,
            predecessors: self.links(id, true),
            successors: self.links(id, false),
        };
        if let Some(row) = self.rows.get(&id) {
            let window = self.windows.get(&id).copied().unwrap_or_default();
            node.content_hash = Some(row.content_hash.to_hex());
            node.title.clone_from(&row.title);
            node.type_name = Some(row.type_name.clone());
            node.date = row.date.map(|date| date.to_string());
            node.declared_status = Some(row.status.as_str());
            node.effective_status = Some(
                self.statuses
                    .get(&id)
                    .copied()
                    .unwrap_or(EffectiveStatus::Unknown)
                    .as_str(),
            );
            node.valid_from = window.valid_from.map(|date| date.to_string());
            node.valid_to = window.valid_to.map(|date| date.to_string());
            return node;
        }
        if let Some(meta) = self.tombstones.get(&id) {
            node.title.clone_from(&meta.title);
            node.date = meta.date.map(|date| date.to_string());
            node.valid_from = meta.valid_from.map(|date| date.to_string());
        }
        node
    }

    /// One relation list, by path and sorted, so the report is readable.
    fn links(&self, id: DocId, predecessors: bool) -> Vec<String> {
        let Some(node) = self.graph.node(id) else {
            return Vec::new();
        };
        let ids = if predecessors {
            &node.predecessors
        } else {
            &node.successors
        };
        let mut paths: Vec<String> = ids.iter().map(|id| self.path(*id)).collect();
        paths.sort();
        paths
    }

    /// The terminal state of one leaf (§3.9).
    ///
    /// `quiet_expired` is a declared `superseded` with no successor, and a leaf
    /// has none by construction. `retired` names a tombstone and is never an
    /// `EffectiveStatus` (M5-10, R4-1).
    fn state(&self, id: DocId) -> &'static str {
        if matches!(self.graph.endpoint(id), Some(Endpoint::Retired(_))) {
            return "retired";
        }
        match self.statuses.get(&id) {
            Some(EffectiveStatus::Active) => "active",
            Some(EffectiveStatus::Draft) => "draft",
            Some(EffectiveStatus::Deprecated) => "deprecated",
            Some(EffectiveStatus::Superseded) => "quiet_expired",
            Some(EffectiveStatus::Unknown) | None => "unknown",
        }
    }

    /// The state and the paths of every leaf. A diamond can end in more than one
    /// leaf; `state` is the first one's and a different one earns a note (M5-37).
    fn terminal(&self, chain: &[ChainNode], notes: &mut Vec<String>) -> TerminalDto {
        let leaves: Vec<DocId> = chain
            .iter()
            .filter(|node| {
                self.graph
                    .node(node.id)
                    .is_none_or(|node| node.successors.is_empty())
            })
            .map(|node| node.id)
            .collect();
        let Some(&first) = leaves.first() else {
            return TerminalDto {
                state: "unknown",
                paths: Vec::new(),
            };
        };
        let state = self.state(first);
        for leaf in &leaves[1..] {
            let other = self.state(*leaf);
            if other != state {
                notes.push(format!(
                    "terminal states differ: {} is {other}",
                    self.path(*leaf)
                ));
            }
        }
        TerminalDto {
            state,
            paths: leaves.iter().map(|id| self.path(*id)).collect(),
        }
    }

    /// The ignored pairs this chain shows, and one note per pair. An unrelated
    /// cycle is another chain's note (M5-38).
    fn cycle_breaks(&self, chain: &[ChainNode], notes: &mut Vec<String>) -> Vec<BreakDto> {
        let inside: BTreeSet<DocId> = chain.iter().map(|node| node.id).collect();
        let mut breaks = Vec::new();
        for &(from, to) in self.graph.ignored() {
            if !(inside.contains(&from) && inside.contains(&to)) {
                continue;
            }
            let (from, to) = (self.path(from), self.path(to));
            notes.push(format!("cycle break: ({from}, {to}) ignored"));
            breaks.push(BreakDto { from, to });
        }
        breaks
    }
}

// ─────────────────────────────── the report ────────────────────────────────

/// The `--json` report: one stable object, like every other command's.
#[derive(Debug, Serialize)]
pub(crate) struct Report {
    vault: String,
    corpus_id: String,
    /// The `--topic` text; absent for a `PATH` argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<String>,
    path: String,
    taken_ms: u64,
    notes: Vec<String>,
    cycle_breaks: Vec<BreakDto>,
    terminal: TerminalDto,
    /// One row per node, ordered by `(depth, path)`.
    results: Vec<NodeDto>,
}

/// One ignored edge, by path, so the note and the JSON agree.
#[derive(Debug, Serialize)]
struct BreakDto {
    from: String,
    to: String,
}

/// The end of a chain.
#[derive(Debug, Serialize)]
struct TerminalDto {
    /// `active`, `draft`, `deprecated`, `quiet_expired`, `unknown` or `retired`.
    state: &'static str,
    paths: Vec<String>,
}

/// One node of the chain (§3.9). The nullable fields are `null` only for a
/// retired document.
#[derive(Debug, Serialize)]
struct NodeDto {
    path: String,
    id: String,
    /// Hash of the whole file's bytes; `null` for a retired document, which
    /// keeps no hash to offer (M6 §2.7 rule 2).
    content_hash: Option<String>,
    /// Effective title, or the last title of a retired document.
    title: String,
    #[serde(rename = "type")]
    type_name: Option<String>,
    date: Option<String>,
    declared_status: Option<&'static str>,
    effective_status: Option<&'static str>,
    valid_from: Option<String>,
    valid_to: Option<String>,
    /// 0 at the root, one more per edge below it.
    depth: u32,
    #[serde(rename = "self")]
    is_self: bool,
    /// The documents this one replaced, then the ones that replace it, by path.
    predecessors: Vec<String>,
    successors: Vec<String>,
}

// ─────────────────────────────── human ─────────────────────────────────────

/// Prints one line per node: depth, status, window start, path. A retired node
/// is the only one with no effective status, and `retired` is what it reports.
fn write_human(out: &mut impl io::Write, report: &Report) {
    let _ = writeln!(out, "corpus:  {}", report.vault);
    if let Some(topic) = &report.topic {
        let _ = writeln!(out, "topic:   {topic:?}");
    }
    let _ = writeln!(out, "path:    {}\n", report.path);
    for node in &report.results {
        let mark = if node.is_self { "  (self)" } else { "" };
        let _ = writeln!(
            out,
            "{:>5}  {:<11}  {:<10}  {}{mark}",
            node.depth,
            node.effective_status.unwrap_or("retired"),
            node.valid_from.as_deref().unwrap_or("-"),
            node.path,
        );
    }
    let _ = writeln!(
        out,
        "\nterminal: {}  {}",
        report.terminal.state,
        report.terminal.paths.join(", ")
    );
    for note in &report.notes {
        let _ = writeln!(out, "note: {note}");
    }
}
