# janus-mem

A local-first **temporal knowledge engine** for AI agents. Indexes a directory of Markdown
journals, specs, ADRs, and principles; serves authority-weighted, time-aware retrieval over
MCP; and is the only sanctioned writer of frontmatter in the vault.

Single Rust binary. No Postgres, no graph database, no background LLM calls.

> **Status: M1 — scan and parse.** There is working code. `janus` reads a corpus'
> `janus.toml`, walks the Markdown tree with ignore rules, isolates and parses
> frontmatter (BOM, CRLF, fail-open YAML), resolves the metadata hierarchy, derives
> document identity, and reports the corpus as a table or versioned JSON. There is
> **no index, search, scoring or MCP server yet** — those are M2–M6. Scanning never
> writes to the vault.

## Usage

```bash
cargo build --release
./target/release/janus init                 # writes janus.toml + .janus/ here
./target/release/janus scan                 # aligned table + skip summary
./target/release/janus scan --json | jq .   # stable "api": 1 report
```

`--vault <path>` (or `JANUS_VAULT`) targets a corpus elsewhere. Read commands find
the corpus by walking up from the current directory; `init` deliberately does not.

`scan --json` is metadata only — never document bodies. It reports every document's
resolved `type`, `status`, `date` and `date_source` (`frontmatter`, `filename`,
`mtime`, or `none`), identity and `id_source`, plus per-rule skip counts so a missing
file has an answer.

## Documentation

- [SPEC.md](SPEC.md) — the full project specification: domain model, scoring, read/write
  APIs, architecture, milestones, and the Rust learning plan.
- [docs/m1-breakdown.md](docs/m1-breakdown.md) — the M1 phase plan and settled decisions.

## Gates

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo doc --no-deps
```

## License

MIT
