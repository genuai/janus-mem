//! The shared answer head and the stable failure codes (M6 §2, §9.5).
//!
//! Every tool surface returns one object with the same first two fields: `api`,
//! then `ok`. An agent learns that contract once and branches on `ok`, then on
//! `code`, then on `hint`. The CLI, the rpc loop and the MCP server all build
//! these types, so none of them can drift from the others.
//!
//! The envelope lives here, not in an adapter, because the spec makes it public
//! API: adding a field is additive and keeps [`API_VERSION`] at `1`, and
//! renaming one is a breaking change (§9.5).

pub mod dto;
pub mod error;

pub use crate::scan::API_VERSION;
pub use dto::{Failure, Ok};
pub use error::ApiCode;
