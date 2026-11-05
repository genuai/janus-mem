//! `janus moves`: confirmed path changes and display-only continuation hints.
//!
//! A move is invisible in every other report. `ids` answers "what id does this
//! path have now", and `lineage` answers "what replaces what", but neither says
//! that one path became another in this corpus (M5 §3.11).
//!
//! A read, like `ids`: it loads the validated generation, takes no lock, builds
//! nothing, refreshes nothing and writes nothing. With no generation it reports
//! an empty pair of lists.
//!
//! A hint is a guess, never a fact. It is display-only and nothing here applies
//! it: no chain changes and no vault file is touched (D7, D23).

use std::collections::BTreeSet;
use std::io::{self, Write as _};
use std::path::Path;

use chrono::NaiveDate;
use serde::Serialize;

use crate::clock::Clock;
use crate::config::Config;
use crate::error::JanusError;
use crate::id::DocId;
use crate::index::{Snapshot, store};
use crate::scan::API_VERSION;

/// How `janus moves` should be rendered.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Emit one JSON object on stdout instead of the table.
    pub json: bool,
    /// Keep rows whose `last_seen` is on or after this date.
    pub since: Option<NaiveDate>,
}

/// Parses a `--since` value: `YYYY-MM-DD`, `YYYY-MM` or `YYYY`.
///
/// The same accepted forms as `--as-of`, but named for this flag so the error
/// names the value the user typed.
///
/// # Errors
///
/// [`JanusError::Usage`] when the value is not one of the three forms.
pub fn parse_since(raw: &str) -> Result<NaiveDate, JanusError> {
    crate::search::parse_as_of(raw).map_err(|_| JanusError::Usage {
        message: format!(
            "unparseable --since value {:?}; expected YYYY-MM-DD, YYYY-MM or YYYY",
            raw.trim()
        ),
    })
}

/// Runs `janus moves`.
///
/// Results go to stdout, diagnostics to stderr, so `janus moves --json | jq`
/// always parses (§8.2).
///
/// # Errors
///
/// Propagates corpus and config failures. A missing generation is an empty
/// report, never an error: the command only reports what was published.
pub fn run(
    root: &Path,
    config: &Config,
    clock: &dyn Clock,
    options: Options,
) -> Result<(), JanusError> {
    let snapshot = store::validated_snapshot(root, config, clock.today());
    let (confirmed, hints) = snapshot.as_ref().map_or_else(
        || (Vec::new(), Vec::new()),
        |snapshot| rows(snapshot, options.since),
    );

    let mut out = io::stdout().lock();
    if options.json {
        let report = Report {
            api: API_VERSION,
            vault: root.display().to_string(),
            confirmed,
            hints,
        };
        let json = serde_json::to_string_pretty(&report)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // A closed pipe is not a failure (`janus moves --json | head`).
        let _ = writeln!(out, "{json}");
    } else {
        write_human(&mut out, &confirmed, &hints);
    }
    Ok(())
}

// ───────────────────────────────── rows ────────────────────────────────────

/// The `--json` report: one stable object, like every other command's.
#[derive(Debug, Serialize)]
struct Report {
    /// Contract version; see [`API_VERSION`].
    api: u32,
    /// Corpus root, absolute.
    vault: String,
    /// Confirmed path changes, sorted by `(last_seen, id, to)`.
    confirmed: Vec<MoveRow>,
    /// Continuation hints, sorted by `(to, from)`.
    hints: Vec<HintRow>,
}

/// One confirmed path change: the identity survived, so the move is a fact.
#[derive(Debug, Serialize)]
struct MoveRow {
    /// The document id, in hex.
    id: String,
    /// The path the id left.
    from: String,
    /// The path the id took.
    to: String,
    /// The day the new path was first seen.
    first_seen: NaiveDate,
    /// The last day the new path was seen.
    last_seen: NaiveDate,
    /// The rung that matched the id, or `unknown` for a retired document.
    reason: String,
}

/// One display-only guess that `to` continues the vanished `from`.
#[derive(Debug, Serialize)]
struct HintRow {
    /// The new document's id, in hex.
    id: String,
    /// The vanished document's id, in hex.
    previous_id: String,
    /// The vanished document's last path.
    from: String,
    /// The new document's path.
    to: String,
    /// `jaccard` of the two chunk-id sets.
    chunk_jaccard: f32,
    /// `1.0` when the normalised titles are equal.
    title_match: f32,
    /// `1.0` when the date-stripped file stems are equal.
    basename_match: f32,
    /// `jaccard` of `tags ∪ shared_with`.
    tag_jaccard: f32,
    /// The weighted score. Never applied.
    confidence: f32,
    /// The last day the new path was seen; the `--since` key.
    last_seen: NaiveDate,
}

/// Builds both lists from one generation.
fn rows(snapshot: &Snapshot, since: Option<NaiveDate>) -> (Vec<MoveRow>, Vec<HintRow>) {
    let retired: BTreeSet<DocId> = snapshot.tombstones.iter().map(|meta| meta.id).collect();
    let mut confirmed = Vec::new();
    for (id, entries) in &snapshot.path_history {
        let reason = if retired.contains(id) {
            "unknown"
        } else {
            snapshot
                .identity
                .get(id)
                .map_or("unknown", |record| record.tier.as_str())
        };
        for pair in entries.windows(2) {
            let (from, to) = (&pair[0], &pair[1]);
            if from.rel_path == to.rel_path || since.is_some_and(|since| to.last_seen < since) {
                continue;
            }
            confirmed.push(MoveRow {
                id: id.to_hex(),
                from: from.rel_path.clone(),
                to: to.rel_path.clone(),
                first_seen: to.first_seen,
                last_seen: to.last_seen,
                reason: reason.to_owned(),
            });
        }
    }
    confirmed.sort_by(|left, right| {
        left.last_seen
            .cmp(&right.last_seen)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.to.cmp(&right.to))
    });

    let mut hints = Vec::new();
    for doc in &snapshot.docs {
        let Some(hint) = doc.hint else {
            continue;
        };
        let Some(to) = snapshot
            .path_history
            .get(&doc.id)
            .and_then(|entries| entries.last())
        else {
            continue;
        };
        let Some(from) = snapshot
            .path_history
            .get(&hint.id)
            .and_then(|entries| entries.last())
        else {
            continue;
        };
        if since.is_some_and(|since| to.last_seen < since) {
            continue;
        }
        hints.push(HintRow {
            id: doc.id.to_hex(),
            previous_id: hint.id.to_hex(),
            from: from.rel_path.clone(),
            to: to.rel_path.clone(),
            chunk_jaccard: hint.chunk_jaccard,
            title_match: hint.title_match,
            basename_match: hint.basename_match,
            tag_jaccard: hint.tag_jaccard,
            confidence: hint.confidence,
            last_seen: to.last_seen,
        });
    }
    hints.sort_by(|left, right| {
        left.to
            .cmp(&right.to)
            .then_with(|| left.from.cmp(&right.from))
    });
    (confirmed, hints)
}

// ───────────────────────────────── human ───────────────────────────────────

/// Prints one line per row. A hint is marked so it is never read as a fact.
fn write_human(out: &mut impl io::Write, confirmed: &[MoveRow], hints: &[HintRow]) {
    for row in confirmed {
        let _ = writeln!(
            out,
            "{} -> {}  {}  {}  {}",
            row.from, row.to, row.id, row.reason, row.last_seen
        );
    }
    for row in hints {
        let _ = writeln!(
            out,
            "hint  {} -> {}  {:.2}  {}",
            row.from, row.to, row.confidence, row.id
        );
    }
}
