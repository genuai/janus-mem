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
//! * [`api`] — the shared answer head and the stable failure codes (M6).
//! * [`chunk`] — the heading-aware chunker: sections with byte spans (M2).
//! * [`clock`] — the injected clock, so scoring never reads the wall clock.
//! * [`config`] — `janus.toml`, alias resolution, type registry, globs, decay.
//! * [`document`] — the parsed shape of a document and its resolved metadata.
//! * [`engine`] — the sync core one adapter calls: compute, never print (M6).
//! * [`doctor`] — `janus doctor`: one report of every visible problem (M5).
//! * [`graph`] — the supersession graph at query time: closure, cycles, scopes.
//! * [`id`] — `DocId`, `Blake3Hash`, `ChunkId` newtypes.
//! * [`ids`] — `janus ids`: the identity and match keys of each document (M5).
//! * [`index`] — the in-memory lexical index and BM25 (M3).
//! * [`lineage`] — `janus lineage`: the supersession chain of one document (M5).
//! * [`link`] — build-time resolution of declared supersession links (M5).
//! * [`mcp`] — `janus mcp`: the read tools over JSON-RPC 2.0 on stdio (M6).
//! * [`moves`] — `janus moves`: confirmed path changes and continuation hints (M5).
//! * [`progress`] — the stderr progress printer for a long build (M4D).
//! * [`read`] — `read_document`: the one path that returns document text (M6).
//! * [`rpc`] — `janus rpc`: one tool call per line on stdin and stdout (M6).
//! * [`search`] — `janus search` and `janus facts`: the M3 command surface.
//! * [`score`] — authority, status gating, decay and scope: pure scoring (M3).
//! * [`status`] — `janus_status`: freshness, embeddings, cache (M6).
//! * [`suggest`] — candidate pairs of unlinked similar documents (M5).
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

pub mod api;
pub mod chunk;
pub mod cli;
pub mod clock;
pub mod config;
pub mod doctor;
pub mod document;
pub mod engine;
pub mod error;
pub mod graph;
pub mod id;
pub mod ids;
pub mod index;
pub mod lineage;
pub mod link;
pub mod mcp;
pub mod moves;
pub mod progress;
pub mod read;
pub mod rpc;
pub mod scan;
pub mod score;
pub mod search;
pub mod status;
pub mod suggest;
pub mod tokenize;
pub mod vault;
