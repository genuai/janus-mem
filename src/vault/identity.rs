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

use std::collections::HashMap;

use chrono::NaiveDate;

use crate::config::Config;
use crate::document::{DateSource, DocNote, Document, IdentityTier};
use crate::id::DocId;

/// Separates the fields of a match key. See the module docs for why NUL.
const FIELD_SEPARATOR: u8 = 0;

/// The rungs tried in order before the `rel_path` fallback.
///
/// Rung 2 is absent on purpose: see the module docs. `rel_path` is absent
/// because it is not counted — it is unique by construction within one walk, so
/// it is the guaranteed fallback rather than a candidate.
const LADDER: [IdentityTier; 4] = [
    IdentityTier::DeclaredId,
    IdentityTier::TitleDate,
    IdentityTier::TitleVersion,
    IdentityTier::Title,
];

/// Assigns every document its [`DocId`] and records which rung produced it.
///
/// Overwrites the provisional identity resolution left behind, so a document's
/// id is always the ladder's answer for the whole set. An ambiguous key adds one
/// [`DocNote::DuplicateIdentity`] naming the strongest key that was ambiguous —
/// the reason the document fell through, which is what `doctor` reports.
pub fn derive_identity(documents: &mut [Document], config: &Config) {
    let counts = count_keys(documents, config);

    for document in documents.iter_mut() {
        let (id, tier, ambiguous) = choose(document, config, &counts);
        document.id = id;
        document.id_source = tier;
        if let Some(key) = ambiguous {
            document.notes.push(DocNote::DuplicateIdentity { key });
        }
    }
}

/// Counts how many documents carry each match key, for every rung.
///
/// One pass over the set, one map: the alternative — comparing each document to
/// every other — is O(n²) on a 20k-document vault (§13).
fn count_keys(documents: &[Document], config: &Config) -> HashMap<Vec<u8>, usize> {
    let mut counts: HashMap<Vec<u8>, usize> =
        HashMap::with_capacity(documents.len() * LADDER.len());

    for document in documents {
        for tier in LADDER {
            if let Some(key) = match_key(tier, document, config) {
                *counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    counts
}

/// Picks the strongest rung whose key is unique across the set.
///
/// Returns the id, the tier, and the rendered key of the first ambiguity (if
/// any) so the caller can explain the fall-through.
fn choose(
    document: &Document,
    config: &Config,
    counts: &HashMap<Vec<u8>, usize>,
) -> (DocId, IdentityTier, Option<String>) {
    let mut ambiguous = None;

    for tier in LADDER {
        let Some(key) = match_key(tier, document, config) else {
            continue;
        };

        // Exactly one carrier means unique. A missing count is not proof of
        // uniqueness, so it falls through like any other ambiguity.
        if counts.get(&key).copied().unwrap_or(0) == 1 {
            return (DocId::from_match_key(&key), tier, ambiguous);
        }

        ambiguous.get_or_insert_with(|| describe_key(tier, document, config));
    }

    let key = rel_path_key(document);
    (
        DocId::from_match_key(&key),
        IdentityTier::RelPath,
        ambiguous,
    )
}

/// Encodes one rung's match key, or `None` when the rung does not apply.
///
/// Availability lives here rather than in a separate predicate so there is one
/// place to read the ladder's conditions: no declared id, no usable date, no
/// version.
fn match_key(tier: IdentityTier, document: &Document, config: &Config) -> Option<Vec<u8>> {
    let mut key = Vec::with_capacity(96);
    key.extend_from_slice(tier.as_str().as_bytes());
    key.push(FIELD_SEPARATOR);

    match tier {
        IdentityTier::DeclaredId => {
            key.extend_from_slice(declared_id(document)?.as_bytes());
        }
        IdentityTier::TitleDate => {
            push_namespace(&mut key, document, config);
            key.extend_from_slice(document.title.as_bytes());
            key.push(FIELD_SEPARATOR);
            key.extend_from_slice(usable_date(document)?.to_string().as_bytes());
        }
        IdentityTier::TitleVersion => {
            // §6.4: rung 4 substitutes for rung 3 when no date is available; it
            // is not a further fallback for a document whose title+date was
            // ambiguous.
            if usable_date(document).is_some() {
                return None;
            }
            push_namespace(&mut key, document, config);
            key.extend_from_slice(document.title.as_bytes());
            key.push(FIELD_SEPARATOR);
            key.extend_from_slice(doc_version(document)?.as_bytes());
        }
        IdentityTier::Title => {
            push_namespace(&mut key, document, config);
            key.extend_from_slice(document.title.as_bytes());
        }
        // Rung 2 needs a previous document set (S-1); `rel_path` is the
        // unconditional fallback and is never counted.
        IdentityTier::ContentHash | IdentityTier::RelPath => return None,
    }

    Some(key)
}

/// The `rel_path` fallback key, which needs no uniqueness test.
fn rel_path_key(document: &Document) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + document.rel_path.len());
    key.extend_from_slice(IdentityTier::RelPath.as_str().as_bytes());
    key.push(FIELD_SEPARATOR);
    key.extend_from_slice(document.rel_path.as_bytes());
    key
}

/// Writes `corpus_id \0 scope \0` into a key.
///
/// Title-derived rungs are namespaced so `Q4 plan` can exist in two scopes, or
/// two corpora, without colliding (§6.4, §6.12 rule 5).
fn push_namespace(key: &mut Vec<u8>, document: &Document, config: &Config) {
    key.extend_from_slice(config.corpus_id.as_bytes());
    key.push(FIELD_SEPARATOR);
    key.extend_from_slice(document.scope.as_bytes());
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

/// The date identity may use: frontmatter or filename, never `mtime`.
///
/// §6.4 excludes `mtime` because clones, checkouts and copies churn it, so a
/// document whose only date came from the filesystem has no date for identity
/// purposes.
fn usable_date(document: &Document) -> Option<NaiveDate> {
    match document.date_source {
        DateSource::Frontmatter | DateSource::Filename => document.date,
        DateSource::Mtime | DateSource::None => None,
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
    use crate::vault::DiscoveredFile;
    use crate::vault::resolve;
    use std::fs;
    use std::path::Path;
    use std::time::SystemTime;

    fn config() -> Config {
        Config::from_toml("", Path::new("<test>")).expect("test config must parse")
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
                resolve::resolve(&file, config).expect("resolve must succeed")
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
    fn the_content_hash_is_not_a_stateless_rung() {
        let config = config();
        let documents = resolved(&[("a.md", "x")], &config);

        assert_eq!(
            match_key(IdentityTier::ContentHash, &documents[0], &config),
            None,
            "rung 2 needs a previous document set (S-1)"
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
    fn mtime_never_contributes_to_identity() {
        // No date and no version: rung 3 is unavailable, so the unique title wins.
        let documents = derived(&[("notes.md", "x")]);

        assert_eq!(documents[0].date_source, DateSource::Mtime);
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
        derive_identity(&mut forward, &config);

        let mut backward = resolved(&entries, &config);
        backward.reverse();
        derive_identity(&mut backward, &config);

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

        derive_identity(&mut plain, &config);
        derive_identity(&mut shared, &config);

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
}
