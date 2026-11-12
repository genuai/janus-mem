//! Build-time resolution of declared supersession links (M5 §3.2, §3.3).
//!
//! A link is a **lookup**, never a guess, and it reads one generation only.
//! Resolution order is fixed: current path → any path ever held (the alias map
//! derived from `path_history`) → declared `id` → dangling. No file is opened
//! and no path is confined against the filesystem, so a link that names nothing
//! costs one failed lookup and yields a [`DanglingLink`], never an error.
//!
//! Both declarations produce one edge each way: `A.supersedes = B` and
//! `B.superseded_by = A` both mean "B replaces A". The pair is de-duplicated and
//! sorted, so a build is deterministic and traversal never cares which side
//! declared the link (§3.3).
//!
//! Text forms. A value is a vault-relative path or a declared `id`. A leading
//! `corpus:` names a corpus. This corpus's own id is stripped and the rest
//! resolves locally. Any other id is [`DanglingReason::OtherCorpus`] at build
//! time: a build reads one corpus and cannot see another one. An absolute path, a
//! path with `..`, or a path with a drive letter is [`DanglingReason::NotFound`].
//!
//! # Cross-corpus links at query time (M7-37)
//!
//! A dangling row keeps the text it was written with, so a query that reads more
//! than one corpus can resolve a qualified link later, from the generation it
//! already holds. [`qualified`] does that: it answers one [`Foreign`] node per
//! link that resolved, and one [`Ignored`] row per link the query did not
//! follow. The policy that allows a link to cross is a workspace fact, so it is
//! applied here and never at build time: flipping it needs no rebuild
//! (case 7.7).
//!
//! A qualified link is followed in the `supersedes` direction: the document that
//! declares the link replaces the document it names. The snapshot keeps no
//! direction on a dangling row, and `promote` writes only `supersedes` (M7-58).

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Config;
use crate::id::DocId;
use crate::index::{
    DanglingLink, DanglingReason, Edge, EdgeKind, IdentityRecord, MatchKey, PathEntry, Snapshot,
};
use crate::vault::identity::alias_map;

/// One document's supersession declarations, as the resolver reads them.
///
/// A view, not the whole document: the resolver needs the id, the path and the
/// two declared lists, and nothing else. That keeps it a pure function of the
/// generation's tables.
pub struct Declared<'a> {
    /// The id this document resolved to in this build.
    pub id: DocId,
    /// Vault-relative, `/`-separated path.
    pub rel_path: &'a str,
    /// Values from `supersedes`: documents this one replaces.
    pub supersedes: &'a [String],
    /// Values from `superseded_by`: documents that replace this one.
    pub superseded_by: &'a [String],
}

/// Resolves every declaration in one generation into edges and dangling rows.
///
/// The output is sorted: `edges` by `(from, to)`, `dangling_links` by
/// `(from, target, reason)`. Both are de-duplicated, so two declarations of the
/// same relation give one row.
#[must_use]
pub fn resolve(
    documents: &[Declared<'_>],
    path_history: &BTreeMap<DocId, Vec<PathEntry>>,
    identity: &BTreeMap<DocId, IdentityRecord>,
    corpus_id: &str,
) -> (Vec<Edge>, Vec<DanglingLink>) {
    let mut live: BTreeMap<&str, DocId> = BTreeMap::new();
    for document in documents {
        live.insert(document.rel_path, document.id);
    }
    let tables = Tables {
        live,
        alias: alias_map(path_history),
        declared: declared_ids(identity),
    };

    let mut pairs: BTreeSet<(DocId, DocId)> = BTreeSet::new();
    let mut dangling: BTreeSet<DanglingLink> = BTreeSet::new();
    for document in documents {
        for target in document.supersedes {
            // `supersedes: X` means this document replaces X.
            record(
                &tables,
                corpus_id,
                document.id,
                target,
                true,
                &mut pairs,
                &mut dangling,
            );
        }
        for target in document.superseded_by {
            record(
                &tables,
                corpus_id,
                document.id,
                target,
                false,
                &mut pairs,
                &mut dangling,
            );
        }
    }

    let edges = pairs
        .into_iter()
        .map(|(from, to)| Edge {
            from,
            to,
            kind: EdgeKind::Supersedes,
        })
        .collect();
    (edges, dangling.into_iter().collect())
}

/// The three tables a link text is looked up in, in resolution order.
struct Tables<'a> {
    /// Current path → id, for the documents this generation sees.
    live: BTreeMap<&'a str, DocId>,
    /// Every path ever held → id, tombstoned paths included.
    alias: BTreeMap<&'a str, DocId>,
    /// Declared `id` text → id.
    declared: BTreeMap<&'a str, DocId>,
}

impl<'a> Tables<'a> {
    /// The tables of one published generation, for a link resolved after the
    /// build (M7-37).
    ///
    /// A build reads the documents it walks and this reads the snapshot, so the
    /// two constructors build the same three tables from the two views of one
    /// generation.
    fn of(snapshot: &'a Snapshot) -> Self {
        let pool = &snapshot.strings;
        let live: BTreeMap<&str, DocId> = snapshot
            .docs
            .iter()
            .filter_map(|doc| Some((pool.get(doc.rel_path)?, doc.id)))
            .collect();
        Self {
            live,
            alias: alias_map(&snapshot.path_history),
            declared: declared_ids(&snapshot.identity),
        }
    }

    /// One lookup chain. A failure is a reason, not an error.
    fn resolve(&self, text: &str, corpus_id: &str) -> Result<DocId, DanglingReason> {
        let text = text.trim();
        let text = match split_qualifier(text) {
            Some((corpus, rest)) if corpus == corpus_id => rest,
            // A qualifier that is not this corpus names another one. Resolving
            // it is M6's job, so it is reported and never guessed (§6.11).
            Some(_) => return Err(DanglingReason::OtherCorpus),
            None => text,
        };
        if !is_confined(text) {
            return Err(DanglingReason::NotFound);
        }
        if let Some(id) = self.live.get(text) {
            return Ok(*id);
        }
        if let Some(id) = self.alias.get(text) {
            return Ok(*id);
        }
        if let Some(id) = self.declared.get(text) {
            return Ok(*id);
        }
        Err(DanglingReason::NotFound)
    }
}

/// Records one declaration as an edge pair or a dangling row.
fn record(
    tables: &Tables<'_>,
    corpus_id: &str,
    declarer: DocId,
    target: &str,
    replaces: bool,
    pairs: &mut BTreeSet<(DocId, DocId)>,
    dangling: &mut BTreeSet<DanglingLink>,
) {
    match tables.resolve(target, corpus_id) {
        // `from` is the document the declaration replaces, whichever side
        // wrote it (M5-6). The dates never decide the direction.
        Ok(other) if replaces => {
            pairs.insert((other, declarer));
        }
        Ok(other) => {
            pairs.insert((declarer, other));
        }
        Err(reason) => {
            dangling.insert(DanglingLink {
                from: declarer,
                target: target.trim().to_owned(),
                reason,
            });
        }
    }
}

/// A declared id is a link target whatever document wrote it.
///
/// The identity record is the only table that keeps the text a retired document
/// answered to, so a link to a vanished document with an `id` still resolves.
fn declared_ids(identity: &BTreeMap<DocId, IdentityRecord>) -> BTreeMap<&str, DocId> {
    let mut declared: BTreeMap<&str, DocId> = BTreeMap::new();
    for (id, record) in identity {
        for key in &record.keys {
            if let MatchKey::DeclaredId(text) = key {
                declared.insert(text.as_str(), *id);
            }
        }
    }
    declared
}

// ─────────────────────── cross-corpus links (M7-37) ────────────────────────

/// One corpus a query may follow a qualified link into (§6.11, M7-37).
#[derive(Debug, Clone, Copy)]
pub struct Corpus<'a> {
    /// The workspace's id for this corpus, which is the qualifier a link names.
    pub id: &'a str,
    /// The generation the link resolves against.
    pub snapshot: &'a Snapshot,
    /// The corpus's own config, so a reader renders its rows as that corpus
    /// reads them and not with the other corpus's aliases (§6.11).
    pub config: &'a Config,
}

/// The corpora one query reads, and the policy that lets a link cross (D42).
///
/// A corpus the query left out is not in `selected`, so its links end the chain
/// and it needs no generation: a read never opens a corpus the query excluded
/// (case 7.8).
#[derive(Debug, Clone, Default)]
pub struct Corpora<'a> {
    /// True when the workspace allows a qualified link to resolve.
    pub allowed: bool,
    /// The corpora this query reads, by workspace id.
    pub selected: Vec<Corpus<'a>>,
    /// Every id the workspace holds, so a miss names what exists instead of
    /// guessing (§6.11). Empty in single-corpus mode.
    pub known: Vec<&'a str>,
}

impl Corpora<'_> {
    /// No other corpus: single-corpus mode, where every qualified link is inert.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            allowed: false,
            selected: Vec::new(),
            known: Vec::new(),
        }
    }
}

/// One qualified link that resolved in another corpus (case 7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Foreign {
    /// Index into [`Corpora::selected`] of the corpus the link resolved in.
    pub corpus: usize,
    /// The document that declared the link, in this generation.
    pub declarer: DocId,
    /// The document it named, in that corpus's generation.
    pub target: DocId,
}

/// Why a query did not follow one qualified link (cases 7.4, 7.7, 7.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The workspace does not let a qualified link resolve.
    Disabled,
    /// The workspace holds no corpus with that id.
    Unknown,
    /// The corpus is in the workspace and this query did not read it.
    NotRead,
    /// The path resolves to nothing in that corpus.
    NotFound,
}

/// One qualified link a query did not follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ignored {
    /// The document that declared the link.
    pub declarer: DocId,
    /// The corpus the qualifier named.
    pub corpus: String,
    /// The text the frontmatter carried, unchanged.
    pub target: String,
    /// Why the query stopped there.
    pub reason: Skip,
}

impl Ignored {
    /// The `doctor` code this row is reported under (§3.10).
    ///
    /// A link to a corpus that cannot be reached is still a link that crosses a
    /// corpus, and a path that misses inside a corpus the query read is the
    /// `dangling_link` a same-corpus miss is.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self.reason {
            Skip::NotFound => "dangling_link",
            Skip::Disabled | Skip::Unknown | Skip::NotRead => "cross_corpus_link_unsupported",
        }
    }

    /// One sentence a reader can act on (case 7.8).
    ///
    /// `known` is the ids the workspace holds, so a misspelt qualifier is a
    /// question the message already answers.
    #[must_use]
    pub fn message(&self, known: &[&str]) -> String {
        match self.reason {
            Skip::Disabled => format!("a link names another corpus: {}", self.target),
            Skip::Unknown => format!(
                "a link names corpus {:?}, which this workspace does not hold; it holds {}",
                self.corpus,
                list(known)
            ),
            Skip::NotRead => format!(
                "a link names corpus {:?}, which this query does not read: {}",
                self.corpus, self.target
            ),
            Skip::NotFound => format!(
                "a link into corpus {:?} does not resolve: {}",
                self.corpus, self.target
            ),
        }
    }
}

/// The ids of every corpus a message can name, sorted and joined.
fn list(known: &[&str]) -> String {
    if known.is_empty() {
        return "no corpus".to_owned();
    }
    let mut ids: Vec<&str> = known.to_vec();
    ids.sort_unstable();
    ids.join(", ")
}

/// Resolves one generation's qualified links against the corpora a query reads.
///
/// Pure: it reads two generations and no file. Both answers are sorted by the
/// order the dangling rows already hold, so a report is stable across runs.
#[must_use]
pub fn qualified(snapshot: &Snapshot, corpora: &Corpora<'_>) -> (Vec<Foreign>, Vec<Ignored>) {
    let mut foreign = Vec::new();
    let mut ignored = Vec::new();
    // A link is only followed when the policy allows it, so nothing is built for
    // the closed case (D42, case 7.7).
    let tables: Vec<Tables<'_>> = if corpora.allowed {
        corpora
            .selected
            .iter()
            .map(|corpus| Tables::of(corpus.snapshot))
            .collect()
    } else {
        Vec::new()
    };
    for row in &snapshot.dangling_links {
        if row.reason != DanglingReason::OtherCorpus {
            continue;
        }
        // The row was written by a qualifier, so the split succeeds. A row that
        // somehow has no qualifier stays unresolved rather than guessed.
        let Some((corpus, rest)) = split_qualifier(&row.target) else {
            continue;
        };
        let mut skip = |reason| {
            ignored.push(Ignored {
                declarer: row.from,
                corpus: corpus.to_owned(),
                target: row.target.clone(),
                reason,
            });
        };
        if !corpora.allowed {
            skip(Skip::Disabled);
            continue;
        }
        if !corpora.known.contains(&corpus) {
            skip(Skip::Unknown);
            continue;
        }
        let Some((index, found)) = corpora
            .selected
            .iter()
            .enumerate()
            .find(|(_, found)| found.id == corpus)
        else {
            skip(Skip::NotRead);
            continue;
        };
        // The qualifier is already off, so the rest resolves as a local link in
        // that corpus. The graphs stay separate: this adds no edge to the other
        // corpus's own answers (§0.2, M7-37).
        match tables[index].resolve(rest, &found.snapshot.corpus_id) {
            Ok(target) => foreign.push(Foreign {
                corpus: index,
                declarer: row.from,
                target,
            }),
            Err(_) => skip(Skip::NotFound),
        }
    }
    (foreign, ignored)
}

/// Splits a `corpus:` qualifier off a link text.
///
/// A single letter before the colon is a Windows drive, not a corpus id, so it
/// stays part of the path and later fails the confinement test. An empty
/// remainder is not a qualified link either.
pub(crate) fn split_qualifier(text: &str) -> Option<(&str, &str)> {
    let (head, rest) = text.split_once(':')?;
    let id_like = !head.is_empty()
        && !head
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_whitespace())
        && !(head.len() == 1 && head.starts_with(|c: char| c.is_ascii_alphabetic()));
    (id_like && !rest.is_empty()).then_some((head, rest))
}

/// True when a link text stays inside the corpus root, lexically.
///
/// The test is textual on purpose: a link is resolved against the generation,
/// so the filesystem is never consulted and a path that walks upward is refused
/// before any lookup (§3.2).
fn is_confined(text: &str) -> bool {
    if text.is_empty() || text.starts_with('/') || text.starts_with('\\') {
        return false;
    }
    // `C:\x` and `C:/x` are absolute on a host with drive letters.
    if text.as_bytes().get(1) == Some(&b':') {
        return false;
    }
    !text.split(['/', '\\']).any(|part| part == "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::IdentityTier;
    use crate::index::IdentityRecord;

    fn id(byte: u8) -> DocId {
        DocId::from_bytes([byte; crate::id::DOC_ID_LEN])
    }

    /// A document's declarations, with the lists owned by the caller.
    fn declared<'a>(
        id: DocId,
        path: &'a str,
        up: &'a [String],
        down: &'a [String],
    ) -> Declared<'a> {
        Declared {
            id,
            rel_path: path,
            supersedes: up,
            superseded_by: down,
        }
    }

    fn values(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    /// A history table with one live path per document, in the given order.
    fn history(entries: &[(DocId, &str)]) -> BTreeMap<DocId, Vec<PathEntry>> {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date");
        entries
            .iter()
            .map(|(id, path)| {
                (
                    *id,
                    vec![PathEntry {
                        rel_path: (*path).to_owned(),
                        first_seen: day,
                        last_seen: day,
                        tombstone: false,
                    }],
                )
            })
            .collect()
    }

    /// An identity table whose records declare the given ids.
    fn identity(entries: &[(DocId, &str)]) -> BTreeMap<DocId, IdentityRecord> {
        entries
            .iter()
            .map(|(id, text)| {
                (
                    *id,
                    IdentityRecord {
                        tier: IdentityTier::DeclaredId,
                        keys: vec![MatchKey::DeclaredId((*text).to_owned())],
                    },
                )
            })
            .collect()
    }

    /// One declaration against one generation, with the default corpus id.
    fn resolved(
        documents: &[Declared<'_>],
        path_history: &BTreeMap<DocId, Vec<PathEntry>>,
        identity: &BTreeMap<DocId, IdentityRecord>,
    ) -> (Vec<Edge>, Vec<DanglingLink>) {
        resolve(documents, path_history, identity, "graph")
    }

    #[test]
    fn case_2_3_a_local_corpus_qualifier_resolves() {
        // `graph:` is the corpus id, so the qualifier is stripped and the path
        // resolves locally. Expected edge: from redis (12) to the declarer (11).
        let up = values(&["graph:decisions/2024-12-01-redis-cache.md"]);
        let docs = [declared(id(11), "decisions/new.md", &up, &[])];
        let (edges, dangling) = resolved(
            &docs,
            &history(&[(id(12), "decisions/2024-12-01-redis-cache.md")]),
            &BTreeMap::new(),
        );

        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!((edges[0].from, edges[0].to), (id(12), id(11)));
        assert!(dangling.is_empty(), "{dangling:?}");
    }

    #[test]
    fn case_2_4_a_foreign_corpus_qualifier_is_dangling() {
        // `other:` names a corpus this generation cannot see. Expected: one
        // dangling row with `OtherCorpus`, and no edge.
        let up = values(&["other:decisions/x.md"]);
        let docs = [declared(id(11), "decisions/new.md", &up, &[])];
        let (edges, dangling) = resolved(&docs, &BTreeMap::new(), &BTreeMap::new());

        assert!(edges.is_empty(), "{edges:?}");
        assert_eq!(dangling.len(), 1, "{dangling:?}");
        assert_eq!(dangling[0].from, id(11));
        assert_eq!(dangling[0].target, "other:decisions/x.md");
        assert_eq!(dangling[0].reason, DanglingReason::OtherCorpus);
    }

    #[test]
    fn case_2_5_a_parent_path_is_dangling_and_opens_no_file() {
        // The resolver reads tables only, so this must fail even though
        // `../../etc/passwd` exists on the machine that runs the test.
        let up = values(&["../../etc/passwd"]);
        let docs = [declared(id(11), "decisions/new.md", &up, &[])];
        let (edges, dangling) = resolved(&docs, &BTreeMap::new(), &BTreeMap::new());

        assert!(edges.is_empty(), "{edges:?}");
        assert_eq!(dangling.len(), 1, "{dangling:?}");
        assert_eq!(dangling[0].reason, DanglingReason::NotFound);
    }

    #[test]
    fn case_3_1_one_declared_side_produces_one_edge() {
        // Only `superseded_by` is declared: one-sided.md (21) says newer.md
        // (22) replaces it. Expected edge (21, 22); newer.md declares nothing.
        let down = values(&["bad/newer.md"]);
        let docs = [
            declared(id(21), "bad/one-sided.md", &[], &down),
            declared(id(22), "bad/newer.md", &[], &[]),
        ];
        let (edges, dangling) = resolved(
            &docs,
            &history(&[(id(21), "bad/one-sided.md"), (id(22), "bad/newer.md")]),
            &BTreeMap::new(),
        );

        assert!(dangling.is_empty(), "{dangling:?}");
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!((edges[0].from, edges[0].to), (id(21), id(22)));
    }

    #[test]
    fn case_3_2_both_sides_declare_one_edge() {
        // A (31).superseded_by = B (32) and B.supersedes = A both mean
        // "B replaces A". Expected one edge (31, 32), not two.
        let down = values(&["b.md"]);
        let up = values(&["a.md"]);
        let docs = [
            declared(id(31), "a.md", &[], &down),
            declared(id(32), "b.md", &up, &[]),
        ];
        let (edges, _) = resolved(
            &docs,
            &history(&[(id(31), "a.md"), (id(32), "b.md")]),
            &BTreeMap::new(),
        );

        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!((edges[0].from, edges[0].to), (id(31), id(32)));
    }

    #[test]
    fn case_3_3_two_documents_supersede_one_target() {
        // one-sided.md (41) and status-lies.md (42) both name newer.md (43) as
        // their successor. Expected two edges with the same `to`.
        let first = values(&["bad/newer.md"]);
        let second = values(&["bad/newer.md"]);
        let docs = [
            declared(id(41), "bad/one-sided.md", &[], &first),
            declared(id(42), "bad/status-lies.md", &[], &second),
            declared(id(43), "bad/newer.md", &[], &[]),
        ];
        let (edges, _) = resolved(
            &docs,
            &history(&[
                (id(41), "bad/one-sided.md"),
                (id(42), "bad/status-lies.md"),
                (id(43), "bad/newer.md"),
            ]),
            &BTreeMap::new(),
        );

        assert_eq!(edges.len(), 2, "{edges:?}");
        assert_eq!(edges[0].to, id(43));
        assert_eq!(edges[1].to, id(43));
        assert_eq!(edges[0].from, id(41));
        assert_eq!(edges[1].from, id(42));
    }

    #[test]
    fn a_declared_id_resolves_after_the_two_path_tables() {
        // The path tables miss, so the declared-id table answers: `adr-7` is
        // the id of document 52, and the edge is (52, 51).
        let up = values(&["adr-7"]);
        let docs = [declared(id(51), "decisions/new.md", &up, &[])];
        let (edges, dangling) = resolved(&docs, &BTreeMap::new(), &identity(&[(id(52), "adr-7")]));

        assert!(dangling.is_empty(), "{dangling:?}");
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!((edges[0].from, edges[0].to), (id(52), id(51)));
    }
}
