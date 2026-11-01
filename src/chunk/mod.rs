//! Heading-aware chunking (§6.5).
//!
//! Chunking is structural: the body is split by headings, not by a fixed
//! window, so a chunk always corresponds to a section a human recognises. The
//! work is deliberately in two layers:
//!
//! * [`blocks`] runs [`pulldown_cmark`] over the body once and returns the
//!   heading tree with **byte** spans (milestone 2 phase 1).
//! * [`split`] turns sections into [`split::Chunk`]s, applying the size limit,
//!   overlap and the content-addressed chunk id (milestone 2 phase 2).
//!
//! Splitting the parser layer from the policy layer is what keeps the borrowing
//! tractable: the event stream borrows from the body, and it is consumed once,
//! entirely inside [`blocks`]. Nothing downstream of a [`Section`] holds a
//! `Cow` or a parser lifetime — spans are plain integers, so the chunker can
//! slice, compare and reorder freely.

pub mod blocks;
pub mod split;

pub use blocks::{BlockKind, BlockSpan, Section, sections};
pub use split::{Chunk, chunk_body};
