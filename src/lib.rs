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
//! Milestone 1 implemented corpus config parsing and document scanning;
//! milestone 2 adds the chunker:
//!
//! * [`chunk`] — the heading-aware chunker: sections with byte spans (M2).
//! * [`clock`] — the injected clock, so scoring never reads the wall clock.
//! * [`config`] — `janus.toml`, alias resolution, type registry, globs, decay.
//! * [`document`] — the parsed shape of a document and its resolved metadata.
//! * [`id`] — `DocId`, `Blake3Hash`, `ChunkId` newtypes.
//! * [`ids`] — `janus ids`: the identity and match keys of each document (M5).
//! * [`index`] — the in-memory lexical index and BM25 (M3).
//! * [`link`] — build-time resolution of declared supersession links (M5).
//! * [`progress`] — the stderr progress printer for a long build (M4D).
//! * [`search`] — `janus search` and `janus facts`: the M3 command surface.
//! * [`score`] — authority, status gating, decay and scope: pure scoring (M3).
//! * [`vault`] — corpus root discovery, the directory walk, path confinement,
//!   frontmatter isolation, metadata resolution, and identity derivation.
//! * [`scan`] — the `janus scan` command (discovery report so far).
//! * [`tokenize`] — the one tokenizer shared by indexing and queries (M3).
//! * [`error`] — the single error type and its exit-code mapping.
//! * [`cli`] — the argument surface (no behaviour).
//!
//! See `SPEC.md` for the domain model, `docs/m1-breakdown.md` and
//! `docs/m2-breakdown.md` for the phase plans.

#![warn(missing_docs)]
// Library code must not panic on recoverable errors; tests may unwrap (clippy.toml).
#![warn(clippy::unwrap_used)]

pub mod chunk;
pub mod cli;
pub mod clock;
pub mod config;
pub mod document;
pub mod error;
pub mod id;
pub mod ids;
pub mod index;
pub mod link;
pub mod progress;
pub mod scan;
pub mod score;
pub mod search;
pub mod tokenize;
pub mod vault;
