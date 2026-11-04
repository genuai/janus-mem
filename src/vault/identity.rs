//! Identity derivation: the §6.4 ladder over a scan's document set.
//!
//! Resolution (3b) gives every document its metadata; this pass decides the
//! [`DocId`] and records which rung of the ladder produced it. It runs over the
//! *whole* set, because "is this key unique?" is a property of the corpus, not
//! of a file — which is why it cannot be folded into [`resolve`].
//!
//! [`resolve`]: crate::vault::resolve::resolve
//!
//! # The ladder as implemented
//!
//! ```text
//! 1  declared id                    strongest claim; opaque, used verbatim
//! 3  title + date                   namespaced by corpus and scope
//! 4  title + doc_version            only when no date is available
//! 5  title                          only if unique
//! 6  rel_path                       unconditional fallback
//! ```
//!
//! **Rung 2 (`content_hash`) is a move key, not a stateless rung.** §6.4 lists it
//! second because it *proves* that a new path holds the bytes of a document that
//! vanished from a previous scan — that is, it matches against a previous
//! document set. In a stateless scan there is no previous set, so the rung has
//! no candidates and is skipped. Treating "unique within this scan" as its
//! candidate test would be worse than useless: a content hash is unique for
//! every document that is not a byte-identical copy, so it would win for
//! essentially every document and make rungs 3–5 unreachable — they could only
//! ever fire for exact copies, which by definition share every other key too.
//! See decision S-1 in `docs/m1-breakdown.md` §11.
//!
//! # Why multiplicity is computed over the whole set
//!
//! A key's count includes every document that carries it, whether or not that
//! document resolved at a stronger rung. Counting only "documents that reached
//! this rung" would make a document's id depend on iteration order, and the
//! ladder must be a function of the corpus alone: two scans of the same tree
//! produce the same ids in any order.
//!
//! # Key encoding
//!
//! A match key is the rung's name, then the fields separated by NUL:
//!
//! ```text
//! declared_id\0<id>
//! title_date\0<corpus_id>\0<scope>\0<title>\0<YYYY-MM-DD>
//! title_version\0<corpus_id>\0<scope>\0<title>\0<version>
//! title\0<corpus_id>\0<scope>\0<title>
//! rel_path\0<rel_path>
//! ```
//!
//! NUL is a safe separator: YAML forbids it in a stream, so it cannot appear in
//! frontmatter, and no filesystem allows it in a name. The leading rung name is
//! what stops the same text from colliding across rungs — `title_date` and
//! `title_version` can both end in `2026-01-01`, and only the tag keeps them
//! distinct.

//! # M5: the previous generation is the identity cache (§3.1)
//!
//! ```text
//! match      declared id, content hash, title+date, title+version, title
//! derivation declared id, title+date, title+version, title, rel_path
//! steps      1 carry-forward on a live path, 2 match a move, 3 derive,
//!            4 record, 5 tombstone
//! ```
//!
//! `content_hash` is a **match key only**: as a derivation it would win for
//! every document that is not an exact copy, so rungs 3–5 would never fire. Step
//! 3 also rejects an id that step 1 or 2 claimed (M5-21), or a build that moves
//! `A` to `B` and creates a file at `A` would give two documents one id.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDate;

use crate::config::Config;
use crate::document::{DateSource, DocNote, Document, IdentityTier};
use crate::id::DocId;
use crate::index::{IdentityRecord, MatchKey, PathEntry, Snapshot, StringId, TombstoneMeta};

/// Separates the fields of a match key. See the module docs for why NUL.
const FIELD_SEPARATOR: u8 = 0;

/// The rungs step 3 tries, in order, before the `rel_path` fallback.
///
/// Rung 2 is absent on purpose: see the module docs. `rel_path` is absent
/// because it is not counted — it is unique by construction within one walk, so
/// it is the guaranteed fallback rather than a candidate.
const DERIVATION_LADDER: [IdentityTier; 4] = [
    IdentityTier::DeclaredId,
    IdentityTier::TitleDate,
    IdentityTier::TitleVersion,
    IdentityTier::Title,
];

/// The rungs step 2 tries, in order, against the basis (§3.1).
///
/// Rung 6 is absent: a move is matched by the keys the document carries, never
/// by the path it arrived at.
const MATCH_LADDER: [IdentityTier; 5] = [
    IdentityTier::DeclaredId,
    IdentityTier::ContentHash,
    IdentityTier::TitleDate,
    IdentityTier::TitleVersion,
    IdentityTier::Title,
];

/// What one resolution decided, for the build and for `janus ids` (§3.1).
#[derive(Debug, Clone, Default)]
pub struct Resolution {
    /// Every path a `DocId` has occupied, including the paths of retired ids.
    pub path_history: BTreeMap<DocId, Vec<PathEntry>>,
    /// One record per id ever seen, live or retired.
    pub identity: BTreeMap<DocId, IdentityRecord>,
    /// The basis ids this scan did not claim, sorted by id.
    pub tombstones: Vec<TombstoneMeta>,
    /// The ids this build claimed from the basis, by step 1 or step 2.
    pub inherited: BTreeSet<DocId>,
}

/// The id one document got, and where it came from.
#[derive(Debug, Clone, Copy)]
struct Assigned {
    id: DocId,
    tier: IdentityTier,
    inherited: bool,
}

/// Assigns every document its [`DocId`] and returns the identity tables (§3.1).
///
/// `basis` is the previous generation, or `None` when there is none: a first
/// build, a deleted `.janus/`, a corrupt `index.bin`, or another corpus. A
/// missing basis is not an error — step 3 alone is a complete answer.
///
/// `today` stamps [`PathEntry::first_seen`] and [`PathEntry::last_seen`]; it is
/// an argument so nothing below `main.rs` reads the wall clock (§7.2).
pub fn resolve(
    documents: &mut [Document],
    basis: Option<&Snapshot>,
    config: &Config,
    today: NaiveDate,
) -> Resolution {
    let (basis, mut identity) = usable_basis(basis, config);
    let chosen = assign(documents, basis, &identity, config);
    let mut inherited = BTreeSet::new();
    let mut path_history: BTreeMap<DocId, Vec<PathEntry>> = basis
        .map(|snapshot| snapshot.path_history.clone())
        .unwrap_or_default();

    // Step 4: record. The keys are refreshed from the current metadata, so a
    // later move matches the title the document has now, not the one it had.
    for &index in &path_order(documents) {
        let Some(assigned) = chosen[index] else {
            // Unreachable: step 3 assigns every document. Fail open on the
            // provisional path id rather than invent one.
            continue;
        };
        let keys = record_keys(&documents[index]);
        let rel_path = documents[index].rel_path.clone();
        let entries = path_history.entry(assigned.id).or_default();
        // Claiming a tombstoned id resurrects the identity, so no entry of a
        // live id is a tombstone (R-7, case 1.15).
        for entry in entries.iter_mut() {
            entry.tombstone = false;
        }
        match entries.last_mut() {
            Some(last) if last.rel_path == rel_path => last.last_seen = today,
            _ => entries.push(PathEntry {
                rel_path,
                first_seen: today,
                last_seen: today,
                tombstone: false,
            }),
        }
        if assigned.inherited {
            inherited.insert(assigned.id);
        }
        identity.insert(
            assigned.id,
            IdentityRecord {
                tier: assigned.tier,
                keys,
            },
        );
    }

    // Step 5: tombstone. The vector is rebuilt from the unclaimed set on every
    // build, so a claimed id can never be reported as retired (case 1.15).
    let claimed: BTreeSet<DocId> = chosen.iter().flatten().map(|row| row.id).collect();
    let mut tombstones = Vec::new();
    for (id, entries) in path_history.iter_mut() {
        if claimed.contains(id) {
            continue;
        }
        // Only a real transition stamps the date. A path that stays retired
        // keeps the day the document was last seen in it (§2.1).
        if let Some(last) = entries.last_mut()
            && !last.tombstone
        {
            last.tombstone = true;
            last.last_seen = today;
        }
        if let Some(meta) = retire(*id, basis) {
            tombstones.push(meta);
        }
    }
    tombstones.sort_by_key(|meta| meta.id);

    Resolution {
        path_history,
        identity,
        tombstones,
        inherited,
    }
}

/// Assigns every document its [`DocId`] with no basis, and keeps no table.
///
/// This is §3.1 steps 1–3 with nothing to match against: the branch a reader
/// takes when no generation validates, and the answer for a document that never
/// moved. A document's id is then a pure function of its own metadata.
pub fn derive_identity(documents: &mut [Document], config: &Config) {
    assign(documents, None, &BTreeMap::new(), config);
}

/// The basis when it describes this corpus, and the records it holds. Every
/// title key is namespaced by the corpus id, so another corpus re-keys.
fn usable_basis<'a>(
    basis: Option<&'a Snapshot>,
    config: &Config,
) -> (Option<&'a Snapshot>, BTreeMap<DocId, IdentityRecord>) {
    let basis = basis.filter(|snapshot| snapshot.corpus_id == config.corpus_id);
    let identity = basis
        .map(|snapshot| snapshot.identity.clone())
        .unwrap_or_default();
    (basis, identity)
}

/// Steps 1–3: carry forward, match a move, then derive (§3.1). One entry per
/// document, in input order, and the id, tier and duplicate note on each
/// document. Calling it twice is safe: it clears the notes it owns first.
fn assign(
    documents: &mut [Document],
    basis: Option<&Snapshot>,
    identity: &BTreeMap<DocId, IdentityRecord>,
    config: &Config,
) -> Vec<Option<Assigned>> {
    for document in documents.iter_mut() {
        document
            .notes
            .retain(|note| !matches!(note, DocNote::DuplicateIdentity { .. }));
    }

    let counts = count_keys(documents, config);
    let order = path_order(documents);
    let mut chosen: Vec<Option<Assigned>> = vec![None; documents.len()];
    let mut claimed: BTreeSet<DocId> = BTreeSet::new();

    // Step 1: carry forward on the live path. A path a document left is
    // historical, never live, so step 2 decides that case.
    if let Some(basis) = basis {
        let live = live_paths(&basis.path_history);
        for &index in &order {
            if let Some(id) = live.get(&documents[index].rel_path)
                && let Some(record) = identity.get(id)
            {
                chosen[index] = Some(Assigned {
                    id: *id,
                    tier: record.tier,
                    inherited: true,
                });
                claimed.insert(*id);
            }
        }
    }

    // Step 2: match a move against an unclaimed basis id.
    if let Some(keys) = basis_keys(basis) {
        for &index in &order {
            if chosen[index].is_some() {
                continue;
            }
            let scope = documents[index].scope.clone();
            for tier in MATCH_LADDER {
                let Some(key) = typed_key(tier, &documents[index]) else {
                    continue;
                };
                let Some(ids) = keys.get(&(scope.clone(), key)) else {
                    continue;
                };
                let open: Vec<DocId> = ids
                    .iter()
                    .copied()
                    .filter(|id| !claimed.contains(id))
                    .collect();
                // The tier is the rung that matched, not the rung that first
                // produced the id: `tier` answers "why does this build think
                // so?", and case 1.3 needs `content_hash` for a byte-identical
                // move even though the id came from title+date.
                if let [id] = open.as_slice() {
                    chosen[index] = Some(Assigned {
                        id: *id,
                        tier,
                        inherited: true,
                    });
                    claimed.insert(*id);
                    break;
                }
            }
        }
    }

    // Step 3: derive. The claimed-id guard lives inside `derive` (M5-21).
    for &index in &order {
        if chosen[index].is_some() {
            continue;
        }
        let (id, tier, ambiguous) = derive(&documents[index], config, &counts, &claimed);
        chosen[index] = Some(Assigned {
            id,
            tier,
            inherited: false,
        });
        claimed.insert(id);
        if let Some(key) = ambiguous {
            documents[index]
                .notes
                .push(DocNote::DuplicateIdentity { key });
        }
    }

    for &index in &order {
        if let Some(assigned) = chosen[index] {
            documents[index].id = assigned.id;
            documents[index].id_source = assigned.tier;
        }
    }

    chosen
}

/// The live path of each basis id: its last entry, when not a tombstone.
fn live_paths(path_history: &BTreeMap<DocId, Vec<PathEntry>>) -> BTreeMap<String, DocId> {
    path_history
        .iter()
        .filter_map(|(id, entries)| {
            entries
                .last()
                .filter(|entry| !entry.tombstone)
                .map(|entry| (entry.rel_path.clone(), *id))
        })
        .collect()
}

/// Every basis id that answers to a key, grouped by scope and key (§3.1 step 2).
/// The scope is part of the grouping, not of the key text, because a key carries
/// no namespace in memory.
fn basis_keys(basis: Option<&Snapshot>) -> Option<BTreeMap<(String, MatchKey), Vec<DocId>>> {
    let basis = basis?;
    let scopes: BTreeMap<DocId, &str> = basis
        .tombstones
        .iter()
        .map(|meta| (meta.id, meta.scope.as_str()))
        .chain(
            basis
                .docs
                .iter()
                .filter_map(|doc| basis.strings.get(doc.scope).map(|scope| (doc.id, scope))),
        )
        .collect();
    let mut keys: BTreeMap<(String, MatchKey), Vec<DocId>> = BTreeMap::new();
    for (id, record) in &basis.identity {
        let Some(scope) = scopes.get(id) else {
            continue;
        };
        for key in &record.keys {
            keys.entry(((*scope).to_owned(), key.clone()))
                .or_default()
                .push(*id);
        }
    }
    for ids in keys.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    Some(keys)
}

/// The last known facts about a basis id this scan did not claim. An id that was
/// already retired keeps its frozen row.
fn retire(id: DocId, basis: Option<&Snapshot>) -> Option<TombstoneMeta> {
    let basis = basis?;
    if let Some(meta) = basis.tombstones.iter().find(|meta| meta.id == id) {
        return Some(meta.clone());
    }
    let doc = basis.docs.iter().find(|doc| doc.id == id)?;
    let text = |id: StringId| basis.strings.get(id).unwrap_or_default().to_owned();
    Some(TombstoneMeta {
        id,
        title: text(doc.title),
        date: doc.date,
        valid_from: doc.effective_from.or(doc.date),
        scope: text(doc.scope),
        shared_with: doc.shared_with.iter().map(|tag| text(*tag)).collect(),
    })
}

/// The documents in `rel_path` order, so every step is order-independent.
fn path_order(documents: &[Document]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..documents.len()).collect();
    order.sort_by(|left, right| {
        documents[*left]
            .rel_path
            .cmp(&documents[*right].rel_path)
            .then_with(|| documents[*left].id.cmp(&documents[*right].id))
    });
    order
}
/// Counts how many documents carry each derivation key, for every rung.
///
/// One pass over the set, one map: the alternative — comparing each document to
/// every other — is O(n²) on a 20k-document vault (§13). Rung 2 is absent: it
/// is a match key, not a derivation key.
fn count_keys(documents: &[Document], config: &Config) -> HashMap<Vec<u8>, usize> {
    let mut counts: HashMap<Vec<u8>, usize> =
        HashMap::with_capacity(documents.len() * DERIVATION_LADDER.len());

    for document in documents {
        for tier in DERIVATION_LADDER {
            if let Some(key) = match_key(tier, document, config) {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    counts
}

/// Picks the strongest derivation key that is unique and unclaimed (§3.1 step 3).
///
/// Returns the id, the tier, and the rendered key of the first ambiguity (if
/// any) so the caller can explain the fall-through.
fn derive(
    document: &Document,
    config: &Config,
    counts: &HashMap<Vec<u8>, usize>,
    claimed: &BTreeSet<DocId>,
) -> (DocId, IdentityTier, Option<String>) {
    let mut ambiguous = None;

    for tier in DERIVATION_LADDER {
        let Some(key) = match_key(tier, document, config) else {
            continue;
        };

        // Exactly one carrier means unique. A missing count is not proof of
        // uniqueness, so it falls through like any other ambiguity.
        if counts.get(&key).copied().unwrap_or(0) != 1 {
            ambiguous.get_or_insert_with(|| describe_key(tier, document, config));
            continue;
        }

        let id = DocId::from_match_key(&key);
        // The claimed-id guard (M5-21). The key is unique in this scan, so it is
        // not ambiguous: its id simply belongs to a document that kept its path,
        // and two documents must never share one id.
        if !claimed.contains(&id) {
            return (id, tier, ambiguous);
        }
    }

    // `rel_path` is unique by construction within one scan, so no basis id can
    // hold the id it derives: holding it needs a basis id derived from this same
    // path, and step 1 adopts exactly that id.
    let key = rel_path_key(document);
    (
        DocId::from_match_key(&key),
        IdentityTier::RelPath,
        ambiguous,
    )
}

/// One key per reachable match rung, in ladder order (§2.1).
fn record_keys(document: &Document) -> Vec<MatchKey> {
    MATCH_LADDER
        .iter()
        .filter_map(|tier| typed_key(*tier, document))
        .collect()
}

/// Encodes one rung's match key, or `None` when the rung does not apply.
fn match_key(tier: IdentityTier, document: &Document, config: &Config) -> Option<Vec<u8>> {
    typed_key(tier, document).map(|key| key_bytes(&key, &config.corpus_id, &document.scope))
}

/// The typed form of one rung's key, or `None` when the rung does not apply.
///
/// Availability lives here so one place reads the ladder's conditions: no
/// declared id, no usable date, no version. The bytes and the stored record are
/// two views of this one answer.
fn typed_key(tier: IdentityTier, document: &Document) -> Option<MatchKey> {
    match tier {
        IdentityTier::DeclaredId => Some(MatchKey::DeclaredId(declared_id(document)?.to_owned())),
        IdentityTier::ContentHash => Some(MatchKey::ContentHash(document.content_hash)),
        IdentityTier::TitleDate => Some(MatchKey::TitleDate {
            title: document.title.clone(),
            date: usable_date(document)?,
        }),
        IdentityTier::TitleVersion => {
            // §6.4: rung 4 substitutes for rung 3 when no date is available; it
            // is not a further fallback for a document whose title+date was
            // ambiguous.
            if usable_date(document).is_some() {
                return None;
            }
            Some(MatchKey::TitleVersion {
                title: document.title.clone(),
                version: doc_version(document)?.to_owned(),
            })
        }
        IdentityTier::Title => Some(MatchKey::Title(document.title.clone())),
        // `rel_path` is the unconditional fallback and is never counted.
        IdentityTier::RelPath => None,
    }
}

/// Encodes a typed key: the rung name, then the fields separated by NUL. The
/// title rungs carry their `corpus/scope` namespace (§6.4, §6.12 rule 5).
fn key_bytes(key: &MatchKey, corpus_id: &str, scope: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(96);
    match key {
        MatchKey::DeclaredId(id) => {
            push_rung(&mut bytes, IdentityTier::DeclaredId);
            bytes.extend_from_slice(id.as_bytes());
        }
        MatchKey::ContentHash(hash) => {
            push_rung(&mut bytes, IdentityTier::ContentHash);
            bytes.extend_from_slice(hash.to_hex().as_bytes());
        }
        MatchKey::TitleDate { title, date } => {
            push_namespace(&mut bytes, IdentityTier::TitleDate, corpus_id, scope);
            bytes.extend_from_slice(title.as_bytes());
            bytes.push(FIELD_SEPARATOR);
            bytes.extend_from_slice(date.to_string().as_bytes());
        }
        MatchKey::TitleVersion { title, version } => {
            push_namespace(&mut bytes, IdentityTier::TitleVersion, corpus_id, scope);
            bytes.extend_from_slice(title.as_bytes());
            bytes.push(FIELD_SEPARATOR);
            bytes.extend_from_slice(version.as_bytes());
        }
        MatchKey::Title(title) => {
            push_namespace(&mut bytes, IdentityTier::Title, corpus_id, scope);
            bytes.extend_from_slice(title.as_bytes());
        }
    }
    bytes
}

/// Writes the rung name and its separator.
fn push_rung(key: &mut Vec<u8>, tier: IdentityTier) {
    key.extend_from_slice(tier.as_str().as_bytes());
    key.push(FIELD_SEPARATOR);
}

/// The `rel_path` fallback key, which needs no uniqueness test.
fn rel_path_key(document: &Document) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + document.rel_path.len());
    push_rung(&mut key, IdentityTier::RelPath);
    key.extend_from_slice(document.rel_path.as_bytes());
    key
}

/// Writes `rung \0 corpus_id \0 scope \0` into a key.
fn push_namespace(key: &mut Vec<u8>, tier: IdentityTier, corpus_id: &str, scope: &str) {
    push_rung(key, tier);
    key.extend_from_slice(corpus_id.as_bytes());
    key.push(FIELD_SEPARATOR);
    key.extend_from_slice(scope.as_bytes());
    key.push(FIELD_SEPARATOR);
}

/// A declared id, unless it is blank.
///
/// A blank `id: ""` is a formatting accident, not a claim. A non-blank id is
/// opaque and used verbatim — never trimmed, lowercased or rewritten, because
/// other systems' ids must survive round-tripping (§6.4).
fn declared_id(document: &Document) -> Option<&str> {
    document
        .frontmatter
        .id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
}

/// A declared `doc_version`, unless it is blank.
fn doc_version(document: &Document) -> Option<&str> {
    document
        .frontmatter
        .doc_version
        .as_deref()
        .filter(|version| !version.trim().is_empty())
}

/// The date identity may use: frontmatter or a day-precision file name only.
///
/// §6.4 excludes values that copies churn. A month, year or folder date is also
/// excluded here, so this milestone moves no existing `DocId` (§3.4).
fn usable_date(document: &Document) -> Option<NaiveDate> {
    match document.date_source {
        DateSource::Frontmatter | DateSource::Filename => document.date,
        DateSource::FilenameMonth
        | DateSource::FilenameYear
        | DateSource::Folder
        | DateSource::FolderMonth
        | DateSource::FolderYear
        | DateSource::None => None,
    }
}

/// Renders an ambiguous key for a human, never as the raw NUL-joined bytes.
fn describe_key(tier: IdentityTier, document: &Document, config: &Config) -> String {
    let namespace = format!("{}/{}", config.corpus_id, document.scope);

    match tier {
        IdentityTier::DeclaredId => {
            format!("id: {}", declared_id(document).unwrap_or_default())
        }
        IdentityTier::ContentHash => {
            let hex = document.content_hash.to_hex();
            format!("content_hash: {}", hex.get(..12).unwrap_or(&hex))
        }
        IdentityTier::TitleDate => format!(
            "title_date: {namespace} \"{}\" {}",
            document.title,
            usable_date(document).map_or_else(String::new, |date| date.to_string()),
        ),
        IdentityTier::TitleVersion => format!(
            "title_version: {namespace} \"{}\" {}",
            document.title,
            doc_version(document).unwrap_or_default(),
        ),
        IdentityTier::Title => format!("title: {namespace} \"{}\"", document.title),
        IdentityTier::RelPath => format!("rel_path: {}", document.rel_path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{DocMeta, LexicalIndex, StringPool};
    use crate::tokenize::Tokenizer;
    use crate::vault::DiscoveredFile;
    use crate::vault::resolve::resolve as resolve_document;
    use std::fs;
    use std::path::Path;
    use std::time::SystemTime;

    fn config() -> Config {
        Config::from_toml("", Path::new("<test>")).expect("test config must parse")
    }

    /// A pinned build date: no identity table may depend on the clock.
    fn today() -> NaiveDate {
        date(2026, 1, 1)
    }

    /// A calendar date, for the cases that need two builds on different days.
    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
    }

    /// Writes each entry to a temp tree and resolves it, in order.
    fn resolved(entries: &[(&str, &str)], config: &Config) -> Vec<Document> {
        let dir = tempfile::tempdir().expect("tempdir");

        entries
            .iter()
            .map(|(rel, text)| {
                let path = dir.path().join(rel);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).expect("test directory");
                }
                fs::write(&path, text).expect("test file");
                let file = DiscoveredFile {
                    path,
                    rel_path: (*rel).to_owned(),
                    size: text.len() as u64,
                    mtime: SystemTime::UNIX_EPOCH,
                };
                resolve_document(&file, config).expect("resolve must succeed")
            })
            .collect()
    }

    /// Resolves and then derives identity with the default config.
    fn derived(entries: &[(&str, &str)]) -> Vec<Document> {
        let config = config();
        let mut documents = resolved(entries, &config);
        derive_identity(&mut documents, &config);
        documents
    }

    fn notes(document: &Document) -> Vec<&'static str> {
        document.notes.iter().map(DocNote::code).collect()
    }

    /// A basis snapshot: the corpus id, one metadata row per document, and the
    /// three identity tables. Nothing else on the snapshot is read here.
    fn basis_of(documents: &[Document], resolution: Resolution, config: &Config) -> Snapshot {
        let mut pool = StringPool::new();
        let mut rows = Vec::with_capacity(documents.len());
        for document in documents {
            rows.push(DocMeta {
                id: document.id,
                rel_path: pool.intern(&document.rel_path),
                title: pool.intern(&document.title),
                type_name: pool.intern(&document.type_name),
                status: document.status,
                scope: pool.intern(&document.scope),
                shared_with: document
                    .shared_with
                    .iter()
                    .map(|tag| pool.intern(tag))
                    .collect(),
                tags: Vec::new(),
                date: document.date,
                date_source: document.date_source,
                effective_from: document.effective_from,
                timeless: false,
                content_hash: document.content_hash,
                body_hash: document.content_hash,
                lex_key: [0; 32],
                chunk_range: (0, 0),
            });
        }
        let index = LexicalIndex::from_parts(pool, Vec::new(), Vec::new(), Vec::new(), 0.0);
        let mut snapshot = Snapshot::from_index(
            &index,
            config.corpus_id.clone(),
            Tokenizer::new().fingerprint(),
            rows,
            Vec::new(),
        );
        snapshot.path_history = resolution.path_history;
        snapshot.identity = resolution.identity;
        snapshot.tombstones = resolution.tombstones;
        snapshot
    }

    /// One build: resolve a fresh scan of `entries` against `basis`.
    fn build(entries: &[(&str, &str)], basis: Option<&Snapshot>) -> (Vec<Document>, Resolution) {
        build_on(today(), entries, basis)
    }

    /// The same build on a given day, so a test can see a date move.
    fn build_on(
        today: NaiveDate,
        entries: &[(&str, &str)],
        basis: Option<&Snapshot>,
    ) -> (Vec<Document>, Resolution) {
        let config = config();
        let mut documents = resolved(entries, &config);
        let resolution = resolve(&mut documents, basis, &config, today);
        (documents, resolution)
    }

    /// Builds once, then hands back the basis a second build would read.
    fn first_build(entries: &[(&str, &str)]) -> Snapshot {
        let (documents, resolution) = build(entries, None);
        basis_of(&documents, resolution, &config())
    }

    #[test]
    fn a_retired_path_keeps_the_day_the_document_vanished() {
        // `last_seen` is the build that last saw the path, and a retired path is
        // never seen again. A build that only restamps the tombstone would make
        // every retired document pass every `moves --since` (P6).
        let text = "---\ntitle: Flow\ndate: 2026-01-01\n---\nbody\n";
        let vanished = date(2026, 1, 2);
        let (documents, tables) = build_on(date(2026, 1, 1), &[("notes/flow.md", text)], None);
        let id = documents[0].id;
        let basis = basis_of(&documents, tables, &config());
        let (empty, gone) = build_on(vanished, &[], Some(&basis));
        assert!(gone.path_history[&id].last().expect("an entry").tombstone);
        let basis = basis_of(&empty, gone, &config());

        let (_, again) = build_on(date(2026, 1, 9), &[], Some(&basis));

        let last = again.path_history[&id].last().expect("an entry");
        assert!(last.tombstone, "the document is still gone");
        assert_eq!(
            last.last_seen, vanished,
            "the day it was last seen, not the latest build"
        );
    }

    #[test]
    fn a_scope_change_rekeys_even_a_declared_id() {
        // M5-27: scope qualifies every match key, rungs 1 and 2 included, so a
        // scope change is a re-key and not a move. The declared id derives the
        // same value, and the id must not be reported as inherited.
        let declared = "---\nid: 01JABC\ntitle: Plan\n---\nbody\n";
        let basis = first_build(&[("a/plan.md", declared)]);
        let scoped = "---\nid: 01JABC\ntitle: Plan\nscope: org:beta\n---\nbody\n";
        let (documents, tables) = build(&[("b/plan.md", scoped)], Some(&basis));

        assert_eq!(
            documents[0].id,
            DocId::from_match_key(b"declared_id\x0001JABC")
        );
        assert_eq!(documents[0].id_source, IdentityTier::DeclaredId);
        assert!(
            !tables.inherited.contains(&documents[0].id),
            "a scope change is churn, not a move"
        );
    }

    // ───────────────────────────── key encoding ─────────────────────────────

    #[test]
    fn match_keys_are_pinned() {
        let config = config();
        let documents = resolved(
            &[(
                "decisions/plan.md",
                "---\nid: 01JABC\ntitle: Q4 plan\nscope: org:acme\ndate: 2026-01-01\ndoc_version: \"2.0\"\n---\nbody\n",
            )],
            &config,
        );
        let document = &documents[0];

        assert_eq!(
            match_key(IdentityTier::DeclaredId, document, &config)
                .expect("declared id applies")
                .as_slice(),
            b"declared_id\x0001JABC".as_slice()
        );
        assert_eq!(
            match_key(IdentityTier::TitleDate, document, &config)
                .expect("title+date applies")
                .as_slice(),
            b"title_date\x00main\x00org:acme\x00Q4 plan\x002026-01-01".as_slice()
        );
        assert_eq!(
            match_key(IdentityTier::Title, document, &config)
                .expect("title applies")
                .as_slice(),
            b"title\x00main\x00org:acme\x00Q4 plan".as_slice()
        );
        assert_eq!(
            rel_path_key(document).as_slice(),
            b"rel_path\x00decisions/plan.md".as_slice()
        );
    }

    #[test]
    fn a_date_disables_the_title_version_rung() {
        let config = config();
        let documents = resolved(
            &[(
                "a.md",
                "---\ntitle: Plan\ndate: 2026-01-01\ndoc_version: \"2.0\"\n---\n",
            )],
            &config,
        );

        assert_eq!(
            match_key(IdentityTier::TitleVersion, &documents[0], &config),
            None
        );
    }

    #[test]
    fn the_content_hash_is_a_match_key_and_not_a_derivation_key() {
        let config = config();
        let documents = resolved(&[("a.md", "x")], &config);
        let keys = record_keys(&documents[0]);

        assert!(
            keys.iter()
                .any(|key| matches!(key, MatchKey::ContentHash(_))),
            "step 2 must be able to match a byte-identical move: {keys:?}"
        );
        assert!(
            !DERIVATION_LADDER.contains(&IdentityTier::ContentHash),
            "step 3 must never derive an id from the bytes (S-1, R2-1)"
        );
        // The bytes of rung 2 are still encodable, because one encoder serves
        // both views of a key: the rung name, NUL, then the hex digest.
        let expected = format!("content_hash\0{}", documents[0].content_hash.to_hex());
        assert_eq!(
            match_key(IdentityTier::ContentHash, &documents[0], &config)
                .expect("a content hash is always available"),
            expected.as_bytes()
        );
    }

    #[test]
    fn a_blank_declared_id_is_not_a_claim() {
        let config = config();
        let documents = resolved(&[("a.md", "---\nid: \"   \"\n---\n")], &config);

        assert_eq!(
            match_key(IdentityTier::DeclaredId, &documents[0], &config),
            None
        );
    }

    #[test]
    fn a_declared_id_is_opaque_and_untrimmed() {
        let documents = derived(&[("a.md", "---\nid: \" 01JABC \"\ntitle: One\n---\n")]);

        assert_eq!(
            documents[0].id,
            DocId::from_match_key(b"declared_id\x00 01JABC ")
        );
    }

    // ──────────────────────────────── ladder ────────────────────────────────

    #[test]
    fn a_unique_declared_id_wins_the_first_rung() {
        let documents = derived(&[(
            "a.md",
            "---\nid: 01JABC\ntitle: One\ndate: 2026-01-01\n---\n",
        )]);

        assert_eq!(documents[0].id_source, IdentityTier::DeclaredId);
        assert_eq!(
            documents[0].id,
            DocId::from_match_key(b"declared_id\x0001JABC")
        );
        assert!(documents[0].notes.is_empty(), "{:?}", documents[0].notes);
    }

    #[test]
    fn a_duplicate_declared_id_falls_through_and_is_reported() {
        let documents = derived(&[
            ("a.md", "---\nid: SAME\ntitle: One\n---\n"),
            ("b.md", "---\nid: SAME\ntitle: Two\n---\n"),
        ]);

        // No date and no version, but the titles are unique.
        assert_eq!(documents[0].id_source, IdentityTier::Title);
        assert_eq!(documents[1].id_source, IdentityTier::Title);
        assert_ne!(documents[0].id, documents[1].id);
        assert_eq!(notes(&documents[0]), ["duplicate_identity"]);
        assert!(documents[0].notes[0].message().contains("id: SAME"));
    }

    #[test]
    fn an_undated_document_uses_title_and_date() {
        let documents = derived(&[("a.md", "---\ntitle: Q4 plan\ndate: 2026-01-01\n---\n")]);

        assert_eq!(documents[0].id_source, IdentityTier::TitleDate);
        assert!(documents[0].notes.is_empty(), "{:?}", documents[0].notes);
    }

    #[test]
    fn a_filename_date_is_used_for_the_title_date_rung() {
        let documents = derived(&[("2024-11-02-postgres.md", "x")]);

        assert_eq!(documents[0].id_source, IdentityTier::TitleDate);
    }

    #[test]
    fn an_undated_document_never_contributes_a_date_to_identity() {
        // No date and no version: rung 3 is unavailable, so the unique title wins.
        let documents = derived(&[("notes.md", "x")]);

        assert_eq!(documents[0].date_source, DateSource::None);
        assert_eq!(documents[0].id_source, IdentityTier::Title);
    }

    #[test]
    fn an_undated_document_uses_title_and_version() {
        let documents = derived(&[("a.md", "---\ntitle: Plan\ndoc_version: \"2.0\"\n---\n")]);

        assert_eq!(documents[0].id_source, IdentityTier::TitleVersion);
    }

    #[test]
    fn title_version_is_not_a_fallback_when_a_date_exists() {
        // Same title+date, different versions: §6.4 makes rung 4 a substitute for
        // rung 3 when no date exists, not a further fallback.
        let documents = derived(&[
            (
                "a.md",
                "---\ntitle: Plan\ndate: 2026-01-01\ndoc_version: \"1.0\"\n---\n",
            ),
            (
                "b.md",
                "---\ntitle: Plan\ndate: 2026-01-01\ndoc_version: \"2.0\"\n---\n",
            ),
        ]);

        for document in &documents {
            assert_eq!(document.id_source, IdentityTier::RelPath);
        }
    }

    #[test]
    fn the_same_title_and_date_in_one_scope_falls_to_rel_path() {
        let documents = derived(&[
            ("a/plan.md", "---\ntitle: Q4 plan\ndate: 2026-01-01\n---\n"),
            ("b/plan.md", "---\ntitle: Q4 plan\ndate: 2026-01-01\n---\n"),
        ]);

        for document in &documents {
            assert_eq!(document.id_source, IdentityTier::RelPath);
            assert_eq!(notes(document), ["duplicate_identity"]);
            assert!(
                document.notes[0].message().contains("title_date"),
                "{}",
                document.notes[0].message()
            );
        }
        assert_eq!(
            documents[0].id,
            DocId::from_match_key(b"rel_path\x00a/plan.md")
        );
        assert_ne!(documents[0].id, documents[1].id);
    }

    #[test]
    fn the_same_title_and_date_in_different_scopes_stays_distinct() {
        let documents = derived(&[
            (
                "a/plan.md",
                "---\ntitle: Q4 plan\ndate: 2026-01-01\nscope: org:acme\n---\n",
            ),
            (
                "b/plan.md",
                "---\ntitle: Q4 plan\ndate: 2026-01-01\nscope: org:beta\n---\n",
            ),
        ]);

        for document in &documents {
            assert_eq!(document.id_source, IdentityTier::TitleDate);
        }
        assert_ne!(documents[0].id, documents[1].id);
    }

    #[test]
    fn content_is_not_a_stateless_match_key() {
        // Different bytes, same title+date: the documents are ambiguous, and
        // identity must not silently become content-derived — that would re-key
        // a document on every edit (S-1).
        let documents = derived(&[
            (
                "a/plan.md",
                "---\ntitle: Q4 plan\ndate: 2026-01-01\n---\nfirst\n",
            ),
            (
                "b/plan.md",
                "---\ntitle: Q4 plan\ndate: 2026-01-01\n---\nsecond\n",
            ),
        ]);

        for document in &documents {
            assert_eq!(document.id_source, IdentityTier::RelPath);
        }
    }

    #[test]
    fn identical_copies_with_the_same_metadata_fall_to_rel_path() {
        let text = "---\ntitle: Copy\ndate: 2026-01-01\n---\nsame\n";
        let documents = derived(&[("a.md", text), ("b.md", text)]);

        for document in &documents {
            assert_eq!(document.id_source, IdentityTier::RelPath);
        }
    }

    #[test]
    fn a_duplicate_note_follows_the_resolution_notes() {
        let documents = derived(&[
            (
                "a.md",
                "---\ntype: runbook\ntitle: Plan\ndate: 2026-01-01\n---\n",
            ),
            ("b.md", "---\ntitle: Plan\ndate: 2026-01-01\n---\n"),
        ]);

        assert_eq!(notes(&documents[0]), ["unknown_type", "duplicate_identity"]);
    }

    // ─────────────────────────────── stability ──────────────────────────────

    #[test]
    fn identity_does_not_depend_on_document_order() {
        let entries = [
            ("a.md", "---\ntitle: A\ndate: 2026-01-01\n---\n"),
            ("b.md", "---\ntitle: B\n---\n"),
            ("c.md", "---\ntitle: A\ndate: 2026-01-01\n---\n"),
            ("d.md", "---\nid: D\n---\n"),
        ];
        let config = config();

        let mut forward = resolved(&entries, &config);
        resolve(&mut forward, None, &config, today());

        let mut backward = resolved(&entries, &config);
        backward.reverse();
        resolve(&mut backward, None, &config, today());

        let mut left: Vec<(&str, String)> = forward
            .iter()
            .map(|document| (document.rel_path.as_str(), document.id.to_hex()))
            .collect();
        let mut right: Vec<(&str, String)> = backward
            .iter()
            .map(|document| (document.rel_path.as_str(), document.id.to_hex()))
            .collect();
        left.sort();
        right.sort();

        assert_eq!(left, right);
    }

    #[test]
    fn derivation_overwrites_the_provisional_rel_path_identity() {
        let config = config();
        let mut documents = resolved(&[("a.md", "---\nid: 01JABC\n---\n")], &config);

        assert_eq!(
            documents[0].id_source,
            IdentityTier::RelPath,
            "3b placeholder"
        );
        derive_identity(&mut documents, &config);
        assert_eq!(documents[0].id_source, IdentityTier::DeclaredId);
    }

    #[test]
    fn every_document_gets_a_distinct_id() {
        let documents = derived(&[
            ("a.md", "---\ntitle: A\ndate: 2026-01-01\n---\n"),
            ("b.md", "---\ntitle: B\ndate: 2026-01-01\n---\n"),
            ("c.md", "---\ntitle: C\n---\n"),
            ("d.md", "---\ntitle: A\ndate: 2026-01-01\n---\n"),
        ]);

        let mut ids: Vec<String> = documents
            .iter()
            .map(|document| document.id.to_hex())
            .collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();

        assert_eq!(ids.len(), total);
    }

    #[test]
    fn shared_with_never_changes_identity() {
        let config = config();
        let mut plain = resolved(
            &[("a.md", "---\ntitle: Q4 plan\ndate: 2026-01-01\n---\n")],
            &config,
        );
        let mut shared = resolved(
            &[(
                "a.md",
                "---\ntitle: Q4 plan\ndate: 2026-01-01\nshared_with: [shared:legal]\n---\n",
            )],
            &config,
        );

        resolve(&mut plain, None, &config, today());
        resolve(&mut shared, None, &config, today());

        assert_eq!(plain[0].id, shared[0].id);
        assert_eq!(plain[0].id_source, shared[0].id_source);
    }

    #[test]
    fn changing_scope_rekeys_the_document() {
        let config = config();
        let mut acme = resolved(
            &[(
                "a.md",
                "---\ntitle: Q4 plan\ndate: 2026-01-01\nscope: org:acme\n---\n",
            )],
            &config,
        );
        let mut beta = resolved(
            &[(
                "a.md",
                "---\ntitle: Q4 plan\ndate: 2026-01-01\nscope: org:beta\n---\n",
            )],
            &config,
        );

        derive_identity(&mut acme, &config);
        derive_identity(&mut beta, &config);

        assert_ne!(acme[0].id, beta[0].id);
    }

    #[test]
    fn an_empty_set_is_not_an_error() {
        let config = config();
        let mut documents: Vec<Document> = Vec::new();

        derive_identity(&mut documents, &config);

        assert!(documents.is_empty());
    }

    // ─────────────────────── the five steps of §3.1 ────────────────────────

    #[test]
    fn a_body_edit_keeps_the_id_and_the_path() {
        // Same path, different bytes: step 1 carries the id forward, so the tier
        // that produced it never moves either.
        let basis = first_build(&[(
            "notes/flow.md",
            "---\ntitle: Flow\ndate: 2026-01-01\n---\nfirst body\n",
        )]);
        let (documents, tables) = build(
            &[(
                "notes/flow.md",
                "---\ntitle: Flow\ndate: 2026-01-01\n---\nsecond body\n",
            )],
            Some(&basis),
        );
        let id = documents[0].id;
        let entries = &tables.path_history[&id];

        assert_eq!(
            id,
            DocId::from_match_key(b"title_date\x00main\x00global\x00Flow\x002026-01-01"),
            "the id is the one the first build derived"
        );
        assert_eq!(documents[0].id_source, IdentityTier::TitleDate);
        assert_eq!(entries.len(), 1, "one path, seen twice: {entries:?}");
        assert_eq!(entries[0].first_seen, today());
        assert!(!entries[0].tombstone);
        assert!(tables.inherited.contains(&id), "the id came from the basis");
        assert!(tables.tombstones.is_empty());
    }

    #[test]
    fn an_in_place_title_edit_keeps_the_id() {
        // Step 1 wins over every key: a path that did not move keeps its id even
        // when the title it was derived from is gone. Step 4 then refreshes the
        // stored key to the new title, so a later move still matches by it.
        let basis = first_build(&[(
            "decisions/2025-02-01-cache.md",
            "---\ntitle: Cache revisited\ndate: 2025-02-01\n---\nbody\n",
        )]);
        let (documents, tables) = build(
            &[(
                "decisions/2025-02-01-cache.md",
                "---\ntitle: Cache comes back\ndate: 2025-02-01\n---\nbody\n",
            )],
            Some(&basis),
        );
        let id = documents[0].id;

        assert_eq!(
            id,
            DocId::from_match_key(b"title_date\x00main\x00global\x00Cache revisited\x002025-02-01")
        );
        assert_eq!(documents[0].id_source, IdentityTier::TitleDate);
        assert!(tables.identity[&id].keys.contains(&MatchKey::TitleDate {
            title: "Cache comes back".to_owned(),
            date: NaiveDate::from_ymd_opt(2025, 2, 1).expect("valid date"),
        }));
    }

    #[test]
    fn a_byte_identical_move_matches_by_content_hash() {
        let text = "---\ntitle: Flow\ndate: 2026-01-01\n---\nbody\n";
        let basis = first_build(&[("notes/flow.md", text)]);
        let (documents, tables) = build(&[("notes/flow-copy.md", text)], Some(&basis));
        let id = documents[0].id;
        let entries = &tables.path_history[&id];

        assert_eq!(documents[0].id_source, IdentityTier::ContentHash);
        assert_eq!(entries.len(), 2, "the id remembers both paths: {entries:?}");
        assert_eq!(entries[0].rel_path, "notes/flow.md");
        assert_eq!(entries[1].rel_path, "notes/flow-copy.md");
        assert!(
            !entries[0].tombstone,
            "the document is live, so its old path is not a tombstone"
        );
        assert!(tables.tombstones.is_empty());
    }

    #[test]
    fn a_move_with_a_body_edit_matches_by_title_and_date() {
        // The bytes changed, so rung 2 cannot fire; the metadata did not, so
        // rung 3 does.
        let basis = first_build(&[(
            "decisions/2024-11-02-postgres.md",
            "---\ntitle: Adopt postgres\ndate: 2024-11-02\n---\nbody\n",
        )]);
        let (documents, _) = build(
            &[(
                "decisions/postgres.md",
                "---\ntitle: Adopt postgres\ndate: 2024-11-02\n---\nnew body\n",
            )],
            Some(&basis),
        );

        assert_eq!(documents[0].id_source, IdentityTier::TitleDate);
        assert_eq!(
            documents[0].id,
            DocId::from_match_key(b"title_date\x00main\x00global\x00Adopt postgres\x002024-11-02")
        );
    }

    #[test]
    fn a_move_that_changes_title_and_date_is_a_new_id_and_a_tombstone() {
        let basis = first_build(&[(
            "decisions/2024-11-02-postgres.md",
            "---\ntitle: Adopt postgres\ndate: 2024-11-02\n---\nbody\n",
        )]);
        let old =
            DocId::from_match_key(b"title_date\x00main\x00global\x00Adopt postgres\x002024-11-02");
        let (documents, tables) = build(
            &[(
                "decisions/2025-01-01-redis.md",
                "---\ntitle: Adopt redis\ndate: 2025-01-01\n---\nbody\n",
            )],
            Some(&basis),
        );

        assert_ne!(documents[0].id, old);
        assert!(!tables.inherited.contains(&documents[0].id));
        assert_eq!(tables.tombstones.len(), 1);
        assert_eq!(tables.tombstones[0].id, old);
        assert_eq!(tables.tombstones[0].title, "Adopt postgres");
        assert_eq!(
            tables.tombstones[0].date,
            NaiveDate::from_ymd_opt(2024, 11, 2)
        );
        let retired = &tables.path_history[&old];
        assert!(retired.last().expect("an entry").tombstone);
    }

    #[test]
    fn a_byte_identical_copy_falls_to_rel_path() {
        // The copy carries every key of the original, but the original kept its
        // path, so step 2 has no unclaimed basis id to answer. Step 3 counts two
        // carriers of the title+date key in this scan, so the copy falls through
        // to the path and is reported as a duplicate.
        let text = "---\ntitle: Copy\ndate: 2026-01-01\n---\nsame\n";
        let basis = first_build(&[("a.md", text)]);
        let (documents, tables) = build(&[("a.md", text), ("b.md", text)], Some(&basis));

        assert_eq!(documents[1].id_source, IdentityTier::RelPath);
        assert_eq!(documents[1].id, DocId::from_match_key(b"rel_path\x00b.md"));
        assert_ne!(documents[0].id, documents[1].id);
        assert_eq!(notes(&documents[1]), ["duplicate_identity"]);
        assert!(notes(&documents[0]).is_empty(), "{:?}", documents[0].notes);
        assert_eq!(tables.path_history.len(), 2, "{:?}", tables.path_history);
    }

    #[test]
    fn a_missing_basis_derives_the_same_ids_for_unmoved_documents() {
        let entries = [(
            "decisions/2024-11-02-postgres.md",
            "---\ntitle: Adopt postgres\ndate: 2024-11-02\n---\nbody\n",
        )];
        let (first, _) = build(&entries, None);
        let (second, tables) = build(&entries, None);

        assert_eq!(first[0].id, second[0].id);
        assert!(tables.inherited.is_empty());
        assert!(tables.tombstones.is_empty());
        assert_eq!(tables.path_history[&second[0].id].len(), 1);
    }

    #[test]
    fn a_rebuild_over_the_same_basis_keeps_every_id() {
        // `--rebuild` supplies the basis and drops only the reuse source, so the
        // resolver sees exactly what it saw the first time (case 1.8).
        let basis = first_build(&[
            ("a.md", "---\ntitle: A\ndate: 2026-01-01\n---\nx\n"),
            ("b.md", "---\ntitle: B\n---\ny\n"),
        ]);
        let entries = [
            ("a.md", "---\ntitle: A\ndate: 2026-01-01\n---\nx\n"),
            ("b.md", "---\ntitle: B\n---\ny\n"),
        ];
        let (first, _) = build(&entries, Some(&basis));
        let (second, _) = build(&entries, Some(&basis));

        assert_eq!(first[0].id, second[0].id);
        assert_eq!(first[1].id, second[1].id);
    }

    #[test]
    fn a_derived_id_that_step_1_claimed_is_rejected() {
        // Case 1.13: move `A` to `B`, and create a new file at `A` whose title
        // and date differ. The new file inherits `A`'s id by path continuity, so
        // the moved document must not re-derive it from its unchanged title.
        let basis = first_build(&[(
            "a.md",
            "---\ntitle: Original\ndate: 2026-01-01\n---\nbody\n",
        )]);
        let old = DocId::from_match_key(b"title_date\x00main\x00global\x00Original\x002026-01-01");
        let (documents, _) = build(
            &[
                (
                    "a.md",
                    "---\ntitle: Replacement\ndate: 2026-02-02\n---\nother\n",
                ),
                (
                    "b.md",
                    "---\ntitle: Original\ndate: 2026-01-01\n---\nbody\n",
                ),
            ],
            Some(&basis),
        );

        assert_eq!(documents[0].id, old, "path continuity wins");
        assert_eq!(documents[0].id_source, IdentityTier::TitleDate);
        assert_eq!(
            documents[1].id,
            DocId::from_match_key(b"title\x00main\x00global\x00Original"),
            "the guard skips the claimed title+date id and takes the title"
        );
        assert_eq!(documents[1].id_source, IdentityTier::Title);
        assert_ne!(documents[0].id, documents[1].id);
    }

    #[test]
    fn an_ambiguous_key_falls_to_rel_path_when_the_path_id_is_claimed() {
        // Case 1.13b: the new file repeats `A`'s title and date, so the moved
        // document's title+date key is ambiguous as well as claimed.
        let basis = first_build(&[(
            "a.md",
            "---\ntitle: Original\ndate: 2026-01-01\n---\nbody\n",
        )]);
        let old = DocId::from_match_key(b"title_date\x00main\x00global\x00Original\x002026-01-01");
        let (documents, _) = build(
            &[
                ("a.md", "---\ntitle: Original\ndate: 2026-01-01\n---\nnew\n"),
                (
                    "b.md",
                    "---\ntitle: Original\ndate: 2026-01-01\n---\nbody\n",
                ),
            ],
            Some(&basis),
        );

        assert_eq!(documents[0].id, old);
        assert_eq!(documents[1].id, DocId::from_match_key(b"rel_path\x00b.md"));
        assert_eq!(documents[1].id_source, IdentityTier::RelPath);
        assert_eq!(notes(&documents[1]), ["duplicate_identity"]);
        assert_ne!(documents[0].id, documents[1].id);
    }

    #[test]
    fn two_consecutive_moves_keep_the_id() {
        // Case 1.14: the first move matches the frozen content hash, the second
        // matches the title+date key that the record refreshed in between.
        let basis = first_build(&[(
            "a.md",
            "---\ntitle: Postgres\ndate: 2026-01-01\n---\nbody\n",
        )]);
        let (moved, first) = build(
            &[(
                "b.md",
                "---\ntitle: Postgres\ndate: 2026-01-01\n---\nbody\n",
            )],
            Some(&basis),
        );
        assert_eq!(moved[0].id_source, IdentityTier::ContentHash);
        let basis = basis_of(&moved, first, &config());

        let (moved_again, _) = build(
            &[(
                "c.md",
                "---\ntitle: Postgres\ndate: 2026-01-01\n---\nedited body\n",
            )],
            Some(&basis),
        );

        assert_eq!(moved_again[0].id, moved[0].id);
        assert_eq!(moved_again[0].id_source, IdentityTier::TitleDate);
    }

    #[test]
    fn a_tombstoned_id_returns_with_its_flag_cleared() {
        // Case 1.15. A move does not tombstone anything (R-7): the id follows
        // the document. Only a document that leaves the corpus retires, and it
        // must come back as itself when it reappears at its first path.
        let text = "---\ntitle: Flow\ndate: 2026-01-01\n---\nbody\n";
        let first = first_build(&[("notes/flow.md", text)]);
        let (gone_docs, gone) = build(&[], Some(&first));
        let id = gone.tombstones.first().expect("a retired id").id;
        assert!(gone.path_history[&id].last().expect("an entry").tombstone);
        let basis = basis_of(&gone_docs, gone, &config());

        let (back, restored) = build(&[("notes/flow.md", text)], Some(&basis));

        assert_eq!(back[0].id, id);
        assert!(
            !restored.path_history[&id]
                .last()
                .expect("an entry")
                .tombstone,
            "claiming a retired id clears the flag"
        );
        assert!(restored.tombstones.is_empty(), "the id returned");
        assert_eq!(
            restored.path_history[&id].len(),
            1,
            "the document never left the path, so there is one entry"
        );
        assert!(restored.inherited.contains(&id));
    }

    #[test]
    fn a_retired_record_keeps_its_keys_so_it_can_be_matched_again() {
        let text = "---\ntitle: Flow\ndate: 2026-01-01\n---\nbody\n";
        let first = first_build(&[("notes/flow.md", text)]);
        let (away, away_tables) = build(&[("notes/other.md", text)], Some(&first));
        let id = away[0].id;
        let keys = away_tables.identity[&id].keys.clone();
        let basis = basis_of(&away, away_tables, &config());

        let (_, restored) = build(&[("notes/other.md", text)], Some(&basis));

        assert_eq!(restored.identity[&id].keys, keys);
        assert_eq!(
            restored.identity[&id].tier,
            IdentityTier::ContentHash,
            "a retired record is kept unchanged"
        );
    }
}
