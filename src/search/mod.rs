//! `janus search`: the §3.1 result contract end to end (SPEC §7.1, §9.1, §9.4).
//!
//! Load (or rebuild) a validated generation, tokenize the query with the same
//! tokenizer that indexed the chunks, score every chunk, then **group to
//! documents, sort, and apply `--limit`** — [`crate::score`] owns the grouping
//! and the total order, so `--limit N` returns N *documents*, each at its argmax
//! chunk, never N chunks of one dominant document.
//!
//! One report, two renderings, like [`crate::scan`]: a human table or one object
//! on stdout versioned with `"api": 1`. `--json` carries spans, paths,
//! breadcrumbs and the full [`Breakdown`] and **never** chunk text (§8.5) — the
//! snapshot has no body, so the guarantee is structural. Diagnostics go to
//! stderr (§8.2).

use std::collections::BTreeSet;

pub mod facts;
use std::io::{self, Write as _};
use std::path::Path;
use std::time::Instant;

use chrono::NaiveDate;
use serde::Serialize;

use crate::clock::Clock;
use crate::config::{self, Config, TypeSpec};
use crate::document::Status;
use crate::error::JanusError;
use crate::id::{ChunkId, DocId};
use crate::index::store::{self, LoadOptions, Policy};
use crate::index::{ChunkMeta, LexicalIndex, Snapshot, StringId, StringPool};
use crate::scan::API_VERSION;
use crate::score::{self, Breakdown, Intent, ScopeFilter, ScoreDoc, ScoreInput};
use crate::tokenize::Tokenizer;

/// The `--limit` default: ten documents (§3.1, S-16).
pub const DEFAULT_LIMIT: usize = 10;

/// How `janus search` should behave. Built by the CLI adapter, decided here.
#[derive(Debug, Clone)]
pub struct Options {
    /// The query text, tokenized with the indexing tokenizer (§3.7).
    pub query: String,
    /// Ranking intent name; `None` means `current`.
    pub intent: Option<String>,
    /// The query scope set; repeated `--scope` unions (§6.12 rule 4).
    pub scopes: Vec<String>,
    /// Exclude `global` and shared-only matches; requires a scope (§9 gap 10).
    pub scope_only: bool,
    /// Maximum documents returned, after grouping (§3.1 step 5).
    pub limit: usize,
    /// Canonical type names or configured aliases.
    pub types: Vec<String>,
    /// Frontmatter tags; a document matches any one of them.
    pub tags: Vec<String>,
    /// `--decay-half-life`; overrides the parameter, never `timeless` (§9 gap 9).
    pub decay_half_life: Option<f64>,
    /// Adds declared-`superseded` to `current`, at factor 1.0 (§3.2, S-19).
    pub include_superseded: bool,
    /// Refuse instead of rebuilding; a read stays a read (S-8).
    pub no_rebuild: bool,
    /// Emit one JSON object on stdout instead of the table.
    pub json: bool,
}

/// Runs `janus search`: ranks, then renders.
///
/// # Errors
///
/// [`JanusError::Usage`] for an unknown intent or `--scope-only` without a
/// scope, [`JanusError::IndexMissing`]/[`JanusError::IndexUnstable`] under
/// `--no-rebuild`, and any corpus, config or IO failure.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    options: Options,
) -> Result<(), JanusError> {
    let started = Instant::now();
    let outcome = query(root, config, clock, &options)?;
    render(&Render {
        root,
        config,
        snapshot: &outcome.snapshot,
        rows: &outcome.rows,
        selected: &outcome.selected,
        hits: &outcome.hits,
        json: options.json,
        started,
        query: Some(&options.query),
        intent: Some(outcome.intent),
        limit: options.limit,
        notes: outcome.notes,
    })
}

/// A ranked query result, owned so a caller can measure it without rendering.
pub struct Outcome {
    /// Every persisted document, in snapshot order.
    pub rows: Vec<DocRow>,
    /// Indices into [`Outcome::rows`] that passed the document filters.
    pub selected: Vec<usize>,
    /// Ranked results; each `doc_index` indexes [`Outcome::selected`].
    pub hits: Vec<Hit>,
    /// What the intent hid and what `--limit` truncated.
    pub notes: Vec<String>,
    /// The resolved query intent.
    pub intent: Intent,
    /// The generation this outcome was computed from.
    pub snapshot: Snapshot,
}

/// Ranks documents without rendering them (§3.1, §3.2).
///
/// The clock is injected, which is what lets the eval harness and the retrieval
/// tests run under [`crate::clock::FixedClock`] (§3.8).
///
/// # Errors
///
/// Exactly [`run`]'s.
pub fn query(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    options: &Options,
) -> Result<Outcome, JanusError> {
    let intent = parse_intent(options.intent.as_deref())?;
    require_scope(options.scope_only, &options.scopes)?;

    let prepared = prepare(root, config, clock, options.no_rebuild)?;
    let rows = doc_rows(&prepared.snapshot, config);
    let filter = Filter::new(
        config,
        &options.types,
        &options.tags,
        &options.scopes,
        options.scope_only,
    );
    let (selected, map) = select(&rows, |row| filter.matches(row));
    let docs: Vec<ScoreDoc<'_>> = selected
        .iter()
        .map(|index| rows[*index].score_doc())
        .collect();

    let (scores, reference) = query_scores(&prepared.index, &options.query);
    let policy = score::policy(intent, options.include_superseded);
    let mut ordinals = vec![0_u32; docs.len()];
    let mut occurrences: Vec<Vec<(ChunkId, u32, usize)>> = vec![Vec::new(); docs.len()];
    let mut hidden = BTreeSet::new();
    let (mut chunk_to_doc, mut chunk_ids, mut breakdowns) = (Vec::new(), Vec::new(), Vec::new());

    for (index, chunk) in prepared.snapshot.chunks.iter().enumerate() {
        let Some(Some(doc_index)) = map.get(chunk.doc as usize) else {
            continue;
        };
        let raw = scores.get(index).copied().unwrap_or(0.0);
        let doc = &docs[*doc_index];
        if raw <= 0.0 {
            continue;
        }
        if !policy.includes(doc.status) {
            // §3.2's gate is an inclusion rule: count it, do not score it zero.
            hidden.insert(*doc_index);
            continue;
        }
        let ordinal = ordinals[*doc_index];
        ordinals[*doc_index] = ordinal.saturating_add(1);
        occurrences[*doc_index].push((chunk.id, ordinal, index));
        chunk_to_doc.push(*doc_index as u32);
        chunk_ids.push(chunk.id);
        breakdowns.push(score::score(
            clock,
            &ScoreInput {
                doc,
                lexical: crate::index::squash(raw, reference),
                semantic: 0.0,
                intent,
                alpha: 0.0,
                half_life_days: options.decay_half_life,
                include_superseded: options.include_superseded,
            },
        ));
    }

    let ranked = score::rank_documents(policy, &docs, &chunk_to_doc, &chunk_ids, &breakdowns);
    let matched = ranked.len();
    let mut hits = Vec::new();
    for winner in ranked {
        if hits.len() >= options.limit {
            break;
        }
        let Some(doc_index) = docs.iter().position(|doc| doc.id == winner.doc.id) else {
            continue;
        };
        let found = occurrences[doc_index]
            .iter()
            .find(|(id, ordinal, _)| *id == winner.chunk_id && *ordinal == winner.ordinal);
        if let Some((_, ordinal, chunk_index)) = found {
            hits.push(Hit {
                doc_index,
                chunk_index: *chunk_index,
                ordinal: *ordinal,
                breakdown: winner.breakdown,
            });
        }
    }

    let mut notes = Vec::new();
    if !hidden.is_empty() {
        let hint = if intent == Intent::Current {
            "use --intent history or --include-superseded"
        } else {
            "use --intent exhaustive"
        };
        notes.push(format!(
            "{} result(s) hidden by intent {}; {hint}",
            hidden.len(),
            intent_name(intent)
        ));
    }
    if matched > hits.len() {
        notes.push(format!(
            "{} more document(s) matched; raise --limit to see them",
            matched - hits.len()
        ));
    }
    if options.scope_only {
        notes.push("--scope-only: global and shared-only matches excluded".to_owned());
    }

    Ok(Outcome {
        rows,
        selected,
        hits,
        notes,
        intent,
        snapshot: prepared.snapshot,
    })
}

// ────────────────────────────── load or build ──────────────────────────────

/// A validated generation, plus the index rebuilt from it.
pub(crate) struct Prepared {
    pub(crate) snapshot: Snapshot,
    pub(crate) index: LexicalIndex,
}

/// Loads a validated generation and prints the one stale line when a read built
/// one (§3.5, S-8). The library stays silent; the command owns stderr.
pub(crate) fn prepare(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    no_rebuild: bool,
) -> Result<Prepared, JanusError> {
    let dir = config::resolve_index_dir(root, &config.index_dir);
    let policy = if no_rebuild {
        Policy::Refuse
    } else {
        Policy::Rebuild
    };
    // `store::load` returns one variant for "absent" and "unstable" (P-8); a
    // caller must hear "never built", not "corrupt", so check the manifest first.
    if policy == Policy::Refuse && !dir.join(store::MANIFEST_FILE).exists() {
        return Err(JanusError::IndexMissing { dir });
    }
    for warning in &config.warnings {
        eprintln!("warning: {warning}");
    }
    let built_at = format!("{}T00:00:00Z", clock.today());
    let load = store::load(&LoadOptions {
        root,
        config,
        built_at: &built_at,
        lock_timeout: store::LOCK_TIMEOUT,
        policy,
        // P2 passes its resolved embedder and asks for the rows. Until then the
        // service is lexical-only: α is 0, so the payload is neither read nor
        // required (V-4), and a generation with vectors still loads (row 6).
        want_vectors: false,
        embedder: None,
    })?;
    // Only a generation this call had to build is stale; a retry that ended by
    // adopting a valid one is not.
    if let (true, Some(cause)) = (load.rebuilt, &load.stale) {
        eprintln!("stale: {cause}");
    }
    let index = load.snapshot.to_index()?;
    Ok(Prepared {
        snapshot: load.snapshot,
        index,
    })
}

/// Tokenizes `query` and returns every chunk's raw BM25 plus the query's `s_ref`.
pub(crate) fn query_scores(index: &LexicalIndex, query: &str) -> (Vec<f64>, f64) {
    let tokens = Tokenizer::new().tokenize(query);
    let terms: Vec<&str> = tokens.iter().collect();
    index.score_query(&terms)
}

/// Resolves the `--intent` name, rejecting an unknown one as a usage error.
pub(crate) fn parse_intent(raw: Option<&str>) -> Result<Intent, JanusError> {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("") | Some("current") => Ok(Intent::Current),
        Some("history") => Ok(Intent::History),
        Some("evolution") => Ok(Intent::Evolution),
        Some("exhaustive") => Ok(Intent::Exhaustive),
        Some(other) => Err(usage(format!(
            "unknown intent {other:?}; expected current, history, evolution or exhaustive"
        ))),
    }
}

/// `--scope-only` without a scope has nothing to narrow to (§9 gap 10).
pub(crate) fn require_scope(scope_only: bool, scopes: &[String]) -> Result<(), JanusError> {
    if scope_only && scopes.is_empty() {
        return Err(usage(
            "--scope-only requires at least one --scope; without one there is nothing to narrow to",
        ));
    }
    Ok(())
}

/// A usage error: understood, but rejected before any work (§9.1 code 2).
pub(crate) fn usage(message: impl Into<String>) -> JanusError {
    JanusError::Usage {
        message: message.into(),
    }
}

// ─────────────────────────────── document view ─────────────────────────────

/// One document's metadata, owned, so a command can borrow a [`ScoreDoc`].
pub struct DocRow {
    /// Stable document identity (§6.4).
    pub id: DocId,
    /// Vault-relative, `/`-separated path.
    pub rel_path: String,
    /// Effective title.
    pub title: String,
    /// Canonical type name, post-alias.
    pub type_name: String,
    /// Resolved authority and decay behaviour.
    pub type_spec: TypeSpec,
    /// Declared status.
    pub status: Status,
    /// Authored date.
    pub date: Option<NaiveDate>,
    /// Valid-time start, when declared.
    pub effective_from: Option<NaiveDate>,
    /// Normalised owning scope, `global` when absent.
    pub scope: String,
    /// Retrieval tags from `shared_with`, never identity (§6.12 rule 2).
    pub shared_with: Vec<String>,
    /// Frontmatter tags, as authored.
    pub tags: Vec<String>,
}

impl DocRow {
    pub(crate) fn score_doc(&self) -> ScoreDoc<'_> {
        ScoreDoc {
            id: self.id,
            rel_path: &self.rel_path,
            type_name: &self.type_name,
            type_spec: self.type_spec,
            status: self.status,
            date: self.date,
            effective_from: self.effective_from,
            scope: &self.scope,
            shared_with: &self.shared_with,
        }
    }
}

/// Resolves every persisted document row, in snapshot order.
pub(crate) fn doc_rows(snapshot: &Snapshot, config: &Config) -> Vec<DocRow> {
    let pool = &snapshot.strings;
    snapshot
        .docs
        .iter()
        .map(|meta| {
            let type_name = text(pool, meta.type_name);
            DocRow {
                id: meta.id,
                rel_path: text(pool, meta.rel_path),
                title: text(pool, meta.title),
                type_spec: config.resolve_type(&type_name).spec,
                type_name,
                status: meta.status,
                date: meta.date,
                effective_from: meta.effective_from,
                scope: text(pool, meta.scope),
                shared_with: strings(pool, &meta.shared_with),
                tags: strings(pool, &meta.tags),
            }
        })
        .collect()
}

fn text(pool: &StringPool, id: StringId) -> String {
    pool.get(id).unwrap_or_default().to_owned()
}

fn strings(pool: &StringPool, ids: &[StringId]) -> Vec<String> {
    ids.iter().map(|id| text(pool, *id)).collect()
}

/// Type, tag and scope filters: eligibility, never a weight (§3.1 step 3).
pub(crate) struct Filter {
    types: BTreeSet<String>,
    tags: BTreeSet<String>,
    scopes: ScopeFilter,
}

impl Filter {
    /// Canonicalizes types through the config registry, so `adr` matches
    /// `decision` (§6.6.1), and folds tag case for a friendlier match.
    pub(crate) fn new(
        config: &Config,
        types: &[String],
        tags: &[String],
        scopes: &[String],
        scope_only: bool,
    ) -> Self {
        Self {
            types: types
                .iter()
                .map(|name| config.resolve_type(name).name.into_owned())
                .collect(),
            tags: tags.iter().map(|tag| normalise_tag(tag)).collect(),
            scopes: ScopeFilter::new(scopes, scope_only),
        }
    }

    pub(crate) fn matches(&self, row: &DocRow) -> bool {
        let type_ok = self.types.is_empty() || self.types.contains(row.type_name.as_str());
        let tag_ok = self.tags.is_empty()
            || row
                .tags
                .iter()
                .any(|tag| self.tags.contains(&normalise_tag(tag)));
        type_ok && tag_ok && self.scopes.matches(&row.scope, &row.shared_with)
    }
}

fn normalise_tag(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// Applies `keep` to every row, returning the kept indices and a row-index map.
pub(crate) fn select(
    rows: &[DocRow],
    keep: impl Fn(&DocRow) -> bool,
) -> (Vec<usize>, Vec<Option<usize>>) {
    let mut selected = Vec::new();
    let mut map = vec![None; rows.len()];
    for (index, row) in rows.iter().enumerate() {
        if keep(row) {
            map[index] = Some(selected.len());
            selected.push(index);
        }
    }
    (selected, map)
}

// ───────────────────────────────── results ─────────────────────────────────

/// One result: a document, the chunk that represents it, and its breakdown.
pub struct Hit {
    /// Index into [`Outcome::selected`].
    pub doc_index: usize,
    /// Index into [`Snapshot::chunks`] for the argmax chunk.
    pub chunk_index: usize,
    /// Position of that chunk among the document's scored occurrences.
    pub ordinal: u32,
    /// Every factor of the score (§3.2).
    pub breakdown: Breakdown,
}

/// Everything a renderer needs to turn hits into output.
pub(crate) struct Render<'a> {
    pub(crate) root: &'a Path,
    pub(crate) config: &'a Config,
    pub(crate) snapshot: &'a Snapshot,
    pub(crate) rows: &'a [DocRow],
    pub(crate) selected: &'a [usize],
    pub(crate) hits: &'a [Hit],
    pub(crate) json: bool,
    pub(crate) started: Instant,
    pub(crate) query: Option<&'a str>,
    pub(crate) intent: Option<Intent>,
    pub(crate) limit: usize,
    pub(crate) notes: Vec<String>,
}

/// Renders the hits in the command's selected mode.
pub(crate) fn render(render: &Render<'_>) -> Result<(), JanusError> {
    let mut out = io::stdout().lock();
    if render.json {
        let report = Report::new(render);
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // A closed pipe is not a command failure (`… --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(&mut out, render);
    }
    Ok(())
}

#[derive(Serialize)]
struct Report<'a> {
    api: u32,
    vault: String,
    corpus_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    intent: Option<&'static str>,
    limit: usize,
    taken_ms: u64,
    degraded: [&'static str; 1],
    notes: &'a [String],
    results: Vec<HitDto<'a>>,
}

impl<'a> Report<'a> {
    fn new(render: &'a Render<'_>) -> Self {
        Self {
            api: API_VERSION,
            vault: render.root.display().to_string(),
            corpus_id: &render.config.corpus_id,
            query: render.query,
            intent: render.intent.map(intent_name),
            limit: render.limit,
            taken_ms: u64::try_from(render.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            degraded: ["lexical only; embeddings arrive in M4"],
            notes: &render.notes,
            results: render
                .hits
                .iter()
                .enumerate()
                .map(|(rank, hit)| HitDto::new(render, rank + 1, hit))
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct HitDto<'a> {
    rank: usize,
    id: String,
    path: String,
    rel_path: &'a str,
    title: &'a str,
    #[serde(rename = "type")]
    type_name: &'a str,
    status: &'a str,
    date: Option<String>,
    effective_from: Option<String>,
    scope: &'a str,
    shared_with: &'a [String],
    tags: &'a [String],
    scores: Breakdown,
    chunk_id: String,
    ordinal: u32,
    /// `(start, end)` byte span into the body; the text is absent (§8.5).
    span: (u32, u32),
    breadcrumb: Vec<&'a str>,
    tokens: u32,
}

impl<'a> HitDto<'a> {
    fn new(render: &'a Render<'_>, rank: usize, hit: &Hit) -> Self {
        let row = &render.rows[render.selected[hit.doc_index]];
        let chunk = &render.snapshot.chunks[hit.chunk_index];
        Self {
            rank,
            id: row.id.to_hex(),
            path: render.root.join(&row.rel_path).display().to_string(),
            rel_path: &row.rel_path,
            title: &row.title,
            type_name: &row.type_name,
            status: row.status.as_str(),
            date: row.date.map(|date| date.to_string()),
            effective_from: row.effective_from.map(|date| date.to_string()),
            scope: &row.scope,
            shared_with: &row.shared_with,
            tags: &row.tags,
            scores: hit.breakdown,
            chunk_id: chunk.id.to_hex(),
            ordinal: hit.ordinal,
            span: chunk.span,
            breadcrumb: breadcrumb(render.snapshot, chunk),
            tokens: chunk.tokens,
        }
    }
}

fn breadcrumb<'a>(snapshot: &'a Snapshot, chunk: &ChunkMeta) -> Vec<&'a str> {
    chunk
        .heading_path
        .iter()
        .map(|id| snapshot.strings.get(*id).unwrap_or_default())
        .collect()
}

fn intent_name(intent: Intent) -> &'static str {
    match intent {
        Intent::Current => "current",
        Intent::History => "history",
        Intent::Evolution => "evolution",
        Intent::Exhaustive => "exhaustive",
    }
}

// ── human ───────────────────────────────────────────────────────────────────

fn write_human(out: &mut impl io::Write, r: &Render<'_>) {
    let _ = writeln!(out, "corpus:  {}", r.root.display());
    match (r.query, r.intent) {
        (Some(query), Some(intent)) => {
            let _ = writeln!(
                out,
                "query:   {query:?}  intent = {}  limit = {}",
                intent_name(intent),
                r.limit
            );
        }
        (Some(query), None) => {
            let _ = writeln!(out, "query:   {query:?}  limit = {}", r.limit);
        }
        (None, _) => {
            let _ = writeln!(out, "facts:   active decisions, specs and principles");
        }
    }
    let _ = writeln!(out);

    if r.hits.is_empty() {
        let _ = writeln!(out, "no results");
    } else {
        let mut rows: Vec<[String; 5]> = vec![[
            "score".to_owned(),
            "authority".to_owned(),
            "status".to_owned(),
            "date".to_owned(),
            "path".to_owned(),
        ]];
        for hit in r.hits {
            let row = &r.rows[r.selected[hit.doc_index]];
            let crumb = breadcrumb(r.snapshot, &r.snapshot.chunks[hit.chunk_index]).join(" > ");
            rows.push([
                format!("{:.4}", hit.breakdown.final_score),
                format!("{:.2}", hit.breakdown.authority),
                row.status.as_str().to_owned(),
                row.date
                    .map_or_else(|| "-".to_owned(), |date| date.to_string()),
                if crumb.is_empty() {
                    row.rel_path.clone()
                } else {
                    format!("{}  {crumb}", row.rel_path)
                },
            ]);
        }
        let mut widths = [0_usize; 4];
        for row in &rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.chars().count());
            }
        }
        for row in &rows {
            let _ = writeln!(
                out,
                "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {}",
                row[0],
                row[1],
                row[2],
                row[3],
                row[4],
                w0 = widths[0],
                w1 = widths[1],
                w2 = widths[2],
                w3 = widths[3],
            );
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "{} result(s)", r.hits.len());
    for note in &r.notes {
        let _ = writeln!(out, "note: {note}");
    }
}
