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

use std::collections::{BTreeMap, BTreeSet};

pub mod facts;
use std::io::{self, Write as _};
use std::path::Path;
use std::time::Instant;

use chrono::NaiveDate;
use serde::Serialize;

use crate::clock::Clock;
use crate::config::{self, Config, TypeSpec};
use crate::document::{DateSource, Status};
use crate::error::JanusError;
use crate::graph::{EffectiveStatus, Graph, Window};
use crate::id::{ChunkId, DocId};
use crate::index::embed::Embedder;
use crate::index::refresh;
use crate::index::store::{self, LoadOptions, Policy};
use crate::index::vectors::{Vectors, dot};
use crate::index::{ChunkMeta, LexicalIndex, Snapshot, StringId, StringPool};
use crate::progress::Printer;
use crate::scan::API_VERSION;
use crate::score::{self, Breakdown, Intent, IntentPolicy, ScopeFilter, ScoreDoc, ScoreInput};
use crate::tokenize::{Tokenizer, Tokens};
use crate::vault::resolve::strip_leading_date;
use crate::vault::{self, WalkOptions};

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
    /// `--as-of`: the valid-time date, already parsed and expanded (§3.8).
    pub as_of: Option<NaiveDate>,
    /// Refuse instead of rebuilding; a read stays a read (S-8).
    pub no_rebuild: bool,
    /// Run lexically without resolving or loading a model (V-14).
    pub no_embeddings: bool,
    /// Why the adapter could not resolve an embedder, when it tried. `None`
    /// means resolution succeeded or the caller never resolved one.
    pub embedding_reason: Option<String>,
    /// Report embedding progress on stderr. The edge resolved the gate, because
    /// this crate never reads the terminal (M4D §3.6).
    pub progress: bool,
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
    embedder: Option<&dyn Embedder>,
    options: Options,
) -> Result<(), JanusError> {
    let started = Instant::now();
    let outcome = query(root, config, clock, embedder, &options)?;
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
        intent: outcome.reported_intent(),
        as_of: outcome.as_of,
        filter: &outcome.filter,
        limit: options.limit,
        alpha: outcome.alpha,
        degraded: outcome.degraded,
        notes: outcome.notes,
        stale: outcome.stale,
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
    /// The semantic weight actually applied to this query.
    pub alpha: f64,
    /// Why this query is not fully hybrid; empty when nothing is degraded.
    pub degraded: Vec<String>,
    /// The query intent the caller named.
    pub intent: Intent,
    /// The valid-time date this outcome answered for, when `--as-of` was given.
    pub as_of: Option<NaiveDate>,
    /// The eligibility gate this outcome was computed with, maps included.
    pub(crate) filter: Filter,
    /// Title tokenizations this query performed: one per selected document
    /// (M4C §3.1). An empty query still tokenizes each title once.
    pub title_tokenizations: usize,
    /// True when the corpus changed after the index was built (M4D §3.1).
    pub stale: bool,
    /// The generation this outcome was computed from.
    pub snapshot: Snapshot,
}

impl Outcome {
    /// The intent the envelope reports: the row that ranked the answer.
    ///
    /// An `--as-of` query ranks with the `history` row, so that is what it
    /// names, next to `as_of` (§3.8, M5-34). The key is never removed: a caller
    /// that keys on `intent` keeps working.
    fn reported_intent(&self) -> Option<Intent> {
        Some(if self.as_of.is_some() {
            Intent::History
        } else {
            self.intent
        })
    }
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
    embedder: Option<&dyn Embedder>,
    options: &Options,
) -> Result<Outcome, JanusError> {
    let intent = parse_intent(options.intent.as_deref())?;
    require_scope(options.scope_only, &options.scopes)?;
    // `--as-of` selects the valid-time window and nothing else, so it scores
    // with the `history` row: no decay, and every status at factor 1.0. The
    // gate below is what turns the status off, not this row (§3.8, M5-34).
    let scoring_intent = if options.as_of.is_some() {
        Intent::History
    } else {
        intent
    };

    // `--no-embeddings` is a statement about the query: do not resolve, load or
    // call a model at all (V-14, §3.6 row 4).
    let load_embedder = if options.no_embeddings {
        None
    } else {
        embedder
    };
    let configured_alpha = if load_embedder.is_some() {
        config.embedding.alpha
    } else {
        0.0
    };
    let prefetch = configured_alpha > 0.0 && load_embedder.is_some();
    let prepared = prepare(
        root,
        config,
        clock,
        load_embedder,
        prefetch,
        options.no_rebuild,
        options.progress,
    )?;

    let mut alpha = configured_alpha;
    let mut degraded = degraded_reasons(
        options.no_embeddings,
        load_embedder.is_some(),
        options.embedding_reason.as_deref(),
    );
    if alpha > 0.0 && prepared.vectors.is_none() {
        // A race could leave a generation without the rows this query wants.
        // Falling back keeps the dot product from ever seeing two widths.
        alpha = 0.0;
        degraded.push("index has no usable vectors; lexical only for this query".to_owned());
    }
    let query_vector = query_vector(load_embedder, &options.query, alpha > 0.0)?;
    let semantics = semantic_scores(
        prepared.vectors.as_ref(),
        query_vector.as_deref(),
        prepared.snapshot.chunks.len(),
    );

    let rows = doc_rows(&prepared.snapshot, config);
    let scopes = ScopeFilter::new(&options.scopes, options.scope_only);
    // One graph and one pair of maps per query, never per row: the gate must
    // not walk the successors again for every document (P3 scope 10).
    let graph = Graph::for_snapshot(&prepared.snapshot, config);
    let gate = Gate::search(
        &graph,
        &scopes,
        intent,
        options.include_superseded,
        options.as_of,
        clock.today(),
    );
    let filter = Filter::new(config, &options.types, &options.tags, scopes, gate);
    let selection = select(&rows, &filter);
    let (selected, map) = (selection.selected, selection.map);
    let docs: Vec<ScoreDoc<'_>> = selected
        .iter()
        .map(|index| rows[*index].score_doc())
        .collect();

    let tokenizer = Tokenizer::new();
    let query_tokens = tokenizer.tokenize(&options.query);
    let query_terms: Vec<&str> = query_tokens.iter().collect();
    let (scores, reference) = prepared.index.score_query(&query_terms);
    let titles: Vec<&str> = selected
        .iter()
        .map(|index| rows[*index].title.as_str())
        .collect();
    let indexed = indexed_terms(&prepared.index, &query_terms);
    let (title_boosts, title_tokenizations) =
        title_boosts(&tokenizer, &indexed, &titles, config.scoring.title_boost);
    let mut ordinals = vec![0_u32; docs.len()];
    let mut occurrences: Vec<Vec<(ChunkId, u32, usize)>> = vec![Vec::new(); docs.len()];
    let (mut chunk_to_doc, mut chunk_ids, mut breakdowns) = (Vec::new(), Vec::new(), Vec::new());

    for (index, chunk) in prepared.snapshot.chunks.iter().enumerate() {
        let Some(Some(doc_index)) = map.get(chunk.doc as usize) else {
            continue;
        };
        let raw = scores.get(index).copied().unwrap_or(0.0);
        let doc = &docs[*doc_index];
        // Compute the score once, then use the quantity that decides the
        // ranking: `relevance > 0` admits a semantic-only match, and at α = 0
        // it is exactly M3's `lexical > 0` (V-9).
        let breakdown = score::score(
            clock,
            &ScoreInput {
                doc,
                lexical: crate::index::squash(raw, reference),
                semantic: semantics.get(index).copied().unwrap_or(0.0),
                intent: scoring_intent,
                effective_status: filter.effective_status(doc.id),
                alpha,
                title_boost: title_boosts.get(*doc_index).copied().unwrap_or(1.0),
                half_life_days: options.decay_half_life,
                include_superseded: options.include_superseded,
            },
        );
        if breakdown.relevance <= 0.0 {
            continue;
        }
        let ordinal = ordinals[*doc_index];
        ordinals[*doc_index] = ordinal.saturating_add(1);
        occurrences[*doc_index].push((chunk.id, ordinal, index));
        chunk_to_doc.push(*doc_index as u32);
        chunk_ids.push(chunk.id);
        breakdowns.push(breakdown);
    }

    let ranked = score::rank_documents(&docs, &chunk_to_doc, &chunk_ids, &breakdowns);
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
    if selection.hidden_status > 0 {
        let hint = if intent == Intent::Current {
            "use --intent history or --include-superseded"
        } else {
            "use --intent exhaustive"
        };
        notes.push(format!(
            "{} document(s) hidden by the status gate; {hint}",
            selection.hidden_status
        ));
    }
    if selection.hidden_window > 0 {
        // `--include-superseded` cancels only the window gate (M5-9), so it is
        // a real escape hatch here even though it does not widen `history`.
        notes.push(format!(
            "{} document(s) hidden by the validity window; use --include-superseded or --intent history to see them",
            selection.hidden_window
        ));
    }
    if selection.no_validity > 0 {
        // `--as-of` alone: a document with no window is excluded, and the count
        // says why. An out-of-window document is simply not in the answer, so
        // it gets no line: the window is the query, not a gate over it.
        notes.push(format!(
            "{} document(s) have no validity window",
            selection.no_validity
        ));
    }
    if let Some(as_of) = options.as_of
        && options
            .intent
            .as_deref()
            .is_some_and(|raw| !raw.trim().is_empty())
    {
        notes.push(format!(
            "as-of {as_of} overrides intent {}",
            intent_name(intent)
        ));
    }
    if matched > hits.len() {
        notes.push(format!(
            "{} more document(s) have relevance > 0; raise --limit to see them",
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
        alpha,
        degraded,
        intent,
        as_of: options.as_of,
        filter,
        title_tokenizations,
        stale: prepared.stale,
        snapshot: prepared.snapshot,
    })
}

// ────────────────────────────── load or build ──────────────────────────────

/// A validated generation, plus the index rebuilt from it.
pub(crate) struct Prepared {
    pub(crate) snapshot: Snapshot,
    pub(crate) index: LexicalIndex,
    pub(crate) vectors: Option<Vectors>,
    /// True when the corpus changed after the generation was built (M4D §3.1).
    /// True only when this read declined to refresh.
    pub(crate) stale: bool,
}

/// Loads a validated generation, refreshes it when the corpus changed, and
/// prints the one `refresh:` line that describes what it did (§3.4, §3.5).
///
/// The library stays silent; the command owns stderr. A fresh read prints
/// nothing at all.
pub(crate) fn prepare(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    embedder: Option<&dyn Embedder>,
    want_vectors: bool,
    no_rebuild: bool,
    progress: bool,
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
    let today = clock.today();
    // One walk, made here because the refresh reuses it for its own plan
    // (§3.1). The witness is exact, so the walk is its whole price.
    let walk = vault::walk(root, config, WalkOptions::default())?;
    // A read can embed for minutes, so a build it triggers reports too (§3.6).
    let printer = Printer::new(progress);
    let sink = |done: u64, total: u64| printer.report(done, total);
    let read = refresh::on_read(
        &dir,
        &LoadOptions {
            root,
            config,
            today,
            lock_timeout: store::LOCK_TIMEOUT,
            policy,
            // A lexical-only read never opens the payload (V-4) and a generation
            // with vectors still loads (row 6). The embedder is passed anyway
            // because a read that has to **rebuild** must publish the generation
            // this process would have built: without it, a first `search` after
            // `janus index` would drop the payload it just inherited (V-21 row 5,
            // §3.6 row 3).
            want_vectors,
            embedder,
            sink: Some(&sink),
        },
        walk,
        config.refresh,
    )?;
    if let Some(line) = read.decision.line() {
        eprintln!("{line}");
    }
    let index = read.load.snapshot.to_index()?;
    Ok(Prepared {
        snapshot: read.load.snapshot,
        index,
        vectors: read.load.vectors,
        stale: read.stale,
    })
}

/// Builds the `degraded` list from the flag, the resolved embedder and its reason.
fn degraded_reasons(no_embeddings: bool, resolved: bool, reason: Option<&str>) -> Vec<String> {
    if no_embeddings {
        return vec!["--no-embeddings: lexical only".to_owned()];
    }
    if resolved {
        return Vec::new();
    }
    vec![
        reason
            .unwrap_or("embeddings unavailable; lexical only")
            .to_owned(),
    ]
}

/// Embeds the query verbatim, once, or returns `None` for no work.
///
/// Empty text has no direction, so it would contribute `semantic = 0` anyway;
/// skipping the call keeps an exhaustive pass at M3 cost. The embedder owns its
/// tokenization, so the text is never tokenized here first (§3.3).
fn query_vector(
    embedder: Option<&dyn Embedder>,
    query: &str,
    enabled: bool,
) -> Result<Option<Vec<f32>>, JanusError> {
    if !enabled || query.trim().is_empty() {
        return Ok(None);
    }
    let Some(embedder) = embedder else {
        return Ok(None);
    };
    let mut rows = embedder.embed(&[query]).map_err(|error| {
        JanusError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("query embedding failed: {error}"),
        ))
    })?;
    if rows.len() != 1 {
        return Err(JanusError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "embedder `{}` returned {} rows for one query",
                embedder.id().name,
                rows.len()
            ),
        )));
    }
    let row = rows.pop().ok_or_else(|| {
        JanusError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("embedder `{}` returned no query row", embedder.id().name),
        ))
    })?;
    let dim = embedder.id().dim as usize;
    if row.len() != dim {
        return Err(JanusError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "embedder `{}` returned {} values for a {dim}-dimensional query",
                embedder.id().name,
                row.len()
            ),
        )));
    }
    Ok(Some(row))
}

/// `semantic = clamp(dot(query, row), 0, 1)` for every chunk (§3.3).
///
/// Rows are unit-norm or zero, so the dot is cosine and a zero row scores `0.0`
/// rather than `NaN`. The scan is single-threaded and allocates one `f64` per
/// chunk; a top-k prefilter is an M8 measurement, not a guess now (V-16).
fn semantic_scores(vectors: Option<&Vectors>, query: Option<&[f32]>, chunks: usize) -> Vec<f64> {
    let mut scores = vec![0.0; chunks];
    let (Some(vectors), Some(query)) = (vectors, query) else {
        return scores;
    };
    for (index, score) in scores.iter_mut().enumerate() {
        let Some(row) = vectors.row(index) else {
            continue;
        };
        // A width mismatch would trip `dot`'s debug assertion. The read path
        // guarantees it cannot happen, but a skipped row is safer than a panic.
        if row.len() != query.len() {
            continue;
        }
        *score = f64::from(dot(query, row).clamp(0.0, 1.0));
    }
    scores
}

/// Tokenizes `query` and returns every chunk's raw BM25 plus the query's `s_ref`.
pub(crate) fn query_scores(index: &LexicalIndex, query: &str) -> (Vec<f64>, f64) {
    let tokens = Tokenizer::new().tokenize(query);
    let terms: Vec<&str> = tokens.iter().collect();
    index.score_query(&terms)
}

/// The distinct query terms the index holds, with their idf (M4C §3.1).
///
/// A term the index never saw has no idf, so it leaves both the matched sum and
/// the total. Repetition in the query also changes nothing: the set is distinct.
fn indexed_terms<'t>(index: &LexicalIndex, terms: &[&'t str]) -> Vec<(&'t str, f64)> {
    let mut seen = BTreeSet::new();
    let mut indexed = Vec::new();
    for term in terms {
        if !seen.insert(*term) {
            continue;
        }
        let Some(id) = index.pool().lookup(term) else {
            continue;
        };
        let Some(meta) = index.terms().get(id.as_usize()) else {
            continue;
        };
        indexed.push((*term, index.idf(meta.df)));
    }
    indexed
}

/// One title boost per document, in input order (M4C §3.1).
///
/// `indexed` is the query's indexed terms with their idf. A title is stripped of
/// a leading date and tokenized once per document, never once per chunk: the
/// reference corpus has 200 000 chunks. The returned count is the number of
/// tokenizations performed, and the eval prints it.
fn title_boosts(
    tokenizer: &Tokenizer,
    indexed: &[(&str, f64)],
    titles: &[&str],
    weight: f64,
) -> (Vec<f64>, usize) {
    let total: f64 = indexed.iter().map(|(_, idf)| idf).sum();
    let mut boosts = Vec::with_capacity(titles.len());
    let mut tokens = Tokens::default();

    for title in titles {
        tokenizer.tokenize_into(strip_leading_date(title), &mut tokens);
        let title_terms: BTreeSet<&str> = tokens.iter().collect();
        let hits: f64 = indexed
            .iter()
            .filter(|(term, _)| title_terms.contains(term))
            .map(|(_, idf)| idf)
            .sum();
        let coverage = if total > 0.0 { hits / total } else { 0.0 };
        boosts.push((1.0 + weight * coverage).clamp(1.0, 2.0));
    }

    (boosts, titles.len())
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

/// Resolves `--as-of` to a calendar date (§3.8).
///
/// Accepts `YYYY-MM-DD`, `YYYY-MM` and `YYYY`. A short form expands to the first
/// day of its period, and the caller echoes the expanded value as `as_of`. An
/// unparseable value is a usage error that names it.
///
/// # Errors
///
/// [`JanusError::Usage`] when the value is not one of the three forms or names
/// a date the calendar does not have.
pub fn parse_as_of(raw: &str) -> Result<NaiveDate, JanusError> {
    let value = raw.trim();
    let field = |text: &str| text.parse::<u32>().ok();
    let ymd = |year: u32, month: u32, day: u32| {
        NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)
    };
    let fields: Vec<&str> = value.split('-').collect();
    let parsed = match fields.as_slice() {
        [year] => field(year).and_then(|year| ymd(year, 1, 1)),
        [year, month] => field(year).and_then(|year| field(month).and_then(|m| ymd(year, m, 1))),
        [year, month, day] => field(year)
            .and_then(|year| field(month).and_then(|m| field(day).and_then(|d| ymd(year, m, d)))),
        _ => None,
    };
    parsed.ok_or_else(|| {
        usage(format!(
            "unparseable --as-of value {value:?}; expected YYYY-MM-DD, YYYY-MM or YYYY"
        ))
    })
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
    /// Where [`DocRow::date`] came from; the precision is in the variant (B-3).
    pub date_source: DateSource,
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
                date_source: meta.date_source,
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

/// The status half of eligibility (§3.7, §3.8).
pub(crate) enum StatusGate {
    /// A §3.2 table row; a status the row does not list is excluded.
    Row(&'static IntentPolicy),
    /// `facts`: effective status `active` and a canonical fact type (§9.2).
    Fact,
    /// `--as-of`: no status gate, because today's standing is not `T`'s.
    Off,
}

/// The time half of eligibility (§3.6, §3.7, §3.8).
pub(crate) enum TimeGate {
    /// `history`, `evolution`, `exhaustive`: no window gate.
    Off,
    /// `current`: the window must contain `today`.
    Today(NaiveDate),
    /// `--as-of`: the window must contain `date`; no window means excluded.
    AsOf(NaiveDate),
}

/// One query's status and time gate, with both per-document maps precomputed.
///
/// This is the only place a window or an effective status is resolved for a
/// query. The maps are built here, once, before the row loop (P3 scope 10).
pub(crate) struct Gate {
    status: StatusGate,
    time: TimeGate,
    windows: BTreeMap<DocId, Window>,
    statuses: BTreeMap<DocId, EffectiveStatus>,
}

impl Gate {
    fn new(status: StatusGate, time: TimeGate, graph: &Graph, scopes: &ScopeFilter) -> Self {
        Self {
            status,
            time,
            windows: graph.windows(scopes),
            statuses: graph.effective_statuses(scopes),
        }
    }

    /// The gate for a `search` query (§3.7, §3.8).
    fn search(
        graph: &Graph,
        scopes: &ScopeFilter,
        intent: Intent,
        include_superseded: bool,
        as_of: Option<NaiveDate>,
        today: NaiveDate,
    ) -> Self {
        let (status, time) = if let Some(date) = as_of {
            (StatusGate::Off, TimeGate::AsOf(date))
        } else if intent == Intent::Current {
            // M5-9: the override row widens the statuses, and it cancels only
            // the window gate. A flag that shows an old document and then drops
            // it for being old is useless.
            let time = if include_superseded {
                TimeGate::Off
            } else {
                TimeGate::Today(today)
            };
            (
                StatusGate::Row(score::policy(intent, include_superseded)),
                time,
            )
        } else {
            (StatusGate::Row(score::policy(intent, false)), TimeGate::Off)
        };
        Self::new(status, time, graph, scopes)
    }

    /// The gate for `facts`: effective `active`, inside the window (§3.7).
    fn facts(graph: &Graph, scopes: &ScopeFilter, today: NaiveDate) -> Self {
        Self::new(StatusGate::Fact, TimeGate::Today(today), graph, scopes)
    }

    /// The gate for `facts`' fallback list, which uses `current`'s rule.
    fn current(graph: &Graph, scopes: &ScopeFilter, today: NaiveDate) -> Self {
        Self::new(
            StatusGate::Row(score::policy(Intent::Current, false)),
            TimeGate::Today(today),
            graph,
            scopes,
        )
    }
}

/// Why one row did or did not pass eligibility, so a command can count what
/// each gate hid (§3.7, §3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The query admits this document.
    Include,
    /// The type, tag or scope filter dropped it. Never counted: the user asked.
    Filtered,
    /// An effective status the query excludes.
    HiddenStatus,
    /// A window that does not contain the query's date.
    HiddenWindow,
    /// `--as-of`: the document has no validity window at all.
    NoValidity,
}

/// Type, tag, scope, status and time: eligibility, never a weight (§3.1 step 3).
pub(crate) struct Filter {
    types: BTreeSet<String>,
    tags: BTreeSet<String>,
    scopes: ScopeFilter,
    gate: Gate,
}

impl Filter {
    /// Canonicalizes types through the config registry, so `adr` matches
    /// `decision` (§6.6.1), and folds tag case for a friendlier match.
    pub(crate) fn new(
        config: &Config,
        types: &[String],
        tags: &[String],
        scopes: ScopeFilter,
        gate: Gate,
    ) -> Self {
        Self {
            types: types
                .iter()
                .map(|name| config.resolve_type(name).name.into_owned())
                .collect(),
            tags: tags.iter().map(|tag| normalise_tag(tag)).collect(),
            scopes,
            gate,
        }
    }

    /// The full eligibility rule for one row, with the reason it failed.
    pub(crate) fn evaluate(&self, row: &DocRow) -> Verdict {
        let type_ok = self.types.is_empty() || self.types.contains(row.type_name.as_str());
        let tag_ok = self.tags.is_empty()
            || row
                .tags
                .iter()
                .any(|tag| self.tags.contains(&normalise_tag(tag)));
        if !(type_ok && tag_ok && self.scopes.matches(&row.scope, &row.shared_with)) {
            return Verdict::Filtered;
        }

        let status = self.effective_status(row.id);
        let admitted = match self.gate.status {
            StatusGate::Row(policy) => policy.includes(Status::from(status)),
            StatusGate::Fact => score::is_fact(&row.score_doc(), status),
            StatusGate::Off => true,
        };
        if !admitted {
            return Verdict::HiddenStatus;
        }

        match self.gate.time {
            TimeGate::Off => Verdict::Include,
            TimeGate::Today(today) => {
                if self.window(row.id).covers_today(today) {
                    Verdict::Include
                } else {
                    Verdict::HiddenWindow
                }
            }
            TimeGate::AsOf(date) => {
                let window = self.window(row.id);
                if window.valid_from.is_none() {
                    Verdict::NoValidity
                } else if window.contains(date) {
                    Verdict::Include
                } else {
                    // The window is the query, so an out-of-window document is
                    // filtered, not hidden: it gets no note (§3.8).
                    Verdict::Filtered
                }
            }
        }
    }

    /// The window resolved for this query (§3.6). An unknown id has no window.
    pub(crate) fn window(&self, id: DocId) -> Window {
        self.gate.windows.get(&id).copied().unwrap_or_default()
    }

    /// The effective status resolved for this query (§3.7). Every row is a live
    /// endpoint of the graph, so the fallback is unreachable.
    pub(crate) fn effective_status(&self, id: DocId) -> EffectiveStatus {
        self.gate
            .statuses
            .get(&id)
            .copied()
            .unwrap_or(EffectiveStatus::Unknown)
    }
}

fn normalise_tag(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// What one pass over the rows selected, and why the rest were dropped.
pub(crate) struct Selection {
    /// Indices into the rows that passed, in row order.
    pub(crate) selected: Vec<usize>,
    /// Row index to position in [`Selection::selected`].
    pub(crate) map: Vec<Option<usize>>,
    /// Documents an effective status excluded.
    pub(crate) hidden_status: usize,
    /// Documents whose window does not contain the query's date.
    pub(crate) hidden_window: usize,
    /// `--as-of`: documents with no validity window at all.
    pub(crate) no_validity: usize,
}

/// Applies `filter` to every row, counting the reasons it dropped one.
pub(crate) fn select(rows: &[DocRow], filter: &Filter) -> Selection {
    let mut out = Selection {
        selected: Vec::new(),
        map: vec![None; rows.len()],
        hidden_status: 0,
        hidden_window: 0,
        no_validity: 0,
    };
    for (index, row) in rows.iter().enumerate() {
        match filter.evaluate(row) {
            Verdict::Include => {
                out.map[index] = Some(out.selected.len());
                out.selected.push(index);
            }
            Verdict::HiddenStatus => out.hidden_status += 1,
            Verdict::HiddenWindow => out.hidden_window += 1,
            Verdict::NoValidity => out.no_validity += 1,
            Verdict::Filtered => {}
        }
    }
    out
}

// ───────────────────────────────── results ─────────────────────────────────

/// One result: a document, the chunk that represents it, and its breakdown.
#[derive(Debug, Clone, Copy)]
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
    /// The expanded `--as-of` date, when the query asked for one (§3.8).
    pub(crate) as_of: Option<NaiveDate>,
    /// The gate this answer passed; it holds the window and status maps.
    pub(crate) filter: &'a Filter,
    pub(crate) limit: usize,
    pub(crate) alpha: f64,
    pub(crate) degraded: Vec<String>,
    pub(crate) notes: Vec<String>,
    /// True when the corpus changed after the index was built (M4D §3.1).
    pub(crate) stale: bool,
}

/// Renders the hits in the command's selected mode.
pub(crate) fn render(render: &Render<'_>) -> Result<(), JanusError> {
    // §3.5's provenance line is built here, not by a caller, so `search` and
    // `facts` cannot disagree about when it appears.
    let notes = notes_with_provenance(render);
    let mut out = io::stdout().lock();
    if render.json {
        let report = Report::new(render, &notes);
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // A closed pipe is not a command failure (`… --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(&mut out, render, &notes);
    }
    Ok(())
}

/// Adds the §3.5 line when a returned document has no date.
///
/// The count is over the documents that were actually returned, after `--limit`
/// and the status gates: the line states a fact about these results, not about
/// the corpus. It never asks the user to edit a file.
fn notes_with_provenance(render: &Render<'_>) -> Vec<String> {
    let mut notes = render.notes.clone();
    let undated = render
        .hits
        .iter()
        .filter(|hit| render.rows[render.selected[hit.doc_index]].date_source == DateSource::None)
        .count();
    if undated > 0 {
        notes.push(undated_note(undated));
    }
    notes
}

/// The `notes` line for `undated` returned results.
fn undated_note(undated: usize) -> String {
    format!("{undated} result(s) have no date and do not decay")
}

/// The `index` block of the JSON report: what the answer came from (M4D §3.1).
#[derive(Serialize)]
struct IndexDto {
    /// True when the corpus changed after the index was built.
    stale: bool,
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
    /// The expanded valid-time date (§3.8). Additive: `api` stays 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    as_of: Option<String>,
    limit: usize,
    taken_ms: u64,
    alpha: f64,
    degraded: &'a [String],
    notes: &'a [String],
    /// The index this answer came from (M4D §3.1). Additive: `api` stays 1.
    index: IndexDto,
    results: Vec<HitDto<'a>>,
}

impl<'a> Report<'a> {
    fn new(render: &'a Render<'_>, notes: &'a [String]) -> Self {
        Self {
            api: API_VERSION,
            vault: render.root.display().to_string(),
            corpus_id: &render.config.corpus_id,
            query: render.query,
            intent: render.intent.map(intent_name),
            as_of: render.as_of.map(|date| date.to_string()),
            limit: render.limit,
            taken_ms: u64::try_from(render.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            alpha: render.alpha,
            degraded: &render.degraded,
            notes,
            index: IndexDto {
                stale: render.stale,
            },
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
    /// Where `date` came from (§6.3, B-6); `absent` means no date at all.
    date_source: &'static str,
    effective_from: Option<String>,
    /// The window this query resolved for the document (§3.6).
    valid_from: Option<String>,
    valid_to: Option<String>,
    /// The status a reader must use, next to the declared `status` (§3.7).
    effective_status: &'static str,
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
        let window = render.filter.window(row.id);
        Self {
            rank,
            id: row.id.to_hex(),
            path: render.root.join(&row.rel_path).display().to_string(),
            rel_path: &row.rel_path,
            title: &row.title,
            type_name: &row.type_name,
            status: row.status.as_str(),
            date: row.date.map(|date| date.to_string()),
            date_source: row.date_source.as_str(),
            effective_from: row.effective_from.map(|date| date.to_string()),
            valid_from: window.valid_from.map(|date| date.to_string()),
            valid_to: window.valid_to.map(|date| date.to_string()),
            effective_status: render.filter.effective_status(row.id).as_str(),
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

fn write_human(out: &mut impl io::Write, r: &Render<'_>, notes: &[String]) {
    let _ = writeln!(out, "corpus:  {}", r.root.display());
    match (r.query, r.intent, r.as_of) {
        (Some(query), Some(intent), Some(as_of)) => {
            let _ = writeln!(
                out,
                "query:   {query:?}  as-of = {as_of}  intent = {}  limit = {}",
                intent_name(intent),
                r.limit
            );
        }
        (Some(query), None, Some(as_of)) => {
            let _ = writeln!(
                out,
                "query:   {query:?}  as-of = {as_of}  limit = {}",
                r.limit
            );
        }
        (Some(query), Some(intent), None) => {
            let _ = writeln!(
                out,
                "query:   {query:?}  intent = {}  limit = {}",
                intent_name(intent),
                r.limit
            );
        }
        (Some(query), None, None) => {
            let _ = writeln!(out, "query:   {query:?}  limit = {}", r.limit);
        }
        (None, ..) => {
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
    for note in notes {
        let _ = writeln!(out, "note: {note}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    /// Options for a unit-level query: lexical only, no rebuild, no JSON.
    fn options(query: &str) -> Options {
        Options {
            query: query.to_owned(),
            intent: None,
            scopes: Vec::new(),
            scope_only: false,
            limit: 10,
            types: Vec::new(),
            tags: Vec::new(),
            decay_half_life: None,
            include_superseded: false,
            as_of: None,
            no_rebuild: false,
            no_embeddings: true,
            embedding_reason: None,
            progress: false,
            json: false,
        }
    }

    #[test]
    fn the_window_map_is_built_once_for_the_whole_query() {
        // Four documents on one query. A window built per row would build the
        // graph four times; the map is built once, before the row loop, so the
        // count is 1 (P3 scope 10).
        let dir = tempfile::tempdir().expect("tempdir");
        let config = Config::from_toml(
            "index_dir = \".janus\"\ncorpus_id = \"gate\"\n",
            Path::new("<test>"),
        )
        .expect("config");
        for index in 1..=4 {
            let text =
                format!("---\ntitle: Cache {index}\ndate: 2021-0{index}-01\n---\n\ncache cache\n");
            std::fs::write(dir.path().join(format!("doc-{index}.md")), text).expect("write");
        }
        let today = NaiveDate::from_ymd_opt(2025, 4, 1).expect("date");

        crate::graph::reset_build_count();
        let outcome = query(
            dir.path(),
            &config,
            &FixedClock(today),
            None,
            &options("cache"),
        )
        .expect("query");

        assert_eq!(outcome.rows.len(), 4, "every document is a row");
        assert_eq!(crate::graph::build_count(), 1, "one graph per query");
    }

    #[test]
    fn as_of_accepts_three_forms_and_expands_a_short_one() {
        for (raw, expected) in [
            ("2020-03-01", (2020, 3, 1)),
            ("2021-07", (2021, 7, 1)),
            ("2021", (2021, 1, 1)),
            (" 2022-02-03 ", (2022, 2, 3)),
        ] {
            let date = parse_as_of(raw).expect("accepted form");
            assert_eq!(
                date,
                NaiveDate::from_ymd_opt(expected.0, expected.1, expected.2).expect("date"),
                "{raw}"
            );
        }
    }

    #[test]
    fn an_unparseable_as_of_is_a_usage_error_that_names_the_value() {
        for raw in ["2025-13-40", "not-a-date", "", "2020-03-01-05"] {
            let error = parse_as_of(raw).expect_err("rejected form");
            let text = error.to_string();
            assert!(text.contains(raw.trim()), "{raw:?}: {text}");
            assert!(
                matches!(error, JanusError::Usage { .. }),
                "{raw:?}: {error}"
            );
        }
    }

    #[test]
    fn a_missing_query_vector_scores_every_chunk_zero() {
        let vectors = Vectors::new(2, vec![1.0, 0.0, 0.0, 1.0]).expect("valid rows");
        assert_eq!(semantic_scores(Some(&vectors), None, 2), vec![0.0, 0.0]);
    }

    #[test]
    fn semantic_scores_clamp_and_treat_a_zero_row_as_zero() {
        // Rows: unit x, zero, unit -x. The query is unit x.
        let vectors = Vectors::new(2, vec![1.0, 0.0, 0.0, 0.0, -1.0, 0.0]).expect("valid rows");
        assert_eq!(
            semantic_scores(Some(&vectors), Some(&[1.0, 0.0]), 3),
            vec![1.0, 0.0, 0.0]
        );
    }

    #[test]
    fn the_undated_note_names_the_count() {
        assert_eq!(undated_note(1), "1 result(s) have no date and do not decay");
        assert_eq!(undated_note(3), "3 result(s) have no date and do not decay");
    }

    #[test]
    fn degraded_reasons_name_the_basis() {
        assert_eq!(
            degraded_reasons(true, false, None),
            vec!["--no-embeddings: lexical only"]
        );
        assert!(degraded_reasons(false, true, Some("ignored")).is_empty());
        assert_eq!(
            degraded_reasons(false, false, Some("model not found; lexical only")),
            vec!["model not found; lexical only"]
        );
    }

    // ─────────────────────────── the title boost ────────────────────────────

    /// The formula table of the M4C plan §3.1, with literals per row.
    ///
    /// `what` 0.3, `is` 0.2, `our` 0.4, `q4` 2.6, `okrs` 2.9: `total` is 6.4.
    fn table() -> Vec<(&'static str, f64)> {
        vec![
            ("what", 0.3),
            ("is", 0.2),
            ("our", 0.4),
            ("q4", 2.6),
            ("okrs", 2.9),
        ]
    }

    fn boost_of(title: &str, weight: f64) -> f64 {
        let tokenizer = Tokenizer::new();
        let (boosts, _) = title_boosts(&tokenizer, &table(), &[title], weight);
        boosts.first().copied().unwrap_or_default()
    }

    #[test]
    fn the_boost_table_matches_hand_computed_values() {
        // hits / total, then 1 + w * cov, clamped to [1, 2]. The second row
        // carries every query term, so it saturates at the bound.
        for (title, expected) in [
            ("q4", 1.0 + 2.6 / 6.4),
            ("what is our q4 okrs", 2.0),
            ("q4 okrs", 1.0 + 5.5 / 6.4),
            ("what", 1.0 + 0.3 / 6.4),
            ("unrelated title", 1.0),
        ] {
            let actual = boost_of(title, 1.0);
            assert!(
                (actual - expected).abs() < 1e-12,
                "{title:?}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn the_weight_scales_the_boost_and_zero_disables_it() {
        for (weight, expected) in [
            (0.0, 1.0),
            (0.5, 1.0 + 0.5 * 2.6 / 6.4),
            (1.0, 1.0 + 2.6 / 6.4),
        ] {
            let actual = boost_of("q4", weight);
            assert!((actual - expected).abs() < 1e-12, "w {weight}: {actual}");
        }
    }

    #[test]
    fn an_empty_query_or_a_term_outside_the_index_boosts_nothing() {
        let tokenizer = Tokenizer::new();
        let (empty, count) = title_boosts(&tokenizer, &[], &["q4 plan", "other"], 1.0);
        assert_eq!(empty, vec![1.0, 1.0]);
        assert_eq!(count, 2, "one tokenization per title");

        // Repetition does not duplicate a term: `hits` is a sum over a set.
        let (repeated, _) = title_boosts(&tokenizer, &[("q4", 2.6)], &["q4 q4 q4"], 1.0);
        assert_eq!(repeated, vec![2.0]);
    }

    #[test]
    fn a_leading_date_does_not_match_a_year_query() {
        let tokenizer = Tokenizer::new();
        let indexed = [("2024", 2.6)];
        let (boost, _) = title_boosts(
            &tokenizer,
            &indexed,
            &["2024-12-01-redis-cache", "redis-cache"],
            1.0,
        );
        assert_eq!(boost, vec![1.0, 1.0]);
    }
}
