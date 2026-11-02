# janus-mem

A local-first **temporal knowledge engine** for AI agents. Indexes a directory of Markdown
journals, specs, ADRs, and principles; serves authority-weighted, time-aware retrieval over
MCP; and is the only sanctioned writer of frontmatter in the vault.

Single Rust binary. No Postgres, no graph database, no background LLM calls.

> **Status: M4 — embeddings and hybrid index.** `janus` reads a corpus' `janus.toml`,
> walks the Markdown tree with ignore rules, isolates and parses frontmatter (BOM,
> CRLF, fail-open YAML), resolves the metadata hierarchy, derives document identity,
> and chunks bodies into heading-aware sections with byte spans. It builds a
> snapshot index (`janus index`), and serves authority-weighted, time-aware
> retrieval (`janus search`) and authoritative-fact listing (`janus facts`) with
> intents, scopes, type/tag filters and a scored breakdown. M4 adds a semantic
> signal beside BM25: an `Embedder` trait, a `vectors.f32` payload published as one
> generation with `index.bin`, incremental vector reuse, and hybrid scoring whose
> weight is `[embedding] alpha`. The vault is the source of truth; `.janus/` is a
> rebuildable cache. There is **no temporal graph or MCP server yet** — those are
> M5–M6.

## Usage

```bash
cargo build --release
./target/release/janus init                 # writes janus.toml + .janus/ here
./target/release/janus scan                 # aligned table + skip summary
./target/release/janus scan --json | jq .   # stable "api": 1 report
./target/release/janus chunks               # one block per document, one row per chunk
./target/release/janus chunks --text --limit 3
./target/release/janus chunks --json path/to/doc.md
./target/release/janus index              # build or refresh .janus/index.bin
./target/release/janus index --dry-run    # receipt only, writes nothing
./target/release/janus search cache       # one row per document, ranked
./target/release/janus search cache --intent history --json | jq .results[0].scores
./target/release/janus search cache --types decision --tags fixtures --limit 5
./target/release/janus facts              # active decisions, specs and principles
./target/release/janus facts --query cache --scope org:acme
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

`index` is the only command that writes `.janus/`. A second run re-tokenizes nothing;
an edited document re-tokenizes that document; a changed tokenizer or chunker
fingerprint rebuilds everything. `--rebuild` forces that, `--dry-run` reports the
counts without writing, and naming paths re-tokenizes just those documents.

`search` returns **one row per document** at its best chunk, sorted by score, and
applies `--limit` to documents — a document with ten good chunks appears once.
`--intent current|history|evolution|exhaustive` selects the status gate and decay,
`--include-superseded` widens `current` only, `--scope`/`--scope-only` filter by
ownership, and `--types`/`--tags` filter on persisted metadata. `--json` carries the
path, span, breadcrumb and full score breakdown (`lexical`, `relevance`,
`type_authority`, `status_factor`, `authority`, `decay`, `final`) and never chunk
text. A read auto-builds a missing index and rebuilds a stale one, printing one
`stale: <cause>` line to stderr.

`facts` lists only active decisions, specs and principles — not drafts, unknown,
deprecated or superseded documents, which `search --intent current` may still show.

## Embeddings (opt-in)

The semantic half of hybrid search is compiled behind the **non-default**
`embeddings` feature, because it pulls ONNX Runtime through `fastembed`:

```bash
cargo build --features embeddings
cargo install janus-mem --features embeddings
```

**The model is read from disk and never downloaded by `janus`.** Put the five
files of an `all-MiniLM-L6-v2` ONNX export (`model.onnx`, `tokenizer.json`,
`config.json`, `special_tokens_map.json`, `tokenizer_config.json`) in one
directory and point the tool at it with `[embedding] model_dir` in `janus.toml`
or with `JANUS_MODEL_DIR`. Without those files the command still works: it says
`model not found`, embeds nothing, and searches lexically.

A build without the feature is a full lexical engine, and a build with it still
runs lexical-only for any vault whose model is absent — embedding is never
required for correctness.

## Hybrid scoring

`relevance = α · semantic + (1 − α) · lexical`, where `semantic` is the cosine
between the query vector and the chunk's vector, `lexical` is the normalised BM25
score M3 already had, and α comes from `[embedding] alpha` (default **0.5**,
clamped to `[0, 1]`). Authority, status gating and time decay are applied on top,
unchanged. A chunk is a candidate when `relevance > 0`, so a document that shares
no word with the query can still be found; at α = 0 the ranking is byte-identical
to M3's.

To turn the semantic half off, pass `--no-embeddings` to `search` or `facts` (that
query only, α = 0, and the response's `degraded` says so), or set
`[embedding] enabled = false` / `alpha = 0.0` in `janus.toml`. `janus index
--no-embeddings` builds lexically and carries existing vectors forward when no
document's embedded text changed. The α default was chosen by measurement, not
taste: `scripts/eval.sh --sweep` prints `active top-1`, `nDCG@10`, and the queries
that the semantic term rescues or breaks, per α (`docs/m4-breakdown.md` §14).

## Documentation

- [SPEC.md](SPEC.md) — the full project specification: domain model, scoring, read/write
  APIs, architecture, milestones, and the Rust learning plan.
- [docs/m1-breakdown.md](docs/m1-breakdown.md) — the M1 phase plan and settled decisions.
- [docs/m2-breakdown.md](docs/m2-breakdown.md) — the M2 phase plan, the chunking rules, and
  the decisions taken while building them.
- [docs/m3-breakdown.md](docs/m3-breakdown.md) — the M3 phase plan: tokenizer, BM25, scoring,
  the index format, and the retrieval eval.
- [docs/ideas.md](docs/ideas.md) — parked ideas. Not plans, not decisions.

## Gates

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo doc --no-deps
scripts/eval.sh   # retrieval quality report; never a gate (§3.8)
scripts/eval.sh --sweep      # the α sweep (§3.5)
scripts/eval.sh --baseline   # the tantivy comparison (§3.8, standalone crate)
```

## License

MIT
