//! The supersession chain read from frontmatter on disk (SPEC §7.4, M7-34).
//!
//! A write never refreshes the index (M7-17), so this module cannot read the
//! graph the index holds: that snapshot is the one the write is about to
//! invalidate. It reads the files instead, follows the two link keys by path,
//! and stops after [`LIMIT`] nodes.
//!
//! The check the caller needs is narrow. A supersession adds one edge, `old`
//! replaced by `new`. That edge closes a cycle exactly when `new` already
//! reaches `old`. A path may name a file that does not exist yet: existing
//! documents can point at the new path, and that is how a cycle is built before
//! the new file is written.
//!
//! This module reads and computes. It stages no temp file, takes no lock and
//! writes nothing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Component, Path};

use crate::config::Config;
use crate::vault::frontmatter;

/// The most nodes one chain read visits (M7-34).
pub const LIMIT: usize = 64;

/// The links one document declares.
#[derive(Debug, Clone, Default)]
struct Links {
    /// Paths this document replaced: each one points at this document.
    supersedes: Vec<String>,
    /// Paths that replaced this document: each one is pointed at by this one.
    superseded_by: Vec<String>,
}

/// The documents one chain read reached, keyed by corpus path.
#[derive(Debug, Clone, Default)]
pub struct Chain {
    /// One entry per visited path. A path with no file has empty links.
    links: BTreeMap<String, Links>,
}

impl Chain {
    /// Reads the chain around `start` from frontmatter on disk.
    ///
    /// The walk follows both link keys and treats a path with no file as a node
    /// with no links. It visits at most [`LIMIT`] nodes, so a corrupt corpus
    /// cannot turn one call into an unbounded walk.
    #[must_use]
    pub fn read(root: &Path, config: &Config, start: &str) -> Self {
        let mut links = BTreeMap::new();
        let mut queue = VecDeque::new();
        if let Some(start) = normalize(start) {
            queue.push_back(start);
        }
        while let Some(path) = queue.pop_front() {
            if links.len() >= LIMIT || links.contains_key(&path) {
                continue;
            }
            let found = read_links(root, config, &path);
            for next in found.supersedes.iter().chain(&found.superseded_by) {
                if !links.contains_key(next) {
                    queue.push_back(next.clone());
                }
            }
            links.insert(path, found);
        }
        Self { links }
    }

    /// The same chain with the proposed supersession added.
    ///
    /// Both keys describe one edge: the old document is replaced by the new one.
    /// Adding both makes the map match the bytes the call would write.
    #[must_use]
    fn proposed(&self, old: &str, new: &str) -> Self {
        let mut links = self.links.clone();
        let entry = links.entry(old.to_owned()).or_default();
        if !entry.superseded_by.iter().any(|path| path == new) {
            entry.superseded_by.push(new.to_owned());
        }
        let entry = links.entry(new.to_owned()).or_default();
        if !entry.supersedes.iter().any(|path| path == old) {
            entry.supersedes.push(old.to_owned());
        }
        Self { links }
    }

    /// The cycle the proposed supersession would close, or `None`.
    ///
    /// The answer is the path from `new` back to `old`, with both ends named.
    /// An empty answer means no cycle.
    #[must_use]
    pub fn cycle(&self, old: &str, new: &str) -> Option<Vec<String>> {
        let proposed = self.proposed(old, new);
        let graph = proposed.edges();
        let mut queue = VecDeque::new();
        let mut parent: BTreeMap<String, String> = BTreeMap::new();
        queue.push_back(new.to_owned());
        parent.insert(new.to_owned(), String::new());
        while let Some(node) = queue.pop_front() {
            if node == old {
                return Some(walk_back(&parent, old, new));
            }
            for next in graph.get(&node).map_or(&[][..], Vec::as_slice) {
                if !parent.contains_key(next) {
                    parent.insert(next.clone(), node.clone());
                    queue.push_back(next.clone());
                }
            }
        }
        None
    }

    /// The chain after the proposed supersession, oldest first.
    ///
    /// The walk follows `supersedes` back to the oldest reachable document, then
    /// `superseded_by` forward. A visited path is never added twice, so an
    /// existing cycle ends the walk instead of repeating it.
    #[must_use]
    pub fn ordered(&self, old: &str, new: &str) -> Vec<String> {
        let proposed = self.proposed(old, new);
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        proposed.ancestors(old, &mut out, &mut seen);
        proposed.descendants(old, &mut out, &mut seen);
        out
    }

    /// One edge per declaration: predecessor to replacement.
    fn edges(&self) -> BTreeMap<String, Vec<String>> {
        let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (path, links) in &self.links {
            for predecessor in &links.supersedes {
                graph
                    .entry(predecessor.clone())
                    .or_default()
                    .push(path.clone());
            }
            for successor in &links.superseded_by {
                graph
                    .entry(path.clone())
                    .or_default()
                    .push(successor.clone());
            }
        }
        for next in graph.values_mut() {
            next.sort();
            next.dedup();
        }
        graph
    }

    /// Appends the oldest ancestors of `path`, then `path`.
    fn ancestors(&self, path: &str, out: &mut Vec<String>, seen: &mut BTreeSet<String>) {
        if !seen.insert(path.to_owned()) {
            return;
        }
        if let Some(links) = self.links.get(path) {
            let mut before = links.supersedes.clone();
            before.sort();
            for predecessor in before {
                self.ancestors(&predecessor, out, seen);
            }
        }
        out.push(path.to_owned());
    }

    /// Appends the successors of `path`, nearest first.
    fn descendants(&self, path: &str, out: &mut Vec<String>, seen: &mut BTreeSet<String>) {
        let Some(links) = self.links.get(path) else {
            return;
        };
        let mut after = links.superseded_by.clone();
        after.sort();
        for successor in after {
            if seen.insert(successor.clone()) {
                out.push(successor.clone());
                self.descendants(&successor, out, seen);
            }
        }
    }
}

/// Reads one file's two link lists, or empty lists when it has no file.
fn read_links(root: &Path, config: &Config, path: &str) -> Links {
    let Ok(text) = fs::read_to_string(root.join(path)) else {
        return Links::default();
    };
    let read = frontmatter::read(&text, config);
    Links {
        supersedes: normalize_all(read.frontmatter.supersedes),
        superseded_by: normalize_all(read.frontmatter.superseded_by),
    }
}

/// A document's link values as corpus paths.
fn normalize_all(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| normalize(&value))
        .collect()
}

/// One link value as a corpus path, or `None` when it cannot be followed.
///
/// A qualified `corpus:path` link names another corpus, which M7-6 does not
/// write, so it is inert here. A path with `..` or a root escapes the corpus and
/// is inert for the same reason.
fn normalize(value: &str) -> Option<String> {
    if value.is_empty() || value.contains(':') {
        return None;
    }
    let mut parts = Vec::new();
    for part in Path::new(value).components() {
        match part {
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// The path from `new` to `old`, as the parent links recorded it.
fn walk_back(parent: &BTreeMap<String, String>, old: &str, new: &str) -> Vec<String> {
    let mut path = vec![old.to_owned()];
    let mut node = old;
    while node != new {
        let Some(previous) = parent.get(node) else {
            break;
        };
        if previous.is_empty() {
            break;
        }
        path.push(previous.clone());
        node = previous;
    }
    path.reverse();
    path
}
