//! Metadata quality: can this corpus support authority and decay at all?
//!
//! Authority is read from a document's type and decay from its date. A corpus
//! that declares neither scores every document the same way, and no ranking
//! setting can recover that. The gate of 2026-11-08 measured it: 13.8% of a
//! 2,320-document corpus declared a type, 7.3% declared a frontmatter date, and
//! 34% had neither. Nothing in the tool said so, so the corpus was tuned against
//! a denominator nobody could see (R1-3).
//!
//! This module is that denominator. It counts the resolved documents and nothing
//! else: it reads no file, writes none, and takes no clock.
//!
//! # One definition of "typed"
//!
//! **Typed** means one thing here and everywhere it is printed, in `scan`, in
//! `status`, and in `docs/corpus-config.md`: the document's resolved type is not
//! the configured default type.
//!
//! A document that falls back to `[defaults] type` is not typed for this
//! measure, although it has a type. Two corpora that declare no metadata differ
//! only in their default, and neither gains authority from it. Declaring a type
//! in frontmatter, or naming one with a `[paths]` hint, is what moves the count.
//!
//! # The two totals always reconcile
//!
//! `dated + undated` equals the number of documents, and `by_source` sums to the
//! same number, because every document has exactly one [`DateSource`].

use std::collections::BTreeMap;

use crate::config::Config;
use crate::document::{DateSource, Document};

/// How many documents carry a type and a date.
#[derive(Debug, Clone)]
pub struct Metadata {
    /// Documents whose resolved type is not the configured default type.
    ///
    /// Not the count of `type` keys in frontmatter: a `[paths]` hint types a
    /// document too, and a declared default type does not (R1-13).
    pub typed: usize,
    /// Documents whose date resolved to any source.
    pub dated: usize,
    /// Documents whose date resolved to nothing ([`DateSource::None`]).
    pub undated: usize,
    /// One count per [`DateSource`], every source present, at zero if unused.
    ///
    /// The key is the stable report name ([`DateSource::as_str`]). The map sorts
    /// by key, so the report order does not depend on file order.
    pub by_source: BTreeMap<&'static str, usize>,
}

impl Metadata {
    /// Counts one resolved document set against one config.
    ///
    /// The default type is resolved through the same alias table a document
    /// type is, so a default written as an alias (`defaults type = "log"`)
    /// still compares equal to a document that resolves to `journal`.
    pub fn of(documents: &[Document], config: &Config) -> Self {
        let default_type = config.default_type().name.into_owned();
        let mut by_source: BTreeMap<&'static str, usize> = DateSource::ALL
            .iter()
            .map(|source| (source.as_str(), 0))
            .collect();

        let mut typed = 0;
        let mut undated = 0;
        for document in documents {
            if document.type_name != default_type {
                typed += 1;
            }
            *by_source.entry(document.date_source.as_str()).or_default() += 1;
            if document.date_source == DateSource::None {
                undated += 1;
            }
        }

        Self {
            typed,
            dated: documents.len() - undated,
            undated,
            by_source,
        }
    }

    /// The documents the counts describe: `dated + undated`.
    pub fn total(&self) -> usize {
        self.dated + self.undated
    }

    /// The undated share as a whole percentage, rounded half up.
    ///
    /// Whole percent, because the JSON share and the printed line must not
    /// disagree in the last digit: [`Self::share_undated`] is this number over
    /// one hundred. A corpus with no documents reports 0%, not a division by
    /// zero.
    pub fn undated_percent(&self) -> usize {
        let total = self.total();
        if total == 0 {
            return 0;
        }
        // Integer arithmetic: no float rounding to disagree across platforms.
        (self.undated * 100 + total / 2) / total
    }

    /// The undated share in `[0, 1]`, from [`Self::undated_percent`].
    pub fn share_undated(&self) -> f64 {
        // A percentage under 101 converts exactly, so this is the printed
        // number over one hundred.
        f64::from(u32::try_from(self.undated_percent()).unwrap_or(u32::MAX)) / 100.0
    }

    /// The one line both outputs print: `28 typed, 39 dated, 16 undated (29%)`.
    pub fn summary(&self) -> String {
        format!(
            "{} typed, {} dated, {} undated ({}%)",
            self.typed,
            self.dated,
            self.undated,
            self.undated_percent()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{self, resolve};
    use std::fs;
    use std::path::Path;
    use std::time::SystemTime;

    fn config(toml: &str) -> Config {
        Config::from_toml(toml, Path::new("<test>")).expect("config must parse")
    }

    /// One document per `(rel_path, text)` pair, resolved but not identified.
    fn documents(config: &Config, files: &[(&str, &str)]) -> Vec<Document> {
        let dir = tempfile::tempdir().expect("tempdir");
        files
            .iter()
            .map(|(rel_path, text)| {
                let path = dir.path().join(rel_path);
                fs::create_dir_all(path.parent().expect("a parent")).expect("directory");
                fs::write(&path, text).expect("file");
                resolve::resolve(
                    &vault::DiscoveredFile {
                        path,
                        rel_path: (*rel_path).to_owned(),
                        size: u64::try_from(text.len()).expect("small fixture"),
                        mtime: SystemTime::UNIX_EPOCH,
                    },
                    config,
                )
                .expect("resolve must succeed")
            })
            .collect()
    }

    #[test]
    fn the_metadata_numbers_reconcile() {
        let config = config("");
        let docs = documents(
            &config,
            &[
                ("a.md", "---\ntype: decision\ndate: 2024-01-01\n---\n"),
                ("2024-02-02-b.md", "no frontmatter\n"),
                ("c.md", "---\ntitle: C\n---\n"),
            ],
        );
        let metadata = Metadata::of(&docs, &config);

        assert_eq!(metadata.dated + metadata.undated, docs.len());
        assert_eq!(metadata.by_source.values().sum::<usize>(), docs.len());
        assert_eq!(metadata.by_source[&"frontmatter"], 1);
        assert_eq!(metadata.by_source[&"filename"], 1);
        assert_eq!(metadata.by_source[&"absent"], 1);
        // 1 of 3 undated: 33%, and the share is that percentage over one
        // hundred, so the printed line and the JSON cannot disagree.
        assert_eq!(metadata.typed, 1);
        assert_eq!(metadata.undated_percent(), 33);
        assert!((metadata.share_undated() - 0.33).abs() < f64::EPSILON);
        assert_eq!(metadata.summary(), "1 typed, 2 dated, 1 undated (33%)");
    }

    #[test]
    fn a_default_typed_document_is_not_typed() {
        const FILES: [(&str, &str); 2] = [
            ("a.md", "---\ntype: decision\n---\n"),
            ("b.md", "---\ntitle: B\n---\n"),
        ];

        let base = config("");
        // `decision` is not the default; the document that falls back is not
        // typed, although it has a type.
        assert_eq!(Metadata::of(&documents(&base, &FILES), &base).typed, 1);

        // Declaring `decision` as the default makes the same corpus untyped.
        let other = config("[defaults]\ntype = \"decision\"\n");
        assert_eq!(Metadata::of(&documents(&other, &FILES), &other).typed, 0);

        // A path hint types a document too; frontmatter is not the only source.
        let hinted = config("[paths]\n\"docs/\" = \"spec\"\n");
        let docs = documents(&hinted, &[("docs/a.md", "no frontmatter\n")]);
        assert_eq!(Metadata::of(&docs, &hinted).typed, 1);
    }

    #[test]
    fn an_empty_corpus_reports_zero_every_source_and_no_division() {
        let metadata = Metadata::of(&[], &config(""));
        assert_eq!(metadata.summary(), "0 typed, 0 dated, 0 undated (0%)");
        assert!((metadata.share_undated() - 0.0).abs() < f64::EPSILON);
        assert_eq!(metadata.by_source.len(), DateSource::ALL.len());
        assert!(metadata.by_source.values().all(|count| *count == 0));
    }
}
