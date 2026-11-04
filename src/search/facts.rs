//! `janus facts`: authoritative facts only (SPEC §9.2, §3.6, S-12).
//!
//! The predicate is an **inclusion rule**, not the `current` gate: a document is
//! a fact when its canonical type is `decision`, `spec` or `principle`, its
//! **effective** status is `active`, and its validity window contains today.
//! `current` admits `draft` (0.7×) and `unknown` (1.0×), so reusing it would
//! surface draft specifications as authoritative facts; a weight is not an
//! inclusion rule (§3.6). The status is the derived one, so a document declared
//! active but replaced by a link is not a fact (§9 correction 11, M5-11).
//!
//! No decay and no status factor: every included document has factor 1.0 and is
//! ordered `type_authority` desc → `date` desc → `chunk_id` asc. With `--query`
//! the BM25 value is computed and reported, and a fact that matches no term is
//! not a result; without it the result is the authority-ordered listing that
//! "give me the current facts" means.
//!
//! # A corpus with no facts
//!
//! A vault that was never written for this tool declares no `decision`, `spec`
//! or `principle`. An empty list then hides the cause. When no document in the
//! corpus has a fact type, this command says so in one note and lists the newest
//! documents of any type instead, under `current`'s inclusion rule and sort
//! (M4B §3.6). The fallback is a report; it is not a new result contract.
//!
//! Rendering is `search::render` — the same table, the same DTO, one report
//! style. `--include-superseded` does not exist here: a search override must not
//! widen a decision's authority (§3.2).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use crate::clock::Clock;
use crate::config::Config;
use crate::document::Status;
use crate::error::JanusError;
use crate::graph::Graph;
use crate::index::embed::Embedder;
use crate::score::{self, Intent, ScopeFilter, ScoreDoc, ScoreInput, ScoredDoc};

// One loader, one filter, one renderer: only the predicate and the order differ.
use super::{Filter, Gate, Hit, Render, prepare, query_scores, render, require_scope, select};

/// How the included documents are ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Type authority first: the §3.6 order for a real fact list.
    Authority,
    /// `current`'s order: by the §3.1 total order, so newest first at zero score.
    Newest,
}

/// How `janus facts` should behave.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Narrows the listing by BM25 and reports relevance.
    pub query: Option<String>,
    /// Frontmatter tags; a document matches any one of them.
    pub tags: Vec<String>,
    /// The query scope set; repeated `--scope` unions (§6.12 rule 4).
    pub scopes: Vec<String>,
    /// Exclude `global` and shared-only matches; requires a scope.
    pub scope_only: bool,
    /// Maximum documents returned.
    pub limit: usize,
    /// Refuse instead of rebuilding; a read stays a read (S-8).
    pub no_rebuild: bool,
    /// Run lexically without resolving or loading a model (V-14).
    pub no_embeddings: bool,
    /// Why the adapter could not resolve an embedder, when it tried.
    pub embedding_reason: Option<String>,
    /// Report embedding progress on stderr. The edge resolved the gate (M4D
    /// §3.6).
    pub progress: bool,
    /// Emit one JSON object on stdout instead of the table.
    pub json: bool,
}

/// Runs `janus facts`.
///
/// # Errors
///
/// [`JanusError::Usage`] for `--scope-only` without `--scope`,
/// [`JanusError::IndexMissing`]/[`JanusError::IndexUnstable`] under
/// `--no-rebuild`, and any corpus, config or IO failure.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    embedder: Option<&dyn Embedder>,
    options: &Options,
) -> Result<(), JanusError> {
    let started = Instant::now();
    require_scope(options.scope_only, &options.scopes)?;

    let load_embedder = if options.no_embeddings {
        None
    } else {
        embedder
    };
    let prepared = prepare(
        root,
        config,
        clock,
        load_embedder,
        false,
        options.no_rebuild,
        options.progress,
    )?;
    let mut degraded = super::degraded_reasons(
        options.no_embeddings,
        load_embedder.is_some(),
        options.embedding_reason.as_deref(),
    );
    if !options.no_embeddings && load_embedder.is_some() {
        degraded.push("facts: lexical only (V-15)".to_owned());
    }
    let rows = super::doc_rows(&prepared.snapshot, config);
    let scopes = ScopeFilter::new(&options.scopes, options.scope_only);
    let graph = Graph::for_snapshot(&prepared.snapshot, config);
    let today = clock.today();
    // The trigger is corpus-wide and is about types only: a vault whose one
    // `decision` is still a draft does declare a fact type, so it keeps the
    // fact list (and an empty result) rather than a fallback list.
    let fallback = !has_fact_types(&rows);
    let order = if fallback {
        Order::Newest
    } else {
        Order::Authority
    };
    // Both lists apply the window gate. The fact list asks for the effective
    // status; the fallback list uses `current`'s inclusion rule.
    let gate = if fallback {
        Gate::current(&graph, &scopes, today)
    } else {
        Gate::facts(&graph, &scopes, today)
    };
    let filter = Filter::new(config, &[], &options.tags, scopes, gate);
    let selection = select(&rows, &filter);
    let (selected, map) = (selection.selected, selection.map);
    let docs: Vec<ScoreDoc<'_>> = selected
        .iter()
        .map(|index| rows[*index].score_doc())
        .collect();

    let scores = options
        .query
        .as_deref()
        .map(|query| query_scores(&prepared.index, query));
    let mut hits = rank(
        clock,
        &prepared.snapshot,
        &docs,
        &map,
        scores.as_ref(),
        order,
        &filter,
    );
    hits.truncate(options.limit);

    let mut notes = Vec::new();
    if fallback {
        notes.push(
            "no document declares type decision, spec or principle; \
             showing the newest documents instead"
                .to_owned(),
        );
    }
    if options.query.is_none() && !fallback {
        notes.push("no --query: ordered by type authority, not by relevance".to_owned());
    }

    render(&Render {
        root,
        config,
        snapshot: &prepared.snapshot,
        rows: &rows,
        selected: &selected,
        hits: &hits,
        json: options.json,
        started,
        query: options.query.as_deref(),
        intent: None,
        as_of: None,
        filter: &filter,
        limit: options.limit,
        alpha: 0.0,
        degraded,
        notes,
        stale: prepared.stale,
    })
}

/// Picks each document's representative chunk and orders documents.
fn rank(
    clock: &dyn Clock,
    snapshot: &crate::index::Snapshot,
    docs: &[ScoreDoc<'_>],
    map: &[Option<usize>],
    scores: Option<&(Vec<f64>, f64)>,
    order: Order,
    filter: &Filter,
) -> Vec<Hit> {
    // `history` is the fact list's scoring row because it is exactly the §3.6
    // shape: no decay, every status at factor 1.0. Only `active` documents reach
    // this path, so the row's inclusivity is never visible. The fallback list
    // uses `current`, because it uses `current`'s inclusion rule.
    let intent = match order {
        Order::Authority => Intent::History,
        Order::Newest => Intent::Current,
    };
    let policy = score::policy(intent, false);
    debug_assert!(
        docs.iter()
            .all(|doc| policy.includes(Status::from(filter.effective_status(doc.id))))
    );

    let (raw, reference) = match scores {
        Some((raw, reference)) => (raw.as_slice(), *reference),
        None => (&[][..], 0.0),
    };

    // The argmax chunk per document: by lexical value with a query, by source
    // order without one. A document with no chunks has nothing to cite.
    let mut best: Vec<Option<(usize, u32, f64)>> = vec![None; docs.len()];
    let mut ordinals = vec![0_u32; docs.len()];
    for (index, chunk) in snapshot.chunks.iter().enumerate() {
        let Some(Some(doc_index)) = map.get(chunk.doc as usize) else {
            continue;
        };
        let lexical = crate::index::squash(raw.get(index).copied().unwrap_or(0.0), reference);
        let ordinal = ordinals[*doc_index];
        ordinals[*doc_index] = ordinal.saturating_add(1);
        let better = best[*doc_index].is_none_or(|(_, _, current)| lexical > current);
        if better {
            best[*doc_index] = Some((index, ordinal, lexical));
        }
    }

    let mut hits: Vec<Hit> = best
        .into_iter()
        .enumerate()
        .filter_map(|(doc_index, found)| {
            let (chunk_index, ordinal, lexical) = found?;
            // With `--query` the listing is narrowed by BM25: a fact that
            // matches no query term is not a result, exactly as in `search`.
            if scores.is_some() && lexical <= 0.0 {
                return None;
            }
            let breakdown = score::score(
                clock,
                &ScoreInput {
                    doc: &docs[doc_index],
                    lexical,
                    semantic: 0.0,
                    intent,
                    effective_status: filter.effective_status(docs[doc_index].id),
                    alpha: 0.0,
                    // A boost cannot change an authority-ordered list, so
                    // reporting one would only inflate `relevance` (§3.2).
                    title_boost: 1.0,
                    half_life_days: None,
                    include_superseded: false,
                },
            );
            Some(Hit {
                doc_index,
                chunk_index,
                ordinal,
                breakdown,
            })
        })
        .collect();

    if order == Order::Newest {
        return newest_first(&hits, snapshot, docs);
    }

    let chunk_ids: BTreeMap<usize, crate::id::ChunkId> = hits
        .iter()
        .map(|hit| (hit.doc_index, snapshot.chunks[hit.chunk_index].id))
        .collect();
    hits.sort_by(|a, b| {
        let (left, right) = (&docs[a.doc_index], &docs[b.doc_index]);
        b.breakdown
            .type_authority
            .total_cmp(&a.breakdown.type_authority)
            .then_with(|| score::cmp_date_desc(left.date, right.date))
            .then_with(|| chunk_ids[&a.doc_index].cmp(&chunk_ids[&b.doc_index]))
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.rel_path.cmp(right.rel_path))
    });
    hits
}

/// Applies `current`'s §3.1 order to the fallback list.
///
/// Without a query every `final` score is zero, so the date key decides and the
/// list is the newest documents first. With a query the relevance term decides
/// first, exactly as `search --intent current` does.
fn newest_first(
    hits: &[Hit],
    snapshot: &crate::index::Snapshot,
    docs: &[ScoreDoc<'_>],
) -> Vec<Hit> {
    let mut ordered: Vec<(ScoredDoc<'_>, Hit)> = hits
        .iter()
        .map(|hit| {
            let scored = ScoredDoc {
                doc: docs[hit.doc_index],
                breakdown: hit.breakdown,
                chunk_id: snapshot.chunks[hit.chunk_index].id,
                ordinal: hit.ordinal,
            };
            (scored, *hit)
        })
        .collect();
    ordered.sort_by(|a, b| score::cmp_documents(&a.0, &b.0));

    ordered.into_iter().map(|(_, hit)| hit).collect()
}

/// Whether any document in the corpus has a type that `facts` calls a fact.
///
/// This reads the whole corpus, before the scope and tag filters: the note
/// explains the vault, so a filter that hides every fact must not claim the
/// vault has none.
fn has_fact_types(rows: &[super::DocRow]) -> bool {
    rows.iter()
        .any(|row| score::FACT_TYPES.contains(&row.type_name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_facts_predicate_is_not_the_current_gate() {
        // Table-driven over every effective status: only `active` is a fact,
        // while the `current` gate admits draft/unknown besides it (§3.6).
        use crate::config::TypeSpec;
        use crate::document::Status;
        use crate::graph::EffectiveStatus;
        use crate::id::DocId;

        let spec = TypeSpec {
            authority: 1.5,
            timeless: true,
            half_life_days: 180.0,
        };
        for status in score::STATUSES {
            let doc = ScoreDoc {
                id: DocId::from_bytes([1; 16]),
                rel_path: "decisions/a.md",
                type_name: "decision",
                type_spec: spec,
                status,
                date: None,
                effective_from: None,
                scope: "global",
                shared_with: &[],
            };
            let effective = EffectiveStatus::from(status);
            assert_eq!(
                score::is_fact(&doc, effective),
                status == Status::Active,
                "{status}"
            );
            assert_eq!(
                score::policy(Intent::Current, false).includes(status),
                status != Status::Deprecated && status != Status::Superseded,
                "{status}"
            );
        }
    }
}
