//! `docs/corpus-config.md` states the four configuration rules, and its worked
//! example must be a valid config.
//!
//! The test reads the Markdown, extracts each fenced `toml` block, and parses it
//! with the real parser. It does not match the text with a pattern: a block that
//! reads well and does not parse is the failure this prevents, and a guide that
//! drifts from the parser is worse than no guide.

use std::fs;
use std::path::Path;

use janus_mem::config::{Config, UndatedDecay};

/// Every fenced `toml` block in `text`, in order, without its fences.
fn toml_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut open = false;
    let mut start = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let fence = line.trim_end();
        if !open && fence == "```toml" {
            open = true;
            start = offset + line.len();
        } else if open && fence == "```" {
            open = false;
            blocks.push(&text[start..offset]);
        }
        offset += line.len();
    }
    assert!(!open, "a toml block is never closed");
    blocks
}

#[test]
fn the_guide_blocks_parse_and_the_example_configures_the_four_rules() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/corpus-config.md");
    let text = fs::read_to_string(&path).expect("docs/corpus-config.md must exist");
    let blocks = toml_blocks(&text);
    assert!(
        blocks.len() >= 5,
        "one block per rule and one worked example: {}",
        blocks.len()
    );

    for (index, block) in blocks.iter().enumerate() {
        let config = Config::from_toml(block, &path).unwrap_or_else(|err| {
            panic!(
                "block {} is not a valid config ({err}):\n{block}",
                index + 1
            )
        });
        assert!(
            config.warnings.is_empty(),
            "block {} warns: {:?}",
            index + 1,
            config.warnings
        );
    }

    let example = blocks
        .iter()
        .find(|block| block.contains("corpus_id"))
        .expect("one block is the whole worked example");
    let config = Config::from_toml(example, &path).expect("the example must parse");

    // Rule 1: synthetic and retired trees are excluded, the real tree is not.
    assert!(config.is_excluded("tests/fixtures/decisions/adr.md"));
    assert!(config.is_excluded("6.archive/2020/spec.md"));
    assert!(!config.is_excluded("docs/corpus-config.md"));

    // Rule 2: a frontmatter-less tree is typed by its path.
    assert_eq!(config.path_type_hint("docs/corpus-config.md"), Some("spec"));
    assert_eq!(config.path_type_hint("SPEC.md"), Some("spec"));
    assert_eq!(
        config.path_type_hint("decisions/adr-1.md"),
        Some("decision")
    );
    assert_eq!(
        config.path_type_hint("journals/2024-01-01.md"),
        Some("journal")
    );

    // Rule 3: the alias table is real, and the order the prose states holds.
    assert_eq!(config.resolve_type("adr").name, "decision");
    assert_eq!(config.resolve_type("research").name, "finding");
    let weights = config.authority_weights();
    assert_eq!(weights.get("decision").copied(), Some(1.5));
    assert!(
        weights.get("journal") > weights.get("finding"),
        "the personal record must outrank the summary: {weights:?}"
    );

    // Rule 4: the undated rule is named, and it is the shipped default.
    assert_eq!(config.decay.undated, UndatedDecay::Median);

    // The rest of the example takes effect rather than being ignored.
    assert_eq!(config.corpus_id, "main");
    assert_eq!(config.defaults.type_name, "note");
    assert_eq!(config.decay.half_life_days, 180.0);
}
