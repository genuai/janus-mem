//! The one place the crate reads the wall clock (S-13, §7.2).
//!
//! Scoring is pure, so decay cannot ask the machine what day it is: "today" is
//! injected. Production passes [`SystemClock`]; tests and the eval harness pass
//! [`FixedClock`] (§3.8). This is the only module below `main.rs` allowed to
//! read the clock. Dates are UTC because frontmatter dates are calendar dates
//! with no timezone, and a machine's timezone must not move a ranking.

use chrono::{NaiveDate, Utc};

/// A source of "today", injected wherever a decision depends on the date.
pub trait Clock {
    /// The current calendar date, in UTC.
    fn today(&self) -> NaiveDate;
}

/// The production clock. Reads the wall clock exactly once per call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn today(&self) -> NaiveDate {
        Utc::now().date_naive()
    }
}

/// A clock frozen at one date, for deterministic tests and eval runs (§3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock(pub NaiveDate);

impl Clock for FixedClock {
    fn today(&self) -> NaiveDate {
        self.0
    }
}
