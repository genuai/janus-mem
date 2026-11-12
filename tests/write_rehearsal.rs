//! The rehearsal as an assertion: run `scripts/write-rehearsal.sh` and pin the
//! numbers it reports (M7-8).
//!
//! The script builds 200 documents, one per shape the scanner knows, runs the
//! four write commands with `--dry-run` over them, and compares every file byte
//! for byte. This test runs that same measurement, so the numbers a person reads
//! from the script are the numbers CI asserts. The script exits non-zero on a
//! changed byte, a refusal that misses its shape, or a corpus that is not 200
//! documents; this test names which of those broke.
//!
//! The rehearsal needs a shell and a POSIX `diff`, so it runs where the suite
//! runs (Linux and macOS). It sleeps nowhere, reads no clock, opens no port and
//! touches no file outside the temporary corpus the script builds.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::process::Command;

/// The script, relative to the crate root, which is where `cargo test` runs.
const SCRIPT: &str = "scripts/write-rehearsal.sh";

/// The refusals the corpus owes, by command, shape and count.
///
/// Thirty-six come from `set` (the patcher sees every shape), seven from
/// `supersede` (which patches `status`), and three from `promote` (which needs a
/// frontmatter block). `write` replaces the whole file, so it refuses none.
const REFUSALS: [(&str, &str, usize); 10] = [
    ("promote", "no_frontmatter", 3),
    ("set", "alias", 4),
    ("set", "anchor", 5),
    ("set", "block_sequence", 8),
    ("set", "duplicate_key", 4),
    ("set", "flow_mapping", 6),
    ("set", "multiline", 6),
    ("set", "no_frontmatter", 3),
    ("supersede", "duplicate_key", 4),
    ("supersede", "no_frontmatter", 3),
];

/// One rehearsal run: its exit code, its report and its diagnostics.
struct Rehearsal {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Rehearsal {
    /// Runs the script against the binary this test built.
    fn run(extra: &[&str]) -> Self {
        let output = Command::new("bash")
            .arg(SCRIPT)
            .arg("--janus")
            .arg(env!("CARGO_BIN_EXE_janus"))
            .args(extra)
            .env("RUST_LOG", "off")
            .output()
            .expect("run the rehearsal script");
        Self {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        }
    }

    /// One number from the report, by its label.
    fn number(&self, label: &str) -> usize {
        let line = self
            .stdout
            .lines()
            .find(|line| line.trim_start().starts_with(label))
            .unwrap_or_else(|| panic!("no `{label}` line in the report:\n{}", self.stdout));
        line.split_whitespace()
            .nth(1)
            .and_then(|value| value.trim_end_matches(':').parse().ok())
            .unwrap_or_else(|| panic!("`{label}` is not a number: {line}"))
    }

    /// The refusals, as `(command, shape, count)`.
    fn refusals(&self) -> BTreeMap<(String, String), usize> {
        self.stdout
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let count: usize = words.next()?.parse().ok()?;
                let command = words.next()?.to_owned();
                let shape = words.next()?.to_owned();
                words.next()?;
                Some(((command, shape), count))
            })
            .collect()
    }
}

#[test]
fn rehearsal_visits_200_documents_changes_no_byte_and_names_every_refusal() {
    let rehearsal = Rehearsal::run(&[]);
    assert_eq!(
        rehearsal.code,
        Some(0),
        "the rehearsal failed:\n{}\n{}",
        rehearsal.stdout,
        rehearsal.stderr
    );
    assert_eq!(rehearsal.number("visited"), 200, "files visited");
    assert_eq!(rehearsal.number("changed"), 0, "files a dry run changed");
    assert_eq!(
        rehearsal.number("added/removed"),
        0,
        "files added or removed"
    );

    let owed: BTreeMap<(String, String), usize> = REFUSALS
        .iter()
        .map(|(command, shape, count)| (((*command).to_owned(), (*shape).to_owned()), *count))
        .collect();
    assert_eq!(rehearsal.refusals(), owed, "refusals by command and shape");
    assert_eq!(
        rehearsal.number("refusals"),
        46,
        "every refusal is one of the shapes the corpus was built for"
    );
}

#[test]
fn the_byte_comparison_can_fail() {
    // One byte appended to one document: the report must say so and the script
    // must exit non-zero. A measurement that cannot fail measures nothing.
    let rehearsal = Rehearsal::run(&["--prove-it-fails"]);
    assert_eq!(rehearsal.code, Some(1), "the sabotaged run must fail");
    assert_eq!(rehearsal.number("changed"), 1, "the one sabotaged document");
    assert!(
        rehearsal
            .stderr
            .contains("FAIL: a dry run touched the corpus"),
        "stderr:\n{}",
        rehearsal.stderr
    );
}
