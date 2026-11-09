//! Corpus selection and the merge of per-corpus answers (M6b §2.3, §3.2, M6b-2).
//!
//! A federated query reads a set of corpora, ranks each one with the same
//! ranking function a single-corpus query uses, and merges the ranked lists by
//! the tool's own order key. Nothing here re-implements an ordering: `rank` and
//! `facts::compute` own their orders, and the merge only decides which of two
//! already-ranked rows comes first.
//!
//! **Exactness.** Every corpus answers with its own top-`N`, where `N` is the
//! query limit. A document in the global top-`N` is necessarily inside its own
//! corpus's top-`N`, so merging those lists and cutting to `N` cannot lose a
//! document that belongs in the answer (§3.2). Asking a corpus for fewer rows is
//! the only way to lose one, and case 2.4's property test pins that.
//!
//! **The merge key is the tool's.** `search` merges by `final`. `facts` merges
//! its fact list by `type_authority`, because that order is why the command
//! exists (§7.1), and its fallback list by `final`. `archived_weight` multiplies
//! `final` and never reorders an authority list.

use std::path::Path;
use std::time::Instant;

use crate::clock::Clock;
use crate::config::Config;
use crate::error::JanusError;
use crate::index::embed::Embedder;
use crate::workspace::{Corpus, Role, Workspace};

use super::facts::{self, Order as FactsOrder};
use super::{Answer, HitDto, IndexDto, Options, Report, check, prepare, rank, render};

/// Which corpora one query reads (§2.3).
///
/// `primary` and `all` are keywords. A workspace id with either name cannot be
/// selected by name; the resolver reports the keywords first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Choice {
    /// The workspace's primary corpus. The default, and the single-corpus mode.
    #[default]
    Primary,
    /// Every corpus the workspace names, archived included.
    All,
    /// Exactly one corpus, by workspace id.
    Id(String),
}

impl Choice {
    /// Parses the `--corpus` flag or the `corpus` parameter. An id is resolved
    /// against the workspace, so this cannot fail on an unknown name.
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            None | Some("") | Some("primary") => Self::Primary,
            Some("all") => Self::All,
            Some(id) => Self::Id(id.to_owned()),
        }
    }

    /// Refuses a selection that needs a workspace, for a caller that serves one
    /// corpus. A silent fall back to the primary would answer a different
    /// question than the one asked (§2.3).
    ///
    /// # Errors
    ///
    /// [`JanusError::Usage`] for `all` or a named corpus.
    pub fn require_single(&self) -> Result<(), JanusError> {
        match self {
            Self::Primary => Ok(()),
            Self::All => Err(usage(
                "corpus \"all\" needs a workspace; this read serves one corpus",
            )),
            Self::Id(id) => Err(usage(format!(
                "corpus {id:?} needs a workspace; this read serves one corpus"
            ))),
        }
    }
}

/// The corpora one query reads, with each one's weight, and what a default
/// query left out (§2.3).
#[derive(Debug)]
pub struct Plan<'a> {
    /// One entry per corpus to read, in workspace order, with the factor that
    /// multiplies `final` for its rows.
    pub corpora: Vec<(&'a Corpus, f64)>,
    /// Archived corpora a default query excluded, for the one note it prints.
    pub excluded_archived: usize,
}

impl Plan<'_> {
    /// The single note a default query prints about the archived corpora it did
    /// not read (case 2.1). `None` when nothing was excluded.
    #[must_use]
    pub fn excluded_note(&self) -> Option<String> {
        match self.excluded_archived {
            0 => None,
            1 => Some("1 archived corpus excluded; pass --corpus all to read it".to_owned()),
            count => Some(format!(
                "{count} archived corpora excluded; pass --corpus all to read them"
            )),
        }
    }
}

/// Resolves a selection against one workspace (§2.3).
///
/// # Errors
///
/// [`JanusError::Usage`] when a named corpus is not in this workspace; the
/// message lists the ids the workspace does name.
pub fn plan<'a>(workspace: &'a Workspace, choice: &Choice) -> Result<Plan<'a>, JanusError> {
    let factor = |corpus: &Corpus| match corpus.role {
        Role::Primary => 1.0,
        Role::Archived => workspace.search.archived_weight,
    };
    match choice {
        Choice::Primary => {
            let corpora: Vec<(&Corpus, f64)> = workspace
                .corpora
                .iter()
                .filter(|corpus| corpus.role == Role::Primary)
                .map(|corpus| (corpus, 1.0))
                .collect();
            let excluded_archived = workspace
                .corpora
                .iter()
                .filter(|corpus| corpus.role == Role::Archived)
                .count();
            Ok(Plan {
                corpora,
                excluded_archived,
            })
        }
        Choice::All => Ok(Plan {
            corpora: workspace
                .corpora
                .iter()
                .map(|corpus| (corpus, factor(corpus)))
                .collect(),
            excluded_archived: 0,
        }),
        Choice::Id(id) => match workspace.corpora.iter().find(|corpus| corpus.id == *id) {
            Some(corpus) => Ok(Plan {
                corpora: vec![(corpus, factor(corpus))],
                excluded_archived: 0,
            }),
            None => Err(usage(format!(
                "unknown corpus {id:?}; this workspace names {}{}",
                workspace
                    .corpora
                    .iter()
                    .map(|corpus| corpus.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if workspace
                    .corpora
                    .iter()
                    .any(|corpus| corpus.role == Role::Archived)
                {
                    ", plus \"primary\" for the primary corpus and \"all\" for every corpus"
                } else {
                    ", plus \"primary\" and \"all\""
                }
            ))),
        },
    }
}

/// One corpus a query reads: what it is, where it is, and how its rows weigh.
pub struct Target<'a> {
    /// The workspace's name for this corpus; every row carries it (§3.2).
    pub id: &'a str,
    /// The corpus root a load walks.
    pub root: &'a Path,
    /// The corpus config, with the workspace's `index_dir` (M6b-11).
    pub config: &'a Config,
    /// The embedder this corpus resolved at the edge, or `None`.
    pub embedder: Option<&'a dyn Embedder>,
    /// Why this corpus has no embedder, for its `degraded` line.
    pub embedding_reason: Option<&'a str>,
    /// `1.0` for a primary corpus, `archived_weight` for an archived one (§2.4).
    pub factor: f64,
}

/// What a merged row is ordered by: the list's own key (§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// `search`, and the `facts` fallback list: by `final`.
    Final,
    /// The `facts` fact list: by `type_authority`, so a weight cannot move it.
    Authority,
}

/// One corpus's answer, before the merge.
struct Part<'a> {
    id: &'a str,
    factor: f64,
    report: Report,
}

/// Runs one federated `search` and renders it.
///
/// # Errors
///
/// Exactly [`super::run`]'s.
pub fn run_search(
    targets: &[Target<'_>],
    clock: &dyn Clock,
    options: Options,
    notes: Vec<String>,
) -> Result<(), JanusError> {
    let started = Instant::now();
    check(&options)?;
    let mut parts = Vec::with_capacity(targets.len());
    for target in targets {
        let embedder = if options.no_embeddings {
            None
        } else {
            target.embedder
        };
        let want_vectors = embedder.is_some() && target.config.embedding.alpha > 0.0;
        let prepared = prepare(
            target.root,
            target.config,
            clock,
            embedder,
            want_vectors,
            options.no_rebuild,
            options.progress,
        )?;
        // Each corpus decides its own semantic weight; only the reason it has
        // none is per corpus, so only that field changes.
        let mut options = options.clone();
        options.embedding_reason = target.embedding_reason.map(str::to_owned);
        let ranked = rank(target.config, clock, embedder, &prepared, &options)?;
        let report = Report::new(&Answer {
            root: target.root,
            config: target.config,
            snapshot: &prepared.snapshot,
            rows: &ranked.rows,
            selected: &ranked.selected,
            hits: &ranked.hits,
            started,
            query: Some(&options.query),
            intent: ranked.reported_intent(),
            as_of: ranked.as_of,
            filter: &ranked.filter,
            limit: options.limit,
            alpha: ranked.alpha,
            degraded: &ranked.degraded,
            notes: &ranked.notes,
            stale: ranked.stale,
        });
        parts.push(Part {
            id: target.id,
            factor: target.factor,
            report,
        });
    }
    let report = merge(parts, Key::Final, options.limit, started, notes);
    render(&report, options.json)
}

/// Runs one federated `facts` and renders it.
///
/// # Errors
///
/// Exactly [`super::facts::run`]'s.
pub fn run_facts(
    targets: &[Target<'_>],
    clock: &dyn Clock,
    options: &facts::Options,
    notes: Vec<String>,
) -> Result<(), JanusError> {
    let started = Instant::now();
    // The usage check runs before any load, so a rejected query builds nothing.
    super::require_scope(options.scope_only, &options.scopes)?;
    let mut parts = Vec::with_capacity(targets.len());
    let mut orders = Vec::with_capacity(targets.len());
    for target in targets {
        let embedder = if options.no_embeddings {
            None
        } else {
            target.embedder
        };
        let prepared = prepare(
            target.root,
            target.config,
            clock,
            embedder,
            false,
            options.no_rebuild,
            options.progress,
        )?;
        let listing = facts::compute(
            target.root,
            target.config,
            clock,
            embedder,
            &prepared,
            options,
            started,
        )?;
        orders.push(listing.order);
        parts.push(Part {
            id: target.id,
            factor: target.factor,
            report: listing.report,
        });
    }
    let report = merge(
        parts,
        key_of(facts_order(orders)),
        options.limit,
        started,
        notes,
    );
    render(&report, options.json)
}

/// The order a merged fact list uses: an authority list whenever any corpus
/// listed real facts, because that is the order the command exists for (§7.1).
fn facts_order(orders: impl IntoIterator<Item = FactsOrder>) -> FactsOrder {
    if orders
        .into_iter()
        .any(|order| order == FactsOrder::Authority)
    {
        FactsOrder::Authority
    } else {
        FactsOrder::Newest
    }
}

/// The merge key of one `facts` list (§3.2).
fn key_of(order: FactsOrder) -> Key {
    match order {
        FactsOrder::Authority => Key::Authority,
        FactsOrder::Newest => Key::Final,
    }
}

/// Names one corpus on its rows and in the report head (§3.2).
fn stamp(report: &mut Report, id: &str) {
    report.corpora = vec![id.to_owned()];
    for hit in &mut report.results {
        hit.corpus = Some(id.to_owned());
    }
}

/// Merges per-corpus answers into one, in the tool's order, then cuts to `limit`
/// (cases 2.2, 2.4, 2.6).
///
/// `parts` must be in corpus order, and each part's rows must already be in that
/// corpus's own order. The stable sort below is then the whole merge: rows of
/// equal value keep the order they arrived in, which is the corpus order across
/// corpora and the corpus's own order inside one corpus. That is the documented
/// tie-break, and it cannot reorder two documents one corpus already ordered.
fn merge(
    parts: Vec<Part<'_>>,
    key: Key,
    limit: usize,
    started: Instant,
    notes: Vec<String>,
) -> Report {
    let multi = parts.len() > 1;
    let mut head = Head::default();
    let mut notes = notes;
    let mut degraded: Vec<String> = Vec::new();
    let mut rows: Vec<(usize, HitDto)> = Vec::new();
    let mut corpora = Vec::with_capacity(parts.len());
    for (index, part) in parts.into_iter().enumerate() {
        let mut report = part.report;
        // The one place a corpus changes a row's score (M6b §2.4).
        for hit in &mut report.results {
            hit.scores = hit.scores.with_corpus(part.factor);
        }
        stamp(&mut report, part.id);
        head.stale |= report.index.stale;
        if index == 0 {
            head.absorb(&report);
        }
        corpora.push(part.id.to_owned());
        for note in &report.notes {
            notes.push(if multi {
                format!("{}: {note}", part.id)
            } else {
                note.clone()
            });
        }
        for reason in &report.degraded {
            if !degraded.contains(reason) {
                degraded.push(reason.clone());
            }
        }
        rows.extend(report.results.drain(..).map(|hit| (index, hit)));
    }

    order_rows(&mut rows, |row: &(usize, HitDto)| value(&row.1, key), limit);
    let results = rows
        .into_iter()
        .enumerate()
        .map(|(rank, (_, mut hit))| {
            hit.rank = rank + 1;
            hit
        })
        .collect();

    Report {
        vault: head.vault,
        corpus_id: head.corpus_id,
        corpora,
        query: head.query,
        intent: head.intent,
        as_of: head.as_of,
        limit,
        taken_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        alpha: head.alpha,
        degraded,
        notes,
        index: IndexDto { stale: head.stale },
        results,
    }
}

/// The report-level fields one merged answer inherits from its first corpus.
///
/// Query-wide values (the query, the intent, the date) are the same in every
/// part, so the first part's copy is the answer's. `alpha` is each corpus's own
/// setting; the head reports the first one, and every row keeps the score its
/// own corpus gave it.
#[derive(Default)]
struct Head {
    vault: String,
    corpus_id: String,
    query: Option<String>,
    intent: Option<&'static str>,
    as_of: Option<String>,
    alpha: f64,
    stale: bool,
}

impl Head {
    /// Takes the report-level fields of the first part.
    fn absorb(&mut self, report: &Report) {
        self.vault = report.vault.clone();
        self.corpus_id = report.corpus_id.clone();
        self.query = report.query.clone();
        self.intent = report.intent;
        self.as_of = report.as_of.clone();
        self.alpha = report.alpha;
    }
}

/// Orders rows by value, highest first, and cuts to `limit`.
///
/// The sort is stable, so equal values keep the order the caller built: corpus
/// order across corpora, then each corpus's own order (§3.2). No tie-break is
/// decided here, which is why no second ranking path can drift from this one.
fn order_rows<T>(rows: &mut Vec<T>, value: impl Fn(&T) -> f64, limit: usize) {
    rows.sort_by(|a, b| value(b).total_cmp(&value(a)));
    rows.truncate(limit);
}

/// The value one row is ordered by, for the key the tool chose (§3.2).
fn value(hit: &HitDto, key: Key) -> f64 {
    match key {
        Key::Final => hit.scores.final_score,
        Key::Authority => hit.scores.type_authority,
    }
}

/// A usage error: understood, but rejected before any read (§9.1 code 2).
fn usage(message: impl Into<String>) -> JanusError {
    JanusError::Usage {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::Breakdown;
    use proptest::prelude::*;

    // ──────────────────────────── the merge rule ────────────────────────────

    /// One corpus's documents, ranked by that corpus's own total order.
    ///
    /// `documents[i]` is a document's value; the document's identity is its
    /// position, and ties keep the lower position first, exactly as `rank` ends
    /// its order with the path.
    fn ranked(documents: &[u32], limit: usize, corpus: usize) -> Vec<((usize, usize), f64)> {
        let mut own: Vec<((usize, usize), f64)> = documents
            .iter()
            .enumerate()
            .map(|(id, value)| ((corpus, id), f64::from(*value) / 9.0))
            .collect();
        own.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.1.cmp(&b.0.1)));
        own.truncate(limit);
        own
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
        /// Case 2.4: the merged order equals sorting the **full union** of both
        /// corpora's documents by the query's key, and no document is missing.
        ///
        /// Values are coarse on purpose (`/9`), so the global top-`N` is often
        /// spread across corpora: a per-corpus fetch below `N` then loses a
        /// document, and this comparison sees it.
        #[test]
        fn case_2_4_the_merge_is_exact_against_the_full_union(
            corpora in prop::collection::vec(prop::collection::vec(0u32..=9, 0..=6), 2..=3),
            limit in 1usize..=6,
        ) {
            // The full union, each document at its own place in its corpus.
            let mut full: Vec<((usize, usize), f64)> = Vec::new();
            let mut parts: Vec<((usize, usize), f64)> = Vec::new();
            for (corpus, documents) in corpora.iter().enumerate() {
                full.extend(ranked(documents, usize::MAX, corpus));
                parts.extend(ranked(documents, limit, corpus));
            }
            prop_assume!(!full.is_empty());

            // The reference: the union by value, then corpus, then the place the
            // document holds in its corpus.
            full.sort_by(|a, b| {
                b.1.total_cmp(&a.1)
                    .then(a.0.0.cmp(&b.0.0))
                    .then(a.0.1.cmp(&b.0.1))
            });
            let expected: Vec<(usize, usize)> =
                full.iter().take(limit).map(|row| row.0).collect();

            order_rows(&mut parts, |row: &((usize, usize), f64)| row.1, limit);
            let merged: Vec<(usize, usize)> = parts.iter().map(|row| row.0).collect();

            prop_assert_eq!(&merged, &expected, "order and membership must both be exact");
            prop_assert_eq!(merged.len(), usize::min(limit, full.len()));
        }
    }

    #[test]
    fn case_2_4_a_per_corpus_fetch_below_the_limit_loses_a_document() {
        // The teeth of the property above. Values: main 0.9 and 0.85, context
        // 0.70. `--limit 2` must return 0.9 and 0.85, both from main. A corpus
        // asked for its top-1 would answer 0.9 and 0.70, and 0.85 is lost.
        let main: [(usize, f64); 2] = [(0, 0.9), (1, 0.85)];
        let context: [(usize, f64); 1] = [(0, 0.70)];
        let mut rows: Vec<(usize, f64)> = main
            .iter()
            .copied()
            .chain(context.iter().map(|(id, value)| (10 + id, *value)))
            .collect();
        order_rows(&mut rows, |row: &(usize, f64)| row.1, 2);
        assert_eq!(rows, vec![(0, 0.9), (1, 0.85)]);

        let mut truncated: Vec<(usize, f64)> = vec![(0, 0.9), (10, 0.70)];
        order_rows(&mut truncated, |row: &(usize, f64)| row.1, 2);
        assert_eq!(
            truncated,
            vec![(0, 0.9), (10, 0.70)],
            "asking a corpus for fewer rows is the only way to lose one"
        );
    }

    #[test]
    fn case_2_4_an_equal_value_keeps_corpus_order_then_the_corpus_s_own_order() {
        // All three rows tie on the key. The merge must keep the order the
        // parts arrived in: main's own order first, then context.
        let mut rows: Vec<(usize, &str)> = vec![(0, "main/new"), (0, "main/old"), (1, "context")];
        order_rows(&mut rows, |_| 0.5, 10);
        assert_eq!(
            rows.iter().map(|row| row.1).collect::<Vec<_>>(),
            vec!["main/new", "main/old", "context"]
        );
    }

    #[test]
    fn case_2_2_the_weight_scales_final_and_is_reported() {
        // The documented formula: final = relevance * authority * decay * corpus.
        // Relevance 0.6, authority 1.5 (type 1.5 * status 1.0), decay 0.8:
        // final = 0.6 * 1.5 * 0.8 = 0.72 for a primary row.
        // An archived row at weight 0.8: final = 0.6 * 1.5 * 0.8 * 0.8 = 0.576.
        let primary = Breakdown {
            semantic: 0.0,
            lexical: 0.6,
            relevance: 0.6,
            title_boost: 1.0,
            type_authority: 1.5,
            status_factor: 1.0,
            authority: 1.5,
            decay: 0.8,
            corpus: 1.0,
            final_score: 0.72,
        };
        let archived = primary.with_corpus(0.8);
        assert_eq!(archived.corpus, 0.8);
        assert!(
            (archived.final_score - 0.576).abs() < 1e-12,
            "0.72 * 0.8 = 0.576, not {}",
            archived.final_score
        );
        // Every other factor is the corpus's own and is not touched.
        assert_eq!(archived.relevance, primary.relevance);
        assert_eq!(archived.authority, primary.authority);
        assert_eq!(archived.decay, primary.decay);

        // A neutral factor is not printed, so no single-corpus row moves.
        let neutral = serde_json::to_value(primary).expect("a breakdown is JSON");
        assert!(neutral.get("corpus").is_none(), "{neutral}");
        let weighted = serde_json::to_value(archived).expect("a breakdown is JSON");
        assert_eq!(weighted["corpus"], serde_json::json!(0.8), "{weighted}");
    }

    #[test]
    fn case_2_7_the_fact_list_keeps_authority_when_a_corpus_falls_back() {
        assert_eq!(
            facts_order([FactsOrder::Newest, FactsOrder::Authority]),
            FactsOrder::Authority,
            "one fact list decides the merged order"
        );
        assert_eq!(
            facts_order([FactsOrder::Newest, FactsOrder::Newest]),
            FactsOrder::Newest
        );
        assert_eq!(key_of(FactsOrder::Authority), Key::Authority);
        assert_eq!(key_of(FactsOrder::Newest), Key::Final);
    }

    // ─────────────────────────────── selection ──────────────────────────────

    /// A workspace with one primary and one archived corpus, on disk.
    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, id) in [("vault", "main"), ("archive", "context")] {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).expect("create the corpus");
            std::fs::write(
                root.join(crate::config::CONFIG_FILE),
                format!("corpus_id = \"{id}\"\n"),
            )
            .expect("write the corpus config");
        }
        let path = dir.path().join(crate::workspace::WORKSPACE_FILE);
        std::fs::write(
            &path,
            "[[corpora]]\nid = 'context'\nroot = 'archive'\nrole = 'archived'\n\n\
             [[corpora]]\nid = 'main'\nroot = 'vault'\nrole = 'primary'\n\n\
             [search]\narchived_weight = 0.8\n",
        )
        .expect("write the workspace");
        let workspace = Workspace::load(&path).expect("the workspace loads");
        (dir, workspace)
    }

    #[test]
    fn case_2_1_the_default_reads_the_primary_and_says_what_it_excluded() {
        let (_dir, workspace) = workspace();
        let plan = plan(&workspace, &Choice::parse(None)).expect("the primary resolves");
        assert_eq!(
            plan.corpora
                .iter()
                .map(|(corpus, factor)| (corpus.id.as_str(), *factor))
                .collect::<Vec<_>>(),
            vec![("main", 1.0)]
        );
        assert_eq!(plan.excluded_archived, 1);
        assert_eq!(
            plan.excluded_note().as_deref(),
            Some("1 archived corpus excluded; pass --corpus all to read it")
        );
    }

    #[test]
    fn case_2_2_all_reads_both_and_weights_the_archived_one() {
        let (_dir, workspace) = workspace();
        let plan = plan(&workspace, &Choice::parse(Some("all"))).expect("every corpus resolves");
        assert_eq!(
            plan.corpora
                .iter()
                .map(|(corpus, factor)| (corpus.id.as_str(), *factor))
                .collect::<Vec<_>>(),
            vec![("main", 1.0), ("context", 0.8)],
            "workspace order, primary first"
        );
        assert_eq!(plan.excluded_archived, 0);
        assert_eq!(plan.excluded_note(), None);
    }

    #[test]
    fn case_2_3_a_named_archived_corpus_keeps_its_weight() {
        let (_dir, workspace) = workspace();
        let plan = plan(&workspace, &Choice::parse(Some("context"))).expect("the id resolves");
        assert_eq!(plan.corpora.len(), 1);
        assert_eq!(plan.corpora[0].0.id, "context");
        assert_eq!(
            plan.corpora[0].1, 0.8,
            "naming a corpus is a scope choice, not a claim that its facts are current"
        );
        assert_eq!(plan.excluded_note(), None);
    }

    #[test]
    fn case_2_5_an_unknown_corpus_is_a_usage_error_that_lists_the_ids() {
        let (_dir, workspace) = workspace();
        let error = plan(&workspace, &Choice::parse(Some("nope"))).expect_err("no such corpus");
        let text = error.to_string();
        assert!(text.contains("nope"), "{text}");
        assert!(text.contains("main"), "{text}");
        assert!(text.contains("context"), "{text}");
        assert!(matches!(error, JanusError::Usage { .. }), "{text}");
    }

    #[test]
    fn a_selection_that_needs_a_workspace_is_refused_in_single_corpus_mode() {
        assert!(Choice::parse(None).require_single().is_ok());
        assert!(Choice::parse(Some("primary")).require_single().is_ok());
        for raw in ["all", "context"] {
            let error = Choice::parse(Some(raw))
                .require_single()
                .expect_err("needs a workspace");
            assert!(error.to_string().contains(raw), "{raw}: {error}");
            assert!(matches!(error, JanusError::Usage { .. }), "{raw}: {error}");
        }
    }
}
