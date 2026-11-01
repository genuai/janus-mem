//! janus-mem: a local-first temporal knowledge engine for AI agents.
//!
//! Indexes a directory of Markdown journals, specs, ADRs and principles, and
//! serves authority-weighted, time-aware retrieval over MCP. The vault is the
//! source of truth; everything under `.janus/` is a rebuildable cache (D3).
//!
//! The library is deliberately larger than the binary: `main.rs` only parses
//! arguments, initialises tracing, and maps errors to exit codes, so the same
//! core can later be driven by MCP and JSON-RPC adapters without duplicating
//! behaviour (§9.5).
//!
//! # Layout
//!
//! Milestone 1 implements corpus config parsing and document scanning:
//!
//! * [`config`] — `janus.toml`, alias resolution, type registry, globs, decay.
//! * [`document`] — the parsed shape of a document and its resolved metadata.
//! * [`id`] — `DocId`, `Blake3Hash`, `ChunkId` newtypes.
//! * [`vault`] — corpus root discovery, the directory walk, path confinement,
//!   frontmatter isolation, metadata resolution, and identity derivation.
//! * [`scan`] — the `janus scan` command (discovery report so far).
//! * [`error`] — the single error type and its exit-code mapping.
//! * [`cli`] — the argument surface (no behaviour).
//!
//! See `SPEC.md` for the domain model and `docs/m1-breakdown.md` for the phase
//! plan.

#![warn(missing_docs)]
// Library code must not panic on recoverable errors; tests may unwrap (clippy.toml).
#![warn(clippy::unwrap_used)]

pub mod cli;
pub mod config;
pub mod document;
pub mod error;
pub mod id;
pub mod scan;
pub mod vault;
