//! The progress printer for a long build (M4D §3.6).
//!
//! One printer, so `janus index` and a build a read triggers say the same words.
//! The library reports through [`EmbedSink`]; this module decides whether a
//! human sees it, and how often.
//!
//! ```
//! embedded 8192/27234 chunks (30%)  ~12 min left
//! ```
//!
//! The gate is a plain `bool`, resolved once at the edge from `--progress` and
//! `stderr().is_terminal()`. No code below `main` reads the terminal, so a
//! captured stderr is silent on any machine and no test depends on a terminal.
//!
//! Two rules keep the output small:
//!
//! * At most one line per 5 % step of the chunks this build embeds, so at most
//!   twenty lines. The sink fires once per chunk; this module divides.
//! * The estimate comes from the observed rate and is rounded to a whole minute.
//!   No test asserts it, because it depends on the machine (D-8).
//!
//! [`EmbedSink`]: crate::index::build::EmbedSink

use std::cell::Cell;
use std::time::Instant;

/// How many steps one build reports: 5 % each.
const STEPS: u64 = 20;

/// Prints progress for one build. Construct one per command, at the edge.
pub struct Printer {
    /// False when the gate is off: every method returns without writing.
    enabled: bool,
    /// The number of texts this build embeds, from the first report.
    total: Cell<u64>,
    /// The last step printed, so a repeated step prints nothing.
    last_step: Cell<u64>,
    /// When the printer was built, which is the start of the work it reports.
    started: Instant,
}

impl Printer {
    /// A printer that writes only when `enabled` is true.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            total: Cell::new(0),
            last_step: Cell::new(0),
            started: Instant::now(),
        }
    }

    /// Reports one embedded chunk, `done` of `total` for this build.
    ///
    /// The cost per call is one division and one comparison, because the sink
    /// fires once per chunk and a build can embed tens of thousands.
    pub fn report(&self, done: u64, total: u64) {
        if !self.enabled || total == 0 {
            return;
        }
        self.total.set(total);
        let step = done.saturating_mul(STEPS) / total;
        if step >= 1 && step != self.last_step.get() {
            self.last_step.set(step);
            eprintln!("{}", self.line(done, total, step));
        }
        if done >= total {
            // The summary prints once, after the last chunk. A build that
            // embeds nothing never reaches this line.
            eprintln!("embedded {total} chunks");
        }
    }

    /// The line for one step: the position, the percentage and the estimate.
    fn line(&self, done: u64, total: u64, step: u64) -> String {
        let percent = step.saturating_mul(100) / STEPS;
        let mut line = format!("embedded {done}/{total} chunks ({percent}%)");
        if let Some(minutes) = self.minutes_left(done) {
            line.push_str(&format!("  ~{minutes} min left"));
        }
        line
    }

    /// Whole minutes left at the rate observed since the printer was built.
    ///
    /// `None` until at least one chunk has finished and some time has passed:
    /// a rate needs both, and a wrong estimate is worse than none.
    fn minutes_left(&self, done: u64) -> Option<u64> {
        let seconds = self.started.elapsed().as_secs_f64();
        if done == 0 || seconds <= 0.0 {
            return None;
        }
        let rate = done as f64 / seconds;
        let left = self.total.get().saturating_sub(done) as f64 / rate;
        Some((left / 60.0).round() as u64)
    }
}
