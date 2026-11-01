# janus-mem

A local-first **temporal knowledge engine** for AI agents. Indexes a directory of Markdown
journals, specs, ADRs, and principles; serves authority-weighted, time-aware retrieval over
MCP; and is the only sanctioned writer of frontmatter in the vault.

Single Rust binary. No Postgres, no graph database, no background LLM calls.

> **Status: M2 — chunk and preserve.** There is working code. `janus` reads a corpus'
> `janus.toml`, walks the Markdown tree with ignore rules, isolates and parses
> frontmatter (BOM, CRLF, fail-open YAML), resolves the metadata hierarchy, derives
> document identity, and reports the corpus as a table or versioned JSON. It also
> chunks a body into heading-aware sections with byte spans, ~15% overlap and
> content-addressed chunk ids, and reports those with `janus chunks`. There is
> **no index, search, scoring or MCP server yet** — those are M3–M6. Neither command
> writes to the vault.

## Usage

```bash
cargo build --release
./target/release/janus init                 # writes janus.toml + .janus/ here
./target/release/janus scan                 # aligned table + skip summary
./target/release/janus scan --json | jq .   # stable "api": 1 report
./target/release/janus chunks               # one block per document, one row per chunk
./target/release/janus chunks --text --limit 3
./target/release/janus chunks --json path/to/doc.md
```

`--vault <path>` (or `JANUS_VAULT`) targets a corpus elsewhere. Read commands find
the corpus by walking up from the current directory; `init` deliberately does not.

`scan --json` is metadata only — never document bodies. It reports every document's
resolved `type`, `status`, `date` and `date_source` (`frontmatter`, `filename`,
`mtime`, or `none`), identity and `id_source`, plus per-rule skip counts so a missing
file has an answer.

`chunks` is a read too: it runs the same pipeline, then splits each body at headings
and reports `ordinal`, content-addressed `id`, byte `start`/`end` into the body,
`chars`, and the heading breadcrumb. `--text` is the only way chunk text appears, and
`--limit N` caps rows per document. Chunk ids depend on the heading path and the
normalized text alone, so editing frontmatter or renaming a file re-keys nothing.

## Documentation

- [SPEC.md](SPEC.md) — the full project specification: domain model, scoring, read/write
  APIs, architecture, milestones, and the Rust learning plan.
- [docs/m1-breakdown.md](docs/m1-breakdown.md) — the M1 phase plan and settled decisions.
- [docs/m2-breakdown.md](docs/m2-breakdown.md) — the M2 phase plan, the chunking rules, and
  the decisions taken while building them.

## Gates

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo doc --no-deps
```

## License

MIT
