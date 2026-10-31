//! The `janus scan` command (SPEC §9.1).
//!
//! Phase 2 scope: discovery only. This reports which files the corpus contains
//! and which were skipped and why, which already answers the single most common
//! question about a knowledge engine — *"why isn't this file searchable?"*
//! (§6.1).
//!
//! Phase 3 adds frontmatter parsing and resolved metadata to every entry;
//! phase 4 adds the versioned `--json` DTO. Both extend this module rather than
//! adding another one, so the output shape is designed once.

use std::io::{self, Write as _};
use std::path::Path;

use crate::config::Config;
use crate::error::JanusError;
use crate::vault::{self, SkipReason, WalkOptions};

/// Runs `janus scan`.
///
/// Results go to stdout, diagnostics to stderr, matching the rule that keeps
/// MCP's protocol stream clean later (§8.2).
///
/// # Errors
///
/// Propagates corpus, config and walk failures.
pub fn run(root: &Path, config: &Config) -> Result<(), JanusError> {
    let report = vault::walk(root, config, WalkOptions::default())?;

    let mut out = io::stdout().lock();

    // A write error here means the consumer closed the pipe (`janus scan |
    // head`), which is not a failure worth a non-zero exit.
    let _ = writeln!(out, "corpus:  {}", root.display());
    let _ = writeln!(
        out,
        "config:  {} (corpus_id = {:?})",
        root.join(crate::config::CONFIG_FILE).display(),
        config.corpus_id
    );

    for note in &config.warnings {
        let _ = writeln!(out, "warning: {note}");
    }

    let _ = writeln!(out);

    let mut bytes_total: u64 = 0;
    for file in &report.files {
        bytes_total += file.size;
        let _ = writeln!(out, "{:>8}  {}", file.size, file.rel_path);
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "{} file(s), {} bytes", report.files.len(), bytes_total);

    let summary = skip_summary(&report);
    if !summary.is_empty() {
        let _ = writeln!(out, "skipped: {}", summary.join(", "));
    }
    if let Some(ignored) = report.ignored_by_git {
        let _ = writeln!(out, "ignored by .gitignore: {ignored}");
    }
    for error in &report.errors {
        let _ = writeln!(out, "error: {}", error.message);
    }

    Ok(())
}

/// Renders the per-rule skip counts, omitting rules that never fired.
fn skip_summary(report: &vault::WalkResult) -> Vec<String> {
    // Fixed order rather than iteration order: the report is read by humans and
    // asserted on by tests, so it must not depend on enum layout.
    const ORDER: [SkipReason; 7] = [
        SkipReason::AlwaysExcluded,
        SkipReason::Hidden,
        SkipReason::ConfigExclude,
        SkipReason::NotMarkdown,
        SkipReason::SymlinkedDirectory,
        SkipReason::SymlinkEscape,
        SkipReason::NonUtf8Path,
    ];

    ORDER
        .iter()
        .filter_map(|reason| {
            let count = report.count(*reason);
            (count > 0).then(|| format!("{count} {reason}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn config() -> Config {
        Config::from_toml("", Path::new("<test>")).unwrap()
    }

    #[test]
    fn skip_summary_is_ordered_and_omits_zero_counts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("keep.md"), "# k").unwrap();
        fs::write(root.join("notes.txt"), "no").unwrap();

        let report = vault::walk(root, &config(), WalkOptions::hermetic()).unwrap();
        let summary = skip_summary(&report);

        assert_eq!(
            summary,
            ["1 always_excluded".to_owned(), "1 not_markdown".to_owned()]
        );
    }
}
