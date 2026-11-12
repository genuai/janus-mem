//! Authority, status, decay and scope scoring (§3.2, §6.6, §6.12, §7.2).
//!
//! Pure: no I/O, no wall clock, no config read. The caller resolves metadata
//! once ([`ScoreDoc`]), supplies the lexical value phase 2 produced, and injects
//! a [`DecayBasis`] that carries `today` and the corpus median.
//!
//! ```text
//! lexical   = squash(s, s_ref)                          §3.3, phase 2
//! semantic  = clamp(dot(query, chunk), 0, 1)             §3.3, M4
//! relevance = (alpha * semantic + (1 - alpha) * lexical) * title_boost
//! authority = type_authority * status_factor            §6.6, §7.2
//! decay     = exp(-ln2 * age_days / half_life_days)     or 1.0
//! final     = relevance * authority * decay * corpus
//! ```
//!
//! `corpus` is `1.0` outside a workspace, and the workspace's `archived_weight`
//! for a row from an archived corpus (M6b §2.4). It is one factor of `final`,
//! applied by [`Breakdown::with_corpus`] after scoring, so a single corpus
//! cannot change another's factors (§6.11).
//!
//! **Status is an inclusion rule, not a weight** (§3.2): [`rank_documents`]
//! drops a document the intent excludes rather than scoring it zero.
//! [`policy`] is the §3.2 table as data, and [`Breakdown::authority`] is the
//! product, so `status_factor` cannot be computed and forgotten.
//!
//! **One row per document** (§3.1): [`rank_documents`] takes `chunk_to_doc`, so
//! the phase-2 [`LexicalIndex`](crate::index::LexicalIndex) stays
//! document-agnostic and phase 4 persists exactly that field as
//! `ChunkMeta.doc`.
//!
//! **Scope is a filter, never a weight** (§7.2): [`ScopeFilter`] implements
//! §6.12 rules 3, 4 and 9. It is also the one query scope set the graph rules
//! read, so a successor matches the query exactly as a document does (§3.6,
//! §3.7).
//!
//! **Status is the effective status** (§6.10 rule 6): [`ScoreInput`] carries the
//! status the graph derived for this query, and the table is consulted with it.
//! The table itself is unchanged — only its input moved.

use std::cmp::Ordering;

use chrono::NaiveDate;
use serde::Serialize;

use crate::config::{TypeSpec, UndatedDecay};
use crate::document::{Document, Status};
use crate::graph::EffectiveStatus;
use crate::id::{ChunkId, DocId};
use crate::vault::resolve::GLOBAL_SCOPE;

use Status::{Active, Deprecated, Draft, Superseded, Unknown};

/// Every declared status, in the order the §3.2 table's cells are listed.
pub const STATUSES: [Status; 5] = [Active, Draft, Deprecated, Superseded, Unknown];

/// Types that are authoritative facts while `active` (§3.6, S-12).
pub const FACT_TYPES: [&str; 3] = ["decision", "spec", "principle"];

/// A query intent (§7.1). Decay and the status gate read the table, not this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Intent {
    /// What is true now: decay on, `active`/`draft`/`unknown` only.
    #[default]
    Current,
    /// What did we ever say: every status, no decay.
    History,
    /// The lineage view: no decay, `draft` excluded.
    Evolution,
    /// Debug/audit: every status including `unknown`, no decay.
    Exhaustive,
}

impl Intent {
    /// Every intent, for table-driven tests and CLI validation.
    pub const ALL: [Self; 4] = [
        Self::Current,
        Self::History,
        Self::Evolution,
        Self::Exhaustive,
    ];
}

/// The scoring view of one document: the only metadata scoring may read.
///
/// Phase 4 persists this shape as `DocMeta`; [`Document`] converts into it, so
/// the view cannot drift from the scan. `type_spec` is already resolved against
/// config (§6.6.1), which is why scoring never reads config itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreDoc<'a> {
    /// Stable document identity (§6.4).
    pub id: DocId,
    /// Vault-relative path, `/`-separated. The comparator's last key (§3.1).
    pub rel_path: &'a str,
    /// Canonical type name, post-alias (§6.6.1).
    pub type_name: &'a str,
    /// Authority, timeless flag and half-life resolved at config load.
    pub type_spec: TypeSpec,
    /// Declared status. Effective status arrives with M5 (§6.10).
    pub status: Status,
    /// Authored date (§6.10).
    pub date: Option<NaiveDate>,
    /// Valid-time start; the decay anchor when present (§6.10, §7.2).
    pub effective_from: Option<NaiveDate>,
    /// Normalised owning scope, `global` when absent (§6.12 rule 1).
    pub scope: &'a str,
    /// Retrieval tags, matched but never part of identity (§6.12 rule 2).
    pub shared_with: &'a [String],
}

impl<'a> From<&'a Document> for ScoreDoc<'a> {
    fn from(doc: &'a Document) -> Self {
        Self {
            id: doc.id,
            rel_path: &doc.rel_path,
            type_name: &doc.type_name,
            type_spec: doc.type_spec,
            status: doc.status,
            date: doc.date,
            effective_from: doc.effective_from,
            scope: &doc.scope,
            shared_with: &doc.shared_with,
        }
    }
}

/// One row of the §3.2 intent table.
///
/// The table is data so a test can iterate every `(intent, status)` cell and
/// adding a row stays one entry rather than an `if` inside [`score`].
#[derive(Debug, Clone, Copy)]
pub struct IntentPolicy {
    /// Intent this row serves.
    pub intent: Intent,
    /// Whether this row is `current`'s `--include-superseded` override.
    pub include_superseded: bool,
    /// Whether time decay applies under this intent.
    pub decay: bool,
    /// Included statuses and their factors. A status absent from this slice is
    /// excluded, not weighted.
    pub statuses: &'static [(Status, f64)],
}

/// The §3.2 table verbatim, including the `--include-superseded` row.
pub const INTENT_TABLE: [IntentPolicy; 5] = [
    IntentPolicy {
        intent: Intent::Current,
        include_superseded: false,
        decay: true,
        statuses: &[(Active, 1.0), (Draft, 0.7), (Unknown, 1.0)],
    },
    IntentPolicy {
        intent: Intent::Current,
        include_superseded: true,
        decay: true,
        statuses: &[
            (Active, 1.0),
            (Draft, 0.7),
            (Unknown, 1.0),
            (Superseded, 1.0),
        ],
    },
    IntentPolicy {
        intent: Intent::History,
        include_superseded: false,
        decay: false,
        statuses: &[
            (Active, 1.0),
            (Draft, 1.0),
            (Deprecated, 1.0),
            (Superseded, 1.0),
            (Unknown, 1.0),
        ],
    },
    IntentPolicy {
        intent: Intent::Evolution,
        include_superseded: false,
        decay: false,
        statuses: &[
            (Active, 1.0),
            (Deprecated, 1.0),
            (Superseded, 1.0),
            (Unknown, 1.0),
        ],
    },
    IntentPolicy {
        intent: Intent::Exhaustive,
        include_superseded: false,
        decay: false,
        statuses: &[
            (Active, 1.0),
            (Draft, 1.0),
            (Deprecated, 1.0),
            (Superseded, 1.0),
            (Unknown, 1.0),
        ],
    },
];

/// Resolves `(intent, include_superseded)` to its table row.
///
/// The flag is an override of `current` only (S-19): under
/// `history`/`evolution`/`exhaustive` it is a no-op, not an error, because those
/// rows already include `superseded` at factor 1.0.
#[must_use]
pub fn policy(intent: Intent, include_superseded: bool) -> &'static IntentPolicy {
    let flag = include_superseded && intent == Intent::Current;
    INTENT_TABLE
        .iter()
        .find(|row| row.intent == intent && row.include_superseded == flag)
        .unwrap_or(&INTENT_TABLE[0])
}

impl IntentPolicy {
    /// The factor for `status`, or `None` when this intent excludes it.
    ///
    /// `None` is the point: exclusion is an inclusion rule, so there is no
    /// number that could quietly rank the document last instead of dropping it.
    #[must_use]
    pub fn status_factor(&self, status: Status) -> Option<f64> {
        self.statuses
            .iter()
            .find(|(included, _)| *included == status)
            .map(|(_, factor)| *factor)
    }

    /// Whether this intent admits `status` at all.
    #[must_use]
    pub fn includes(&self, status: Status) -> bool {
        self.status_factor(status).is_some()
    }
}

/// Every factor of one chunk's score, reported separately (§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Breakdown {
    /// Semantic similarity, `clamp(cosine, 0, 1)`; `0.0` without an embedder.
    pub semantic: f64,
    /// Normalised BM25 value from phase 2 (§3.3).
    pub lexical: f64,
    /// `alpha * semantic + (1 - alpha) * lexical`, before the title boost.
    pub relevance: f64,
    /// The title multiplier applied to `relevance`. `1.0` when no rare query
    /// term is in the title (M4C §3.1).
    pub title_boost: f64,
    /// `AUTHORITY[type]` (§6.6).
    pub type_authority: f64,
    /// `STATUS_FACTOR[status]` for this intent; `0.0` when excluded.
    pub status_factor: f64,
    /// The product `type_authority * status_factor` — what `final` multiplies.
    pub authority: f64,
    /// Time decay, `1.0` when it does not apply.
    pub decay: f64,
    /// The workspace's weight for this row's corpus: `1.0` for a primary corpus
    /// and outside a workspace, `archived_weight` for an archived one (§2.4 of
    /// the M6b plan). Omitted from JSON when it is `1.0`, so a single-corpus
    /// report is byte-identical to the one before federation existed.
    #[serde(rename = "corpus", skip_serializing_if = "is_neutral")]
    pub corpus: f64,
    /// `relevance * authority * decay * corpus`.
    #[serde(rename = "final")]
    pub final_score: f64,
}

impl Breakdown {
    /// Multiplies `final` by one corpus's weight and reports it (§2.4, M6b-2).
    ///
    /// The only place a corpus changes a score. Every other factor keeps the
    /// value its own corpus gave it, so no corpus can influence another's row.
    #[must_use]
    pub fn with_corpus(mut self, factor: f64) -> Self {
        self.corpus = factor;
        self.final_score *= factor;
        self
    }
}

/// Whether a corpus factor is the neutral `1.0` that no report needs to print.
fn is_neutral(factor: &f64) -> bool {
    *factor == 1.0
}

/// The corpus-relative facts an undated document's decay needs (§3.13, M5-17).
///
/// Build it once per query from the whole generation, before any filter. A
/// median built after `--types` or `--scope` would let one filter change another
/// document's score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecayBasis {
    /// The date the query asks about.
    pub today: NaiveDate,
    /// The median age in days of every document with a decay anchor, or `None`
    /// when the generation has no dated document.
    pub undated_age_days: Option<f64>,
    /// The configured rule for a document whose date resolves to nothing.
    pub undated: UndatedDecay,
}

impl DecayBasis {
    /// Builds the basis from every `effective_from ?? date` anchor.
    ///
    /// The age is `today - anchor` in days, clamped at 0. A missing anchor is
    /// skipped. The median is the middle value, or the lower middle for an even
    /// count.
    #[must_use]
    pub fn new(today: NaiveDate, anchors: impl IntoIterator<Item = Option<NaiveDate>>) -> Self {
        let mut ages: Vec<i64> = anchors
            .into_iter()
            .flatten()
            .map(|anchor| (today - anchor).num_days().max(0))
            .collect();
        if ages.is_empty() {
            return Self {
                today,
                undated_age_days: None,
                undated: UndatedDecay::default(),
            };
        }
        ages.sort_unstable();
        let middle = (ages.len() - 1) / 2;
        Self {
            today,
            undated_age_days: Some(ages[middle] as f64),
            undated: UndatedDecay::default(),
        }
    }

    /// Sets the rule an undated document scores under (§6.3, R1-2).
    #[must_use]
    pub fn with_undated(self, undated: UndatedDecay) -> Self {
        Self { undated, ..self }
    }

    /// The rule that actually decided an undated document's decay, or `None`
    /// when no rule applied: `median` has no median to name on a corpus with no
    /// dated document, and a disabled intent reaches neither rule.
    ///
    /// The note and the score both read this, so they cannot disagree.
    #[must_use]
    pub fn undated_rule(&self) -> Option<UndatedDecay> {
        match self.undated {
            UndatedDecay::None => Some(UndatedDecay::None),
            UndatedDecay::Median => self.undated_age_days.map(|_| UndatedDecay::Median),
        }
    }
}

/// Everything one chunk's score needs (§3.2).
#[derive(Debug, Clone, Copy)]
pub struct ScoreInput<'a> {
    /// The chunk's document.
    pub doc: &'a ScoreDoc<'a>,
    /// Normalised lexical value, `[0, 1]` (§3.3).
    pub lexical: f64,
    /// Semantic similarity, `clamp(cosine, 0, 1)` (§7.2).
    pub semantic: f64,
    /// Query intent; selects the decay and inclusion row.
    pub intent: Intent,
    /// The status the graph derived for this query (§3.7). The table is
    /// consulted with it, never with [`ScoreDoc::status`].
    pub effective_status: EffectiveStatus,
    /// Semantic weight in `[0, 1]`; `0.0` is exactly the lexical path (§7.2).
    pub alpha: f64,
    /// Title multiplier in `[1, 2]`, computed once per document by the caller
    /// (M4C §3.1). `1.0` applies no boost.
    pub title_boost: f64,
    /// `--decay-half-life` override; `None` uses the document's type.
    pub half_life_days: Option<f64>,
    /// `--include-superseded`; only widens `current` (S-19).
    pub include_superseded: bool,
    /// The corpus-relative decay facts, built once per query (M5-17).
    pub decay_basis: DecayBasis,
}

/// Scores one chunk against one document (§3.2, §7.2).
///
/// Pure: the only inputs are the arguments. Preconditions, all satisfied by
/// phase-2 output: `lexical`, `semantic` and `alpha` are finite and in
/// `[0, 1]`; `title_boost` is finite and in `[1, 2]` (M4C §3.1).
///
/// An excluded status yields `status_factor = authority = final = 0.0`; callers
/// must drop the document with [`IntentPolicy::includes`] instead of relying on
/// that zero (§3.2).
#[must_use]
pub fn score(input: &ScoreInput<'_>) -> Breakdown {
    let doc = input.doc;
    let policy = policy(input.intent, input.include_superseded);
    let status_factor = policy
        .status_factor(Status::from(input.effective_status))
        .unwrap_or(0.0);
    let blended = input.alpha * input.semantic + (1.0 - input.alpha) * input.lexical;
    let relevance = blended * input.title_boost;
    let authority = doc.type_spec.authority * status_factor;
    let decay = decay(input, policy.decay);

    Breakdown {
        semantic: input.semantic,
        lexical: input.lexical,
        relevance,
        title_boost: input.title_boost,
        type_authority: doc.type_spec.authority,
        status_factor,
        authority,
        decay,
        corpus: 1.0,
        final_score: relevance * authority * decay,
    }
}

/// `exp(-ln2 * age_days / half_life_days)`, or `1.0` when decay does not apply.
///
/// Decay is off when the intent disables it, the type is `timeless`, the
/// half-life is `<= 0` (or `NaN`), or an undated document has no age to use.
/// A dated document's age is `today - (effective_from ?? date)`, clamped at `0`,
/// so a future-dated document decays `1.0`. An undated document's age is the
/// corpus median under `median`, and no age at all under `none` (§6.3).
fn decay(input: &ScoreInput<'_>, enabled: bool) -> f64 {
    let doc = input.doc;
    if !enabled || doc.type_spec.timeless {
        return 1.0;
    }

    let half_life = input.half_life_days.unwrap_or(doc.type_spec.half_life_days);
    // A NaN half-life is "off" as well: an incomparable value must not produce
    // a NaN decay.
    if half_life <= 0.0 || half_life.is_nan() {
        return 1.0;
    }

    let age_days = match doc.effective_from.or(doc.date) {
        Some(anchor) => (input.decay_basis.today - anchor).num_days().max(0) as f64,
        // No date is missing metadata, not age (§6.3). `median` ages the
        // document by the corpus; `none` refuses to invent an age at all.
        None => match input.decay_basis.undated {
            UndatedDecay::None => return 1.0,
            UndatedDecay::Median => match input.decay_basis.undated_age_days {
                Some(median) => median,
                None => return 1.0,
            },
        },
    };
    decay_factor(age_days, half_life)
}

/// `exp(-ln2 * age_days / half_life_days)` for one age in days.
fn decay_factor(age_days: f64, half_life: f64) -> f64 {
    (-std::f64::consts::LN_2 * age_days / half_life).exp()
}

/// One document's best chunk after grouping (§3.1 step 2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoredDoc<'a> {
    /// The document that owns the winning chunk.
    pub doc: ScoreDoc<'a>,
    /// The winning chunk's full breakdown.
    pub breakdown: Breakdown,
    /// The winning chunk's content identity — SPEC's in-document tie-break.
    pub chunk_id: ChunkId,
    /// The winning chunk's position within its document: the occurrence
    /// discriminator when two chunks of one document share a `chunk_id`.
    pub ordinal: u32,
}

/// A chunk occurrence competing for its document's slot (§3.1 step 2).
#[derive(Debug, Clone, Copy)]
struct Candidate {
    breakdown: Breakdown,
    chunk_id: ChunkId,
    ordinal: u32,
}

impl Candidate {
    /// Higher `final` first, then lower `chunk_id`, then lower `ordinal`.
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .breakdown
            .final_score
            .total_cmp(&self.breakdown.final_score)
            .then_with(|| self.chunk_id.cmp(&other.chunk_id))
            .then_with(|| self.ordinal.cmp(&other.ordinal))
    }
}

/// Orders documents for output: the §3.1 total order, exactly.
///
/// `final` desc, `date` desc with missing dates last, argmax `chunk_id` asc,
/// then `DocId` asc, then `rel_path` asc. The last two are the documented
/// extension that makes the order total: `ChunkId` excludes `DocId` by design
/// (D48), so two documents in different scopes can tie on SPEC's three keys.
#[must_use]
pub fn cmp_documents(a: &ScoredDoc<'_>, b: &ScoredDoc<'_>) -> Ordering {
    b.breakdown
        .final_score
        .total_cmp(&a.breakdown.final_score)
        .then_with(|| cmp_date_desc(a.doc.date, b.doc.date))
        .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        .then_with(|| a.doc.id.cmp(&b.doc.id))
        .then_with(|| a.doc.rel_path.cmp(b.doc.rel_path))
}

/// Newest first; a document with no date sorts after every dated one.
///
/// Shared with `janus facts`, whose §3.6 order has the same date key.
#[must_use]
pub fn cmp_date_desc(a: Option<NaiveDate>, b: Option<NaiveDate>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Groups chunks by document, drops chunks that carry no status factor, and
/// sorts (§3.1).
///
/// `chunk_to_doc[i]` indexes `docs`; `chunk_ids[i]` and `chunk_scores[i]`
/// describe the same chunk occurrence. The three slices must be parallel — a
/// length mismatch returns no hits rather than panicking. The grouping ordinal
/// is the chunk's position among that document's occurrences in input order.
///
/// An excluded status scores `status_factor = 0.0`, so the score itself is the
/// check. The declared status on `docs` is not: eligibility reads the effective
/// status, which is a per-query value and lives in the [`ScoreInput`] that
/// produced each breakdown.
///
/// Type, tag and scope filters run on documents before this call: pass only the
/// documents that passed (§3.1 step 3).
#[must_use]
pub fn rank_documents<'a>(
    docs: &[ScoreDoc<'a>],
    chunk_to_doc: &[u32],
    chunk_ids: &[ChunkId],
    chunk_scores: &[Breakdown],
) -> Vec<ScoredDoc<'a>> {
    if chunk_to_doc.len() != chunk_ids.len() || chunk_to_doc.len() != chunk_scores.len() {
        return Vec::new();
    }

    let mut seen = vec![0u32; docs.len()];
    let mut winners: Vec<Option<Candidate>> = vec![None; docs.len()];

    for (index, &doc_index) in chunk_to_doc.iter().enumerate() {
        let doc_index = doc_index as usize;
        let (Some(_doc), Some(ordinal)) = (docs.get(doc_index), seen.get_mut(doc_index)) else {
            continue;
        };
        if chunk_scores[index].status_factor <= 0.0 {
            continue;
        }

        let candidate = Candidate {
            breakdown: chunk_scores[index],
            chunk_id: chunk_ids[index],
            ordinal: *ordinal,
        };
        *ordinal = ordinal.saturating_add(1);

        if let Some(slot) = winners.get_mut(doc_index) {
            let better = match slot {
                Some(current) => candidate.cmp(current) == Ordering::Less,
                None => true,
            };
            if better {
                *slot = Some(candidate);
            }
        }
    }

    let mut ranked: Vec<ScoredDoc<'a>> = winners
        .into_iter()
        .zip(docs)
        .filter_map(|(winner, doc)| {
            let winner = winner?;
            Some(ScoredDoc {
                doc: *doc,
                breakdown: winner.breakdown,
                chunk_id: winner.chunk_id,
                ordinal: winner.ordinal,
            })
        })
        .collect();
    ranked.sort_by(cmp_documents);
    ranked
}

/// `facts(doc)`: an authoritative fact (SPEC §9.2, §3.6, S-12).
///
/// A canonical fact type **and** effective status `active` — deliberately
/// narrower than `current`'s gate, which admits `draft` (0.7×) and `unknown`
/// (1.0×). A weight is not an inclusion rule. The graph supplies the status, so
/// a document declared active but replaced by a link is not a fact (§9
/// correction 11, M5-11). `--include-superseded` does not reach this predicate,
/// the window gate is applied by the caller, and the scope filter is applied
/// separately.
#[must_use]
pub fn is_fact(doc: &ScoreDoc<'_>, status: EffectiveStatus) -> bool {
    status == EffectiveStatus::Active && FACT_TYPES.contains(&doc.type_name)
}

/// A normalised query-side scope set (§6.12 rules 3, 4, 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeFilter {
    scopes: Vec<String>,
    scope_only: bool,
}

impl ScopeFilter {
    /// Normalises, lowercases and deduplicates `scopes` (§6.12 rule 9).
    ///
    /// An empty set is the default: everything matches, nothing is silently
    /// hidden (rule 3). `scope_only` with no scopes has nothing to narrow to, so
    /// it is treated the same way; phase 6 rejects that combination.
    #[must_use]
    pub fn new(scopes: &[String], scope_only: bool) -> Self {
        let mut normalised: Vec<String> = scopes.iter().map(|s| normalise_scope(s)).collect();
        normalised.sort();
        normalised.dedup();
        Self {
            scopes: normalised,
            scope_only,
        }
    }

    /// The normalised scope set, for echoing in output notes.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// Whether `--scope-only` was requested.
    #[must_use]
    pub const fn scope_only(&self) -> bool {
        self.scope_only
    }

    /// Whether a document matches this filter (§6.12 rules 3 and 4).
    ///
    /// Let `S(d) = {doc.scope} ∪ doc.shared_with`. With no filter, everything
    /// matches. Otherwise a document matches when its owning scope is in the
    /// query set, its owning scope is `global`, or one of its shared tags is in
    /// the query set. `--scope-only` keeps only the first: `global` and
    /// shared-only matches are dropped.
    #[must_use]
    pub fn matches(&self, scope: &str, shared_with: &[String]) -> bool {
        if self.scope_only {
            // `--scope-only` narrows the result set (§6.12 rule 4); with no
            // query scope there is nothing to narrow to.
            if self.scopes.is_empty() {
                return true;
            }
            let scope = normalise_scope(scope);
            return scope != GLOBAL_SCOPE && self.scopes.contains(&scope);
        }
        self.intersects(scope, shared_with)
    }

    /// Whether `S(d)` intersects the query scope set: the successor-match rule
    /// of §3.6 and §3.7, and §6.12 rule 7.
    ///
    /// With no query scope the set is universal and everything intersects.
    /// Otherwise a document intersects when its owning scope is `global`, when
    /// its owning scope is in the set, or when one of its shared tags is in the
    /// set. `--scope-only` is deliberately ignored: it narrows the result set
    /// (§6.12 rule 4), never the meaning of `Q`. A global successor always
    /// counts, whatever the caller asked to be shown.
    #[must_use]
    pub fn intersects(&self, scope: &str, shared_with: &[String]) -> bool {
        if self.scopes.is_empty() {
            return true;
        }
        let scope = normalise_scope(scope);
        if scope == GLOBAL_SCOPE || self.scopes.contains(&scope) {
            return true;
        }
        shared_with.iter().any(|tag| {
            let tag = normalise_scope(tag);
            self.scopes.contains(&tag)
        })
    }
}

/// Trims, lowercases and collapses internal whitespace, in that order.
///
/// Both sides of every comparison go through this, so `ORG:Acme` and
/// ` org:acme ` meet as `org:acme` (§6.12 rule 9). Only ASCII case is folded:
/// the valid scope alphabet is ASCII.
///
/// The same algorithm lives privately in `vault::resolve` for document scopes;
/// it is repeated here because phase 3 must not modify M1 code. §12 C-7 records
/// the follow-up that should hoist it into one module.
#[must_use]
pub fn normalise_scope(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Days;

    use std::path::Path;

    use crate::config::Config;

    /// The eval clock from §3.8: every test is frozen in time.
    const TODAY: (i32, u32, u32) = (2025, 4, 1);

    /// A decision's resolved behaviour: 1.5 authority, 180-day half-life.
    const DECISION: TypeSpec = TypeSpec {
        authority: 1.5,
        timeless: false,
        half_life_days: 180.0,
    };

    /// A timeless principle: authority 1.2, never decays.
    const TIMELESS: TypeSpec = TypeSpec {
        authority: 1.2,
        timeless: true,
        half_life_days: 180.0,
    };

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(TODAY.0, TODAY.1, TODAY.2).expect("valid test date")
    }

    /// The basis for a corpus with no dated document: no median age.
    fn basis() -> DecayBasis {
        DecayBasis::new(today(), [])
    }

    /// `TODAY` minus `days`; `days_ago(180)` is one half-life of [`DECISION`].
    fn days_ago(days: u64) -> NaiveDate {
        today() - Days::new(days)
    }

    /// A decision dated exactly one half-life ago, so `decay` is `0.5`.
    fn doc(id: u8, status: Status) -> ScoreDoc<'static> {
        ScoreDoc {
            id: DocId::from_bytes([id; 16]),
            rel_path: "decisions/cache.md",
            type_name: "decision",
            type_spec: DECISION,
            status,
            date: Some(days_ago(180)),
            effective_from: None,
            scope: GLOBAL_SCOPE,
            shared_with: &[],
        }
    }

    fn with_date(doc: ScoreDoc<'static>, date: Option<NaiveDate>) -> ScoreDoc<'static> {
        ScoreDoc { date, ..doc }
    }

    fn with_type(doc: ScoreDoc<'static>, name: &'static str, spec: TypeSpec) -> ScoreDoc<'static> {
        ScoreDoc {
            type_name: name,
            type_spec: spec,
            ..doc
        }
    }

    fn input<'a>(doc: &'a ScoreDoc<'a>, intent: Intent, superseded: bool) -> ScoreInput<'a> {
        ScoreInput {
            doc,
            lexical: 0.5,
            semantic: 0.0,
            intent,
            effective_status: EffectiveStatus::from(doc.status),
            alpha: 0.0,
            title_boost: 1.0,
            half_life_days: None,
            include_superseded: superseded,
            decay_basis: basis(),
        }
    }

    /// Scores under `current`: relevance `0.5`, and decay `0.5` for [`doc`].
    fn score_doc(doc: &ScoreDoc<'_>) -> Breakdown {
        score(&input(doc, Intent::Current, false))
    }

    fn scoped(scopes: &[&str], scope_only: bool) -> ScopeFilter {
        let owned: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
        ScopeFilter::new(&owned, scope_only)
    }

    fn chunk_id(byte: u8) -> ChunkId {
        ChunkId::from_bytes([byte; 32])
    }

    // ───────────────────────────── the §3.2 table ────────────────────────────

    /// One §3.2 row: `(intent, include_superseded, decay)` and five cells.
    type Row = ((Intent, bool, bool), [Option<f64>; 5]);

    /// The §3.2 table transcribed by hand, cells in [`STATUSES`] order.
    const TABLE: [Row; 5] = [
        (
            (Intent::Current, false, true),
            [Some(1.0), Some(0.7), None, None, Some(1.0)],
        ),
        (
            (Intent::Current, true, true),
            [Some(1.0), Some(0.7), None, Some(1.0), Some(1.0)],
        ),
        (
            (Intent::History, false, false),
            [Some(1.0), Some(1.0), Some(1.0), Some(1.0), Some(1.0)],
        ),
        (
            (Intent::Evolution, false, false),
            [Some(1.0), None, Some(1.0), Some(1.0), Some(1.0)],
        ),
        (
            (Intent::Exhaustive, false, false),
            [Some(1.0), Some(1.0), Some(1.0), Some(1.0), Some(1.0)],
        ),
    ];

    #[test]
    fn every_intent_status_cell_matches_the_contract_table() {
        let mut cells = 0;
        for ((intent, flag, decay), expected) in TABLE {
            let row = policy(intent, flag);
            let label = format!("{intent:?}/superseded={flag}");
            assert_eq!(row.intent, intent, "{label}: row intent");
            assert_eq!(row.include_superseded, flag, "{label}: row flag");
            assert_eq!(row.decay, decay, "{label}: decay flag");

            for (status, expected_factor) in STATUSES.iter().zip(expected) {
                cells += 1;
                let cell = format!("{label}/{status}");
                assert_eq!(
                    row.includes(*status),
                    expected_factor.is_some(),
                    "{cell}: inclusion"
                );
                assert_eq!(
                    row.status_factor(*status),
                    expected_factor,
                    "{cell}: factor"
                );

                let scored = score(&input(&doc(1, *status), intent, flag));
                match expected_factor {
                    Some(factor) => {
                        assert_eq!(scored.status_factor, factor, "{cell}: applied");
                        assert_eq!(scored.authority, 1.5 * factor, "{cell}: product");
                        assert_eq!(scored.relevance, 0.5, "{cell}: relevance");
                        let expected_decay = if decay { 0.5 } else { 1.0 };
                        assert!(
                            (scored.decay - expected_decay).abs() < 1e-12,
                            "{cell}: decay {}",
                            scored.decay
                        );
                    }
                    None => {
                        assert_eq!(scored.status_factor, 0.0, "{cell}: no factor");
                        assert_eq!(scored.final_score, 0.0, "{cell}: no score");
                    }
                }
            }
        }
        assert_eq!(cells, 25, "the table covers every intent and status");
    }

    #[test]
    fn every_intent_and_flag_resolves_to_a_row_for_that_intent() {
        for intent in Intent::ALL {
            for flag in [false, true] {
                let row = policy(intent, flag);
                assert_eq!(row.intent, intent);
                assert_eq!(row.include_superseded, flag && intent == Intent::Current);
            }
        }
        assert_eq!(INTENT_TABLE.len(), 5, "four intents plus the override");
    }

    #[test]
    fn include_superseded_is_a_no_op_outside_current() {
        for intent in [Intent::History, Intent::Evolution, Intent::Exhaustive] {
            let plain = policy(intent, false);
            let flagged = policy(intent, true);
            assert_eq!(plain.decay, flagged.decay, "{intent:?}");
            for status in STATUSES {
                assert_eq!(
                    plain.status_factor(status),
                    flagged.status_factor(status),
                    "{intent:?}/{status}"
                );
            }
        }
    }

    // ────────────────────────────── the numbers ──────────────────────────────

    #[test]
    fn a_breakdown_reports_every_factor_as_exact_values() {
        // lexical 0.5, alpha 0, decision authority 1.5, and `history` flattens
        // both the status factor and the decay, so every value is exact.
        let scored = score(&input(&doc(1, Status::Draft), Intent::History, false));
        let expected = Breakdown {
            semantic: 0.0,
            lexical: 0.5,
            relevance: 0.5,
            title_boost: 1.0,
            type_authority: 1.5,
            status_factor: 1.0,
            authority: 1.5,
            decay: 1.0,
            corpus: 1.0,
            final_score: 0.75,
        };
        assert_eq!(scored, expected);
        assert_eq!(
            scored.authority,
            scored.type_authority * scored.status_factor
        );
    }

    #[test]
    fn final_is_the_documented_product_in_the_documented_order() {
        // Relevance 0.61, authority 1.5 and a one-day decay. The left-to-right
        // product differs from `relevance * (authority * decay)` in the last
        // bit, so this pins the documented grouping (§3.2).
        let dated = with_date(doc(1, Status::Active), Some(days_ago(1)));
        let mut input = input(&dated, Intent::Current, false);
        input.lexical = 0.61;
        let scored = score(&input);
        assert_eq!(
            scored.final_score,
            scored.relevance * scored.authority * scored.decay,
            "final must be relevance * authority * decay, evaluated in that order"
        );
    }

    #[test]
    fn draft_is_a_fraction_of_active_and_unknown_is_neutral() {
        let active = score_doc(&doc(1, Status::Active));
        let draft = score_doc(&doc(1, Status::Draft));
        let unknown = score_doc(&doc(1, Status::Unknown));

        // 0.5 lexical * 1.5 authority * 1.0 factor * 0.5 decay = 0.375.
        assert!((active.final_score - 0.375).abs() < 1e-12);
        assert!((draft.final_score / active.final_score - 0.7).abs() < 1e-12);
        assert!((unknown.final_score / active.final_score - 1.0).abs() < 1e-12);
    }

    #[test]
    fn alpha_moves_weight_between_the_two_signals() {
        let doc = doc(1, Status::Active);
        let mut input = input(&doc, Intent::History, false);
        input.semantic = 0.25;
        for (alpha, expected) in [(0.0, 0.5), (1.0, 0.25), (0.5, 0.375)] {
            input.alpha = alpha;
            assert_eq!(score(&input).relevance, expected);
        }
    }

    #[test]
    fn the_alpha_table_matches_hand_computed_relevance() {
        let doc = doc(1, Status::Active);
        let mut input = input(&doc, Intent::History, false);
        input.alpha = 0.65;

        // §3.3's table at α = 0.65: relevance = 0.65·semantic + 0.35·lexical.
        // Row 5 is 0.65·0.4 + 0.35·1.0 = 0.26 + 0.35 = 0.61 (the plan's first
        // draft printed 0.480; P2 corrects it in §3.3 as well).
        for (semantic, lexical, expected) in [
            (0.5, 0.5, 0.500_f64),
            (0.0, 1.0, 0.350),
            (1.0, 0.0, 0.650),
            (0.4, 0.0, 0.260),
            (0.4, 1.0, 0.610),
        ] {
            input.semantic = semantic;
            input.lexical = lexical;
            let relevance = score(&input).relevance;
            assert!(
                (relevance - expected).abs() < 1e-12,
                "semantic {semantic}, lexical {lexical}: expected {expected}, got {relevance}"
            );
        }

        // The endpoint and midpoint identities, checked separately because the
        // table only exercises the default weight.
        for (alpha, semantic, lexical) in [(0.0, 0.4, 0.7), (1.0, 0.4, 0.7), (0.35, 0.4, 0.7)] {
            input.alpha = alpha;
            input.semantic = semantic;
            input.lexical = lexical;
            let expected = alpha * semantic + (1.0 - alpha) * lexical;
            let relevance = score(&input).relevance;
            assert!(
                (relevance - expected).abs() < 1e-12,
                "alpha {alpha}: expected {expected}, got {relevance}"
            );
        }
    }

    #[test]
    fn the_title_boost_multiplies_relevance_and_nothing_else() {
        let doc = doc(1, Status::Active);
        let base = score(&input(&doc, Intent::History, false));
        for boost in [1.0, 1.25, 1.5, 2.0] {
            let boosted = score(&ScoreInput {
                title_boost: boost,
                ..input(&doc, Intent::History, false)
            });
            // semantic, lexical, authority and decay keep their values.
            assert_eq!(boosted.semantic, base.semantic, "boost {boost}");
            assert_eq!(boosted.lexical, base.lexical, "boost {boost}");
            assert_eq!(boosted.authority, base.authority, "boost {boost}");
            assert_eq!(boosted.decay, base.decay, "boost {boost}");
            assert_eq!(boosted.title_boost, boost, "boost {boost}");
            // relevance and final move by exactly the multiplier.
            assert!((boosted.relevance - base.relevance * boost).abs() < 1e-12);
            assert!((boosted.final_score - base.final_score * boost).abs() < 1e-12);
        }
    }

    // ─────────────────────────── inclusion and ranking ───────────────────────

    fn statuses_ranked(intent: Intent, flag: bool, docs: &[ScoreDoc<'static>]) -> Vec<Status> {
        let mapping: Vec<u32> = (0..docs.len() as u32).collect();
        let ids: Vec<ChunkId> = (0..docs.len()).map(|i| chunk_id(i as u8)).collect();
        let scores: Vec<Breakdown> = docs
            .iter()
            .map(|d| score(&input(d, intent, flag)))
            .collect();
        rank_documents(docs, &mapping, &ids, &scores)
            .iter()
            .map(|hit| hit.doc.status)
            .collect()
    }

    fn all_statuses() -> Vec<ScoreDoc<'static>> {
        STATUSES
            .iter()
            .enumerate()
            .map(|(i, status)| doc(i as u8, *status))
            .collect()
    }

    #[test]
    fn status_is_an_inclusion_rule_not_a_zero_weight() {
        let docs = all_statuses();

        // current: 1.0*1.5*0.5 = 0.75 for active and unknown, 0.7*1.5*0.5 =
        // 0.525 for draft. Active wins the tie on chunk id.
        assert_eq!(
            statuses_ranked(Intent::Current, false, &docs),
            vec![Status::Active, Status::Unknown, Status::Draft]
        );
        // history: every factor is 1.0 and decay is off, so all five tie at
        // 0.75 and the chunk-id key keeps the input order.
        assert_eq!(statuses_ranked(Intent::History, false, &docs), STATUSES);
        // The override adds `superseded` at 0.75 and leaves `deprecated` out.
        assert_eq!(
            statuses_ranked(Intent::Current, true, &docs),
            vec![
                Status::Active,
                Status::Superseded,
                Status::Unknown,
                Status::Draft
            ]
        );
        // evolution drops `draft` and keeps the rest.
        assert_eq!(
            statuses_ranked(Intent::Evolution, false, &docs),
            vec![
                Status::Active,
                Status::Deprecated,
                Status::Superseded,
                Status::Unknown
            ]
        );
    }

    #[test]
    fn deprecated_and_superseded_are_absent_under_current_present_under_history() {
        let docs = all_statuses();
        let current = statuses_ranked(Intent::Current, false, &docs);
        assert!(!current.contains(&Status::Deprecated));
        assert!(!current.contains(&Status::Superseded));
        assert!(statuses_ranked(Intent::History, false, &docs).contains(&Status::Deprecated));
        assert!(statuses_ranked(Intent::History, false, &docs).contains(&Status::Superseded));
        let overridden = statuses_ranked(Intent::Current, true, &docs);
        assert!(overridden.contains(&Status::Superseded));
        assert!(!overridden.contains(&Status::Deprecated));
    }

    #[test]
    fn non_current_intents_disable_decay_and_flatten_the_status_factor() {
        let ancient = NaiveDate::from_ymd_opt(1990, 1, 1).expect("valid date");
        for intent in [Intent::History, Intent::Evolution, Intent::Exhaustive] {
            let row = policy(intent, false);
            for status in STATUSES {
                let doc = with_date(doc(1, status), Some(ancient));
                let scored = score(&input(&doc, intent, false));
                assert_eq!(scored.decay, 1.0, "{intent:?}/{status}: decay");
                if row.includes(status) {
                    assert_eq!(scored.status_factor, 1.0, "{intent:?}/{status}: factor");
                }
            }
        }
    }

    // ──────────────────────────────── decay ──────────────────────────────────

    #[test]
    fn decay_decreases_strictly_as_the_document_ages() {
        let mut previous = f64::INFINITY;
        for age in 0..=720u64 {
            let decay = score_doc(&with_date(doc(1, Status::Active), Some(days_ago(age)))).decay;
            assert!(
                decay < previous,
                "age {age}: {decay} is not below {previous}"
            );
            previous = decay;
        }

        // One half-life is 0.5, two are 0.25.
        assert!((score_doc(&doc(1, Status::Active)).decay - 0.5).abs() < 1e-12);
        let two = with_date(doc(1, Status::Active), Some(days_ago(360)));
        assert!((score_doc(&two).decay - 0.25).abs() < 1e-12);
    }

    #[test]
    fn an_undated_document_decays_by_the_corpus_median_age() {
        // Ages 10, 20 and 30 days: the median is 20. Half-life 180:
        // exp(-ln2 · 20 / 180) = 0.9258747.
        let corpus = DecayBasis::new(
            today(),
            [Some(days_ago(10)), Some(days_ago(20)), Some(days_ago(30))],
        );
        assert_eq!(corpus.undated, UndatedDecay::Median, "the shipped default");
        assert_eq!(corpus.undated_age_days, Some(20.0));

        let undated = with_date(doc(1, Status::Active), None);
        let scored = score(&ScoreInput {
            decay_basis: corpus,
            ..input(&undated, Intent::Current, false)
        });
        assert!(
            (scored.decay - 0.925_874_7).abs() < 1e-7,
            "{}",
            scored.decay
        );

        // The lower middle of an even count: [10, 30] takes 10.
        let even = DecayBasis::new(today(), [Some(days_ago(10)), Some(days_ago(30))]);
        assert_eq!(even.undated_age_days, Some(10.0));
    }

    #[test]
    fn the_none_rule_gives_an_undated_document_no_age() {
        // Same corpus and same half-life as the median case: the dated document
        // still decays 0.5, and only the undated one changes, from 0.9259 to 1.
        let corpus = DecayBasis::new(today(), [Some(days_ago(10)), Some(days_ago(20))])
            .with_undated(UndatedDecay::None);
        let undated = with_date(doc(1, Status::Active), None);
        let scored = score(&ScoreInput {
            decay_basis: corpus,
            ..input(&undated, Intent::Current, false)
        });
        assert_eq!(scored.decay, 1.0);
        assert_eq!(
            score_doc(&with_date(doc(1, Status::Active), Some(days_ago(180)))).decay,
            0.5
        );
    }

    #[test]
    fn the_reported_undated_rule_is_the_one_that_scored() {
        let one_dated = [Some(days_ago(10))];
        let none_dated: [Option<NaiveDate>; 1] = [None];

        // `none` always has something to report, because it needs no median.
        let none = DecayBasis::new(today(), none_dated).with_undated(UndatedDecay::None);
        assert_eq!(none.undated_rule(), Some(UndatedDecay::None));

        // `median` reports nothing on a corpus with no dated document: the age
        // it would name does not exist, so the decay is 1.0 and unremarked.
        let empty = DecayBasis::new(today(), none_dated).with_undated(UndatedDecay::Median);
        assert_eq!(empty.undated_rule(), None);

        // A basis built without a rule runs the shipped default, `median`, and
        // reports it when the corpus has a median to name.
        let default = DecayBasis::new(today(), one_dated);
        assert_eq!(default.undated, UndatedDecay::Median);
        assert_eq!(default.undated_rule(), Some(UndatedDecay::Median));
    }

    #[test]
    fn decay_is_one_in_every_documented_short_circuit() {
        let ancient = NaiveDate::from_ymd_opt(1990, 1, 1).expect("valid date");

        // 1. a timeless type, however old the document is.
        assert_eq!(
            score_doc(&with_type(
                with_date(doc(1, Status::Active), Some(ancient)),
                "principle",
                TIMELESS
            ))
            .decay,
            1.0
        );

        // 2. a half-life of zero or less, from the type or from the override.
        for half_life in [0.0, -5.0] {
            let spec = TypeSpec {
                half_life_days: half_life,
                ..DECISION
            };
            assert_eq!(
                score_doc(&with_type(doc(1, Status::Active), "decision", spec)).decay,
                1.0
            );
        }
        let active = doc(1, Status::Active);
        let overridden = ScoreInput {
            half_life_days: Some(0.0),
            ..input(&active, Intent::Current, false)
        };
        assert_eq!(score(&overridden).decay, 1.0);

        // 3. an undated document when the corpus has no dated document.
        assert_eq!(
            score_doc(&with_date(doc(1, Status::Active), None)).decay,
            1.0
        );

        // 4. a future date: the age clamps at zero.
        let future = with_date(doc(1, Status::Active), Some(today() + Days::new(30)));
        assert_eq!(score_doc(&future).decay, 1.0);

        // 5. an intent that disables decay, on a 35-year-old document.
        let old = with_date(doc(1, Status::Active), Some(ancient));
        for intent in [Intent::History, Intent::Evolution, Intent::Exhaustive] {
            assert_eq!(score(&input(&old, intent, false)).decay, 1.0);
        }
    }

    // ───────────────────── the global half-life switch ─────────────────────

    /// A journal dated ten days ago, with its type resolved by `config`.
    fn journal_ten_days_old(config: &Config) -> ScoreDoc<'static> {
        with_type(
            with_date(doc(1, Status::Active), Some(days_ago(10))),
            "journal",
            config.resolve_type("journal").spec,
        )
    }

    /// Parses config text with a fake origin, for tests that need no filesystem.
    fn parse(text: &str) -> Config {
        Config::from_toml(text, Path::new("<test>")).expect("config should parse")
    }

    #[test]
    fn a_zero_global_half_life_turns_decay_off_for_every_type() {
        // `[decay.by_type] journal = 180` cannot keep a journal decaying once
        // the global value turns decay off. Ten days ago costs nothing.
        let config = parse("[decay]\nhalf_life_days = 0\n\n[decay.by_type]\njournal = 180\n");

        assert_eq!(score_doc(&journal_ten_days_old(&config)).decay, 1.0);
        assert!(
            config.resolve_type("principle").spec.timeless,
            "the timeless rule must not depend on the half-life"
        );
    }

    #[test]
    fn a_positive_global_half_life_lets_the_override_set_the_rate() {
        let config = parse("[decay]\nhalf_life_days = 180\n\n[decay.by_type]\njournal = 180\n");

        // ln2 = 0.693147, age 10 days, half-life 180 days:
        // exp(-0.693147 * 10 / 180) = exp(-0.0385082) = 0.962224.
        // The assertion uses 6 decimal places.
        let decay = score_doc(&journal_ten_days_old(&config)).decay;
        assert!((decay - 0.962_224).abs() < 1e-6, "decay was {decay}");
    }

    #[test]
    fn a_zero_global_and_a_zero_override_both_mean_off() {
        let config = parse("[decay]\nhalf_life_days = 0\n\n[decay.by_type]\njournal = 0\n");

        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        assert_eq!(score_doc(&journal_ten_days_old(&config)).decay, 1.0);
    }

    #[test]
    fn effective_from_is_the_decay_anchor_ahead_of_date() {
        let dated = doc(1, Status::Active);
        let anchored = ScoreDoc {
            date: Some(days_ago(3650)),
            effective_from: Some(days_ago(0)),
            ..dated
        };
        // Age zero decays exactly 1.0; the date alone would have decayed.
        assert_eq!(score_doc(&anchored).decay, 1.0);
        assert!(score_doc(&anchored).decay > score_doc(&dated).decay);
    }

    // ───────────────────────────────── scope ─────────────────────────────────

    #[test]
    fn no_scope_filter_is_a_union_over_every_scope() {
        let all = scoped(&[], false);
        assert!(all.scopes().is_empty());
        assert!(!all.scope_only());
        assert!(all.matches(GLOBAL_SCOPE, &[]));
        assert!(all.matches("org:acme", &[]));
        assert!(all.matches("org:beta", &["shared:finance".to_owned()]));
    }

    #[test]
    fn a_scope_filter_keeps_owned_global_and_shared_matches() {
        let acme = scoped(&["org:acme"], false);
        assert!(acme.matches("org:acme", &[]));
        assert!(!acme.matches("org:beta", &[]));
        assert!(acme.matches(GLOBAL_SCOPE, &[]), "global is universal");

        let union = scoped(&["org:acme", "shared:finance"], false);
        assert!(union.matches("org:acme", &[]));
        assert!(union.matches("org:beta", &["shared:finance".to_owned()]));
        assert!(!union.matches("org:beta", &["shared:legal".to_owned()]));
    }

    #[test]
    fn intersects_ignores_scope_only() {
        // The successor-match rule never reads `--scope-only`: a global
        // document and a shared tag intersect the set either way.
        let only = scoped(&["org:acme"], true);
        assert!(only.intersects(GLOBAL_SCOPE, &[]));
        assert!(only.intersects("org:acme", &[]));
        assert!(!only.intersects("org:beta", &[]));
        assert!(only.intersects("org:beta", &["ORG:Acme".to_owned()]));
        assert!(scoped(&[], false).intersects("org:beta", &[]), "universal");
        assert!(
            !only.matches(GLOBAL_SCOPE, &[]),
            "`matches` still applies scope_only"
        );
    }

    #[test]
    fn scope_only_drops_global_and_shared_only_matches() {
        let acme = scoped(&["org:acme"], true);
        assert!(acme.scope_only());
        assert!(acme.matches("org:acme", &[]));
        assert!(!acme.matches(GLOBAL_SCOPE, &[]));

        let union = scoped(&["org:acme", "shared:finance"], true);
        assert!(union.matches("org:acme", &[]));
        assert!(!union.matches("org:beta", &["shared:finance".to_owned()]));
        assert!(!union.matches(GLOBAL_SCOPE, &["shared:finance".to_owned()]));
    }

    #[test]
    fn scopes_normalise_case_and_whitespace_on_both_sides() {
        let acme = scoped(&["ORG:Acme"], false);
        assert_eq!(acme.scopes(), ["org:acme"]);
        assert!(acme.matches("org:acme", &[]));
        assert!(acme.matches("ORG:Acme", &[]));
        assert!(acme.matches("  org:acme  ", &[]));

        let shared = scoped(&["shared:finance"], false);
        assert!(shared.matches("org:beta", &["Shared:Finance".to_owned()]));
        assert_eq!(normalise_scope("  ORG:Acme  "), "org:acme");
    }

    #[test]
    fn the_same_title_in_two_scopes_both_survive() {
        let acme = ScoreDoc {
            id: DocId::from_bytes([1; 16]),
            rel_path: "decisions/q4.md",
            scope: "org:acme",
            ..doc(1, Status::Active)
        };
        let beta = ScoreDoc {
            id: DocId::from_bytes([2; 16]),
            rel_path: "decisions/q4.md",
            scope: "org:beta",
            ..doc(2, Status::Active)
        };
        let docs = [acme, beta];

        let all = scoped(&[], false);
        let survivors = docs
            .iter()
            .filter(|d| all.matches(d.scope, d.shared_with))
            .count();
        assert_eq!(survivors, 2, "the default view hides nothing");

        let only = scoped(&["org:acme"], false);
        let survivors: Vec<&ScoreDoc<'_>> = docs
            .iter()
            .filter(|d| only.matches(d.scope, d.shared_with))
            .collect();
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].id, acme.id);

        // Identical title, date and score share a `ChunkId`, so `DocId` is the
        // key that keeps the order stable (§3.1, §6.12.1 obligation 5).
        let ranked = rank_documents(
            &docs,
            &[0, 1],
            &[chunk_id(9), chunk_id(9)],
            &[score_doc(&acme), score_doc(&beta)],
        );
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].doc.id, acme.id);
    }

    // ───────────────────────────────── facts ─────────────────────────────────

    #[test]
    fn only_active_fact_types_are_facts() {
        let mut cells = 0;
        for status in STATUSES {
            for type_name in FACT_TYPES {
                cells += 1;
                let doc = with_type(doc(1, status), type_name, DECISION);
                assert_eq!(
                    is_fact(&doc, EffectiveStatus::from(status)),
                    status == Status::Active,
                    "{type_name}/{status}"
                );
            }
        }
        for type_name in ["journal", "note", "finding", "idea"] {
            let doc = with_type(doc(1, Status::Active), type_name, DECISION);
            let active = EffectiveStatus::from(Status::Active);
            assert!(!is_fact(&doc, active), "{type_name} is not a fact type");
        }
        assert_eq!(cells, 15, "every status times every fact type");
    }

    #[test]
    fn a_draft_decision_is_searchable_but_not_a_fact() {
        let draft = doc(1, Status::Draft);
        assert!(policy(Intent::Current, false).includes(Status::Draft));
        assert!(!is_fact(&draft, EffectiveStatus::Draft));
        assert!(is_fact(&doc(1, Status::Active), EffectiveStatus::Active));
        assert!(!is_fact(&doc(1, Status::Unknown), EffectiveStatus::Unknown));
    }

    #[test]
    fn the_table_reads_the_effective_status_not_the_declared_one() {
        // `bad/status-lies.md`: declared `active`, replaced by a link. Under
        // `current` the effective status has no factor even though the declared
        // status does, and the override row admits it at 1.0.
        let lying = doc(1, Status::Active);
        let mut input = input(&lying, Intent::Current, false);
        assert_eq!(score(&input).status_factor, 1.0, "declared");

        input.effective_status = EffectiveStatus::Superseded;
        let scored = score(&input);
        assert_eq!(scored.status_factor, 0.0);
        assert_eq!(scored.final_score, 0.0);

        input.include_superseded = true;
        assert_eq!(score(&input).status_factor, 1.0, "overridden");
    }

    // ─────────────────────────────── ordering ────────────────────────────────

    fn hit(id: u8, final_score: f64, chunk: u8) -> ScoredDoc<'static> {
        ScoredDoc {
            doc: doc(id, Status::Active),
            breakdown: Breakdown {
                semantic: 0.0,
                lexical: 0.0,
                relevance: 0.0,
                title_boost: 1.0,
                type_authority: 1.0,
                status_factor: 1.0,
                authority: 1.0,
                decay: 1.0,
                corpus: 1.0,
                final_score,
            },
            chunk_id: chunk_id(chunk),
            ordinal: 0,
        }
    }

    #[test]
    fn the_document_order_uses_all_five_keys_in_order() {
        let base = hit(1, 0.5, 1);

        // final desc
        assert_eq!(cmp_documents(&hit(1, 0.6, 1), &base), Ordering::Less);

        // date desc, no date last
        let newer = ScoredDoc {
            doc: with_date(base.doc, Some(today())),
            ..base
        };
        let older = ScoredDoc {
            doc: with_date(base.doc, Some(days_ago(10))),
            ..base
        };
        let undated = ScoredDoc {
            doc: with_date(base.doc, None),
            ..base
        };
        assert_eq!(cmp_documents(&newer, &older), Ordering::Less);
        assert_eq!(cmp_documents(&older, &undated), Ordering::Less);
        assert_eq!(cmp_documents(&undated, &older), Ordering::Greater);

        // argmax chunk_id asc
        assert_eq!(
            cmp_documents(&hit(1, 0.5, 1), &hit(1, 0.5, 2)),
            Ordering::Less
        );
        // DocId asc
        assert_eq!(
            cmp_documents(&hit(1, 0.5, 1), &hit(2, 0.5, 1)),
            Ordering::Less
        );
        // rel_path asc, the backstop
        let first = ScoredDoc {
            doc: ScoreDoc {
                rel_path: "a.md",
                ..base.doc
            },
            ..base
        };
        let second = ScoredDoc {
            doc: ScoreDoc {
                rel_path: "b.md",
                ..base.doc
            },
            ..base
        };
        assert_eq!(cmp_documents(&first, &second), Ordering::Less);
    }

    #[test]
    fn the_comparator_has_no_ties_between_distinct_documents() {
        let hits = [
            hit(1, 0.5, 1),
            hit(2, 0.5, 1),
            hit(1, 0.5, 2),
            hit(1, 0.6, 1),
            hit(1, 0.4, 1),
        ];
        for a in &hits {
            for b in &hits {
                assert_eq!(
                    cmp_documents(a, b),
                    cmp_documents(b, a).reverse(),
                    "antisymmetry"
                );
                if a != b {
                    assert_ne!(cmp_documents(a, b), Ordering::Equal, "total order");
                }
            }
        }
    }

    #[test]
    fn tied_documents_sort_identically_whatever_the_input_order() {
        let acme = ScoreDoc {
            id: DocId::from_bytes([1; 16]),
            rel_path: "decisions/q4.md",
            scope: "org:acme",
            ..doc(1, Status::Active)
        };
        let beta = ScoreDoc {
            id: DocId::from_bytes([2; 16]),
            rel_path: "decisions/q4.md",
            scope: "org:beta",
            ..doc(2, Status::Active)
        };
        let scores = [score_doc(&acme), score_doc(&beta)];
        let ids = [chunk_id(9), chunk_id(9)];

        let forward = rank_documents(&[acme, beta], &[0, 1], &ids, &scores);
        let reversed = rank_documents(&[beta, acme], &[1, 0], &ids, &scores);
        let order = |hits: &[ScoredDoc<'_>]| hits.iter().map(|h| h.doc.id).collect::<Vec<_>>();

        assert_eq!(order(&forward), order(&reversed));
        assert_eq!(order(&forward), vec![acme.id, beta.id]);
    }

    #[test]
    fn a_document_keeps_its_highest_scoring_chunk() {
        let doc = doc(1, Status::Active);
        let breakdown = score_doc(&doc);
        let strong = Breakdown {
            final_score: 0.9,
            ..breakdown
        };
        let weak = Breakdown {
            final_score: 0.1,
            ..breakdown
        };

        let hits = rank_documents(
            &[doc],
            &[0, 0],
            &[chunk_id(1), chunk_id(2)],
            &[weak, strong],
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].breakdown.final_score, 0.9);
        assert_eq!(hits[0].chunk_id, chunk_id(2));
    }

    #[test]
    fn an_equal_score_tie_picks_the_lower_chunk_id_then_the_lower_ordinal() {
        let doc = doc(1, Status::Active);
        let breakdown = score_doc(&doc);

        // Input order [2, 1]: the lower chunk id wins, at ordinal 1.
        let hits = rank_documents(
            &[doc],
            &[0, 0],
            &[chunk_id(2), chunk_id(1)],
            &[breakdown, breakdown],
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk_id, chunk_id(1));
        assert_eq!(hits[0].ordinal, 1);

        // Identical ids (expected, CH-10): the lower ordinal wins.
        let hits = rank_documents(
            &[doc],
            &[0, 0],
            &[chunk_id(1), chunk_id(1)],
            &[breakdown, breakdown],
        );
        assert_eq!(hits[0].ordinal, 0);
    }

    #[test]
    fn mismatched_parallel_slices_rank_nothing_instead_of_panicking() {
        let doc = doc(1, Status::Active);
        let scores = [score_doc(&doc), score_doc(&doc)];
        let hits = rank_documents(&[doc], &[0, 0], &[chunk_id(1)], &scores);
        assert!(hits.is_empty());
    }

    // ─────────────────────────── resolved document ───────────────────────────

    #[test]
    fn a_resolved_document_converts_into_the_scoring_view() {
        use crate::config::Config;
        use crate::vault::DiscoveredFile;
        use crate::vault::resolve::resolve;
        use std::fs;
        use std::path::Path;
        use std::time::SystemTime;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("decisions/2024-10-03-cache.md");
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        let text = "---\ntype: adr\nstatus: draft\nscope: ORG:Acme\nshared_with: [shared:finance]\ndate: 2024-10-03\n---\nbody\n";
        fs::write(&path, text).expect("file");

        let file = DiscoveredFile {
            path: path.clone(),
            rel_path: "decisions/2024-10-03-cache.md".to_owned(),
            size: text.len() as u64,
            mtime: SystemTime::UNIX_EPOCH,
        };
        let config = Config::from_toml("", Path::new("<test>")).expect("config");
        let document = resolve(&file, &config).expect("resolve");

        let view = ScoreDoc::from(&document);
        assert_eq!(view.rel_path, "decisions/2024-10-03-cache.md");
        assert_eq!(view.type_name, "decision", "the adr alias is resolved");
        assert_eq!(view.type_spec.authority, 1.5);
        assert_eq!(view.status, Status::Draft);
        assert_eq!(view.scope, "org:acme", "resolution normalises the scope");
        assert_eq!(
            view.date,
            Some(NaiveDate::from_ymd_opt(2024, 10, 3).expect("date"))
        );
        let status = EffectiveStatus::from(view.status);
        assert!(!is_fact(&view, status), "a draft decision is never a fact");
    }

    #[test]
    fn an_undated_document_with_no_dated_corpus_does_not_decay() {
        let undated = with_date(doc(1, Status::Active), None);
        let dated = with_date(doc(1, Status::Active), Some(days_ago(3650)));

        // An empty basis has no median, so there is no age to invent.
        assert_eq!(score_doc(&undated).decay, 1.0);
        assert!(score_doc(&dated).decay < 1.0);

        // On an exact final-score tie, the undated document goes last.
        let base = hit(1, 0.5, 1);
        let undated_hit = ScoredDoc {
            doc: with_date(base.doc, None),
            ..base
        };
        assert_eq!(cmp_documents(&base, &undated_hit), Ordering::Less);
        assert_eq!(cmp_documents(&undated_hit, &base), Ordering::Greater);
    }

    // ──────────────────────────────── property ───────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn scoring_is_bounded_and_finite(
            lexical in 0.0f64..=1.0,
            semantic in 0.0f64..=1.0,
            alpha in 0.0f64..=1.0,
            title_boost in 1.0f64..=2.0,
            authority in 0.8f64..=1.5,
            half_life in 0.0f64..=3650.0,
            age in 0i64..=20_000,
            status in prop::sample::select(STATUSES.to_vec()),
            intent in prop::sample::select(Intent::ALL.to_vec()),
            superseded in any::<bool>(),
        ) {
            let dated = ScoreDoc {
                date: Some(today() - Days::new(age as u64)),
                type_spec: TypeSpec { authority, half_life_days: half_life, ..DECISION },
                ..doc(1, status)
            };
            let breakdown = score(&ScoreInput {
                doc: &dated,
                lexical,
                semantic,
                alpha,
                title_boost,
                intent,
                effective_status: EffectiveStatus::from(dated.status),
                half_life_days: None,
                include_superseded: superseded,
                decay_basis: basis(),
            });

            prop_assert!(breakdown.final_score >= 0.0);
            prop_assert!(breakdown.final_score <= 3.0);
            prop_assert!(breakdown.final_score.is_finite());
            prop_assert!(breakdown.relevance.is_finite() && breakdown.relevance <= 2.0);
            prop_assert!(breakdown.title_boost.is_finite() && breakdown.title_boost <= 2.0);
            prop_assert!(breakdown.decay.is_finite() && breakdown.decay <= 1.0);
            prop_assert!(breakdown.authority.is_finite() && breakdown.authority <= 1.5);
        }
    }
}
