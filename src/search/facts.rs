//! `janus facts`: authoritative facts only (SPEC §9.2, §3.6, S-12).
//!
//! The predicate is an **inclusion rule**, not the `current` gate: a document is
//! a fact when its canonical type is `decision`, `spec` or `principle` *and* its
//! declared status is `active`. `current` admits `draft` (0.7×) and `unknown`
//! (0.8×), so reusing it would surface draft specifications as authoritative
//! facts; a weight is not an inclusion rule (§3.6).
//!
//! No decay and no status factor: every included document has factor 1.0 and is
//! ordered `type_authority` desc → `date` desc → `chunk_id` asc. With `--query`
//! the BM25 value is computed and reported, and a fact that matches no term is
//! not a result; without it the result is the authority-ordered listing that
//! "give me the current facts" means.
//!
//! Rendering is `search::render` — the same table, the same DTO, one report
//! style. `--include-superseded` does not exist here: a search override must not
//! widen a decision's authority (§3.2).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use crate::clock::Clock;
use crate::config::Config;
use crate::error::JanusError;
use crate::score::{self, Intent, ScoreDoc, ScoreInput};

// One loader, one filter, one renderer: only the predicate and the order differ.
use super::{Filter, Hit, Render, prepare, query_scores, render, require_scope, select};

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
    options: &Options,
) -> Result<(), JanusError> {
    let started = Instant::now();
    require_scope(options.scope_only, &options.scopes)?;

    let prepared = prepare(root, config, clock, options.no_rebuild)?;
    let rows = super::doc_rows(&prepared.snapshot, config);
    let filter = Filter::new(
        config,
        &[],
        &options.tags,
        &options.scopes,
        options.scope_only,
    );
    let (selected, map) = select(&rows, |row| {
        score::is_fact(&row.score_doc()) && filter.matches(row)
    });
    let docs: Vec<ScoreDoc<'_>> = selected
        .iter()
        .map(|index| rows[*index].score_doc())
        .collect();

    let scores = options
        .query
        .as_deref()
        .map(|query| query_scores(&prepared.index, query));
    let hits = rank(
        clock,
        &prepared.snapshot,
        &docs,
        &map,
        scores.as_ref(),
        options.limit,
    );

    let notes = if options.query.is_none() {
        vec!["no --query: ordered by type authority, not by relevance".to_owned()]
    } else {
        Vec::new()
    };

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
        limit: options.limit,
        notes,
    })
}

/// Picks each document's representative chunk and orders documents by authority.
fn rank(
    clock: &dyn Clock,
    snapshot: &crate::index::Snapshot,
    docs: &[ScoreDoc<'_>],
    map: &[Option<usize>],
    scores: Option<&(Vec<f64>, f64)>,
    limit: usize,
) -> Vec<Hit> {
    // `history` is reused as the scoring row because it is exactly the §3.6
    // shape: no decay, every status at factor 1.0. Only `active` documents reach
    // this point, so the row's inclusivity is never visible.
    let policy = score::policy(Intent::History, false);
    debug_assert!(docs.iter().all(|doc| policy.includes(doc.status)));

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
                    intent: Intent::History,
                    alpha: 0.0,
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
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_facts_predicate_is_not_the_current_gate() {
        // Table-driven over every status: only `active` is a fact, while the
        // `current` gate admits draft/unknown besides it (§3.6).
        use crate::config::TypeSpec;
        use crate::document::Status;
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
            assert_eq!(score::is_fact(&doc), status == Status::Active, "{status}");
            assert_eq!(
                score::policy(Intent::Current, false).includes(status),
                status != Status::Deprecated && status != Status::Superseded,
                "{status}"
            );
        }
    }
}
