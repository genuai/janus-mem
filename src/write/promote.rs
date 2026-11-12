//! Promotion: declare an archived document as lineage for a primary one
//! (SPEC §6.11, plan §3.7, M7-23).
//!
//! Promotion never moves and never rewrites the archive. The one write a call
//! makes lands in the corpus it names:
//!
//! ```text
//! link   patch `supersedes` of one primary document with `corpus:path`
//! adopt  create one primary document that carries the source's text
//! ```
//!
//! Both write the qualified text, so a reader of the primary corpus can see
//! where the document came from. The source document keeps its bytes and its
//! modification time: this module reads it and nothing else (case 7.3).
//!
//! This module is pure. It builds the two things the engine then writes — the
//! key values of an adopted copy and the refusal a link owes a document that
//! already names another one — and it touches no file, takes no lock and maps no
//! error to a code.

use std::collections::BTreeMap;

use crate::config::Config;
use crate::vault::frontmatter;

/// What one promotion copies from the source corpus (§3.7).
///
/// The engine read the bytes, because a pure module reads no file, and the link
/// text is the qualified form the target document holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The workspace's id for the corpus the source lives in.
    pub corpus: String,
    /// The source path inside that corpus, `/`-separated (§8.7).
    pub rel_path: String,
    /// The source document's body, byte for byte.
    pub body: String,
    /// The source document's declared title, when it has one.
    pub title: Option<String>,
}

impl Source {
    /// The link text a promotion writes: `corpus:path` (§6.11).
    #[must_use]
    pub fn qualified(&self) -> String {
        format!("{}:{}", self.corpus, self.rel_path)
    }
}

/// Why a promotion was refused, before any byte was staged.
///
/// One variant per refusal, and no path: the caller that owns the paths adds
/// them (M7-4). Every variant is a refusal, never a partial write.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PromoteError {
    /// The target document already declares a different `supersedes` value.
    ///
    /// The writer replaces one value and cannot append to a list, so a link
    /// would drop the one the document already holds. A caller that needs both
    /// links edits the file itself.
    #[error("the document already names {current} in `supersedes`, and a promote writes one link")]
    AlreadyLinked {
        /// The value the document declares today.
        current: String,
    },
}

/// The frontmatter one adopted copy carries (case 7.2).
///
/// The keys are the ones the request implies: the qualified source, which is
/// what makes the copy lineage, and the source's own title, so the copy reads as
/// the document it came from. Nothing else is copied: a type or a status carried
/// across a corpus would let archived material vote in the primary one (D42).
#[must_use]
pub fn adopted(source: &Source) -> BTreeMap<String, Option<String>> {
    let mut keys = BTreeMap::new();
    if let Some(title) = &source.title {
        keys.insert("title".to_owned(), Some(title.clone()));
    }
    keys.insert("supersedes".to_owned(), Some(source.qualified()));
    keys
}

/// Refuses a link patch that would replace a different link (case 7.1).
///
/// `text` is the target document as it is on disk now, and `qualified` is the
/// link this call writes. A document that already names the same source needs no
/// guard: the patch reports no changed key, which is the receipt's job (M7-44).
///
/// # Errors
///
/// [`PromoteError::AlreadyLinked`] when `supersedes` names another document.
pub fn check_link(text: &str, config: &Config, qualified: &str) -> Result<(), PromoteError> {
    let read = frontmatter::read(text, config);
    let Some(current) = read.frontmatter.supersedes.first() else {
        return Ok(());
    };
    if current == qualified {
        return Ok(());
    }
    Err(PromoteError::AlreadyLinked {
        current: current.clone(),
    })
}
