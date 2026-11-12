//! The value check a write runs before it plans any bytes (§2.4, M7b-7).
//!
//! A reader interprets four keys: `status`, `date`, `effective_from` and
//! `effective_to`. A write refuses a value the reader cannot interpret, because
//! the file would then hold a claim nobody reads. `date=not-a-date` looks like a
//! date in an editor and falls through to the file name in the engine.
//!
//! `type` is not checked. Types are an open registry (§6.6.1, D34): an unknown
//! name keeps its name, takes the neutral weight and earns a `doctor` note.
//! Closing that set on write is a product decision, so this module leaves it
//! open. Every other key belongs to the caller and is stored as written.
//!
//! `null` is a value, not an absence, so it is accepted on every checked key
//! (M7b-9). `effective_to: null` is the canonical spelling (SPEC §6.3).
//!
//! The accepted date set is the parser's, not a list kept here: a write must
//! never be stricter than a read of the same bytes. `parse_date` therefore
//! decides, and the two sides cannot drift.
//!
//! A key is resolved through [`Config::field_of`], so an alias is validated like
//! the canonical name: a vault whose config maps `state` to the status field
//! refuses `state=nonsense` for the same reason it refuses `status=nonsense`.

use crate::config::{Config, Field};
use crate::vault::frontmatter::parse_date;

use super::PatchError;

/// The status values a write accepts, as a message prints them.
pub const ACCEPTED_STATUS: &str = "active, draft, superseded, deprecated, unknown, null";

/// The date values a write accepts, as a message prints them.
pub const ACCEPTED_DATE: &str = "YYYY-MM-DD, an ISO-8601 timestamp, or null";

/// The five status names, lowercase, in the order the enum declares them.
///
/// [`crate::document::Status::parse`] maps every unrecognised name to `unknown`,
/// so it cannot decide whether a write value is one of the five. This list can.
const STATUS_NAMES: [&str; 5] = ["active", "draft", "superseded", "deprecated", "unknown"];

/// Whether a write may store `value` under `key`, or the refusal it earns.
///
/// `value` is the request's value: `None` writes `null`. A key the config does
/// not interpret is accepted unchanged, whatever it holds.
///
/// # Errors
///
/// [`PatchError::InvalidValue`] when `key` is one of the four checked keys and
/// `value` is neither `null` nor a value the reader accepts. The error names the
/// key, the value and the accepted set.
pub fn validate(config: &Config, key: &str, value: Option<&str>) -> Result<(), PatchError> {
    let accepted = match config.field_of(key) {
        // `None` is a `null`, which every checked key accepts.
        Some(Field::Status) if value.is_some_and(|text| !is_status(text)) => ACCEPTED_STATUS,
        Some(Field::Date | Field::EffectiveFrom | Field::EffectiveTo)
            if value.is_some_and(|text| !is_date(text)) =>
        {
            ACCEPTED_DATE
        }
        _ => return Ok(()),
    };
    Err(PatchError::InvalidValue {
        key: key.to_owned(),
        value: value.unwrap_or("null").to_owned(),
        accepted,
    })
}

/// Whether `text` is one of the five status names, compared case-insensitively.
///
/// Trimming matches the reader: `status: " active "` reads as `active`.
fn is_status(text: &str) -> bool {
    let text = text.trim().to_ascii_lowercase();
    STATUS_NAMES.iter().any(|name| *name == text)
}

/// Whether `text` is a date the reader parses.
fn is_date(text: &str) -> bool {
    parse_date(text).is_some()
}
