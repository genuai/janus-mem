//! The crate must not read the wall clock below `main.rs` (§7.2, S-13, §3.8).
//!
//! Scoring takes an injected [`Clock`](../src/clock.rs), and every retrieval
//! test freezes it with `FixedClock`. A `SystemTime::now()` or `Local::now()`
//! hidden in a module would make a ranking depend on the day the test ran —
//! exactly the flakiness §3.8 removes. This test is a grep, so forgetting is a
//! failing build rather than a reviewer's memory.

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn no_module_below_main_reads_the_wall_clock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        files.len() > 10,
        "the scan must actually visit the crate, saw {} files",
        files.len()
    );

    let mut offenders = Vec::new();
    for file in &files {
        if file.file_name().is_some_and(|name| name == "main.rs") {
            continue;
        }
        let text = fs::read_to_string(file).expect("source file is UTF-8");
        for needle in ["SystemTime::now", "Local::now"] {
            if text.contains(needle) {
                offenders.push(format!("{}: {needle}", file.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "wall clock read below main.rs: {offenders:?}"
    );
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("source directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
