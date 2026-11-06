# janus-mem — point-in-time answers from your Markdown notes

Your notes and decisions live in Markdown. Over time two files disagree, and nothing tells
you which one still holds. An agent reads the older one and quotes it back to you.

janus-mem answers with the document that is still true today. It can also answer with the
one that was true on a date you name. It is one binary that reads a folder of Markdown.
It runs local-first: your files stay in git, and nothing leaves your machine.

## Do you have this problem?

- You search for a decision and find three versions. You do not know which one won.
- An agent quotes a plan you dropped last year.
- You cannot answer "what did we believe in June?" without reading everything.
- Your notes are longer than your memory of them.

If two or more are true, the missing piece is time. Your vault records what you wrote. It
does not record what replaced what.

## What it does

| You ask | It answers |
|---|---|
| What is true now? | the current document, with the replaced ones kept out |
| What was true on a date? | the document whose valid-time window holds that date |
| Which decision replaced this one? | the whole chain, oldest to newest, and its ending state |
| What is wrong with my vault? | broken links, cycles, and empty date windows, as one list |

It reads Markdown you already wrote. You add no database, no server, and no schema
migration.

## Try it in five minutes

```bash
git clone https://github.com/genuai/janus-mem
cd janus-mem
cargo build --release

export JANUS="$PWD/target/release/janus"
export VAULT="$HOME/notes"
```

```bash
"$JANUS" --vault "$VAULT" init
"$JANUS" --vault "$VAULT" index
"$JANUS" --vault "$VAULT" search "your question here"
"$JANUS" --vault "$VAULT" doctor
```

`init` writes two things into the folder: `janus.toml` and `.janus/`. Both are safe to
delete. To try it without touching your notes, copy the folder first and pass the copy.

The default build has no model and needs no network. It is a full lexical engine.

## What each command answers

```bash
janus --vault "$VAULT" search "cache strategy"
janus --vault "$VAULT" search "cache strategy" --as-of 2025-06-30
janus --vault "$VAULT" lineage decisions/2024-11-02-postgres-cache.md
janus --vault "$VAULT" doctor
janus --vault "$VAULT" moves --since 2026-01-01
janus --vault "$VAULT" ids decisions/2024-11-02-postgres-cache.md
janus --vault "$VAULT" facts
janus --vault "$VAULT" scan
```

Every report command takes `--json`, and every report carries `"api": 1`. Reads refresh a
stale index by themselves and print one `refresh:` line to stderr. They never edit your
Markdown. Every command takes `--vault <path>` or `JANUS_VAULT`; read commands also find
the corpus by walking up from the current directory.

## The part that makes it useful: dates and links

janus reads what you wrote. It does not invent metadata. Three keys do most of the work:

```yaml
---
title: Move the cache to Redis
date: 2025-03-01
type: decision
status: active
supersedes: decisions/2024-11-02-postgres-cache.md
---
```

- **`date`** turns on decay, valid-time windows and `--as-of`. A date can also come from a
  file name (`2025-03-01-plan.md`, `2025-03-plan.md`, `2026-q4-plan.md`) or from the deepest
  folder name. A file's modification time is never a date.
- **`type`** and **`status`** set authority. A decision outranks a journal. Without them,
  every document weighs the same.
- **`supersedes`** builds the supersession chain. One line connects a new decision to the
  one it replaces.

Check your own vault before you judge the tool:

```bash
janus --vault "$VAULT" scan --json | jq '[.docs[] | select(.date)] | length'
janus --vault "$VAULT" doctor
```

Most vaults have far fewer dates than their owners expect. A vault with no dates still
searches well. Its answers are flat, and `--as-of` has little to say. Dates and links are
the difference between a search engine and a knowledge engine.

## What it does not do yet

- **No MCP server.** An agent calls the CLI today. M6 adds the server and `janus rpc`.
- **No writes.** janus never edits your Markdown. Writes arrive in M7, behind an explicit
  flag and a hash check.
- **macOS and Linux.** Windows builds, but is not tested.
- **Speed is not the selling point.** Expect about a second per query on a few thousand
  documents. This is a known gap, and it is on the M8 list.

## How it works

- One Rust binary. No Postgres, no graph database, no background model calls.
- It is a temporal knowledge engine: time is part of every document, not a tag you add
  later.
- The vault is the source of truth. `.janus/` is a cache that you can delete at any time.
- Identity follows the document, not the path. A rename keeps its id, so a link still
  points at the right document.
- Nothing is guessed. An unknown type keeps its name. A missing date stays missing.
- Reads fail open. A malformed frontmatter block becomes a note, not a failed command.

## Details

**The date ladder.** A document's date comes from the first source that answers:

1. a frontmatter date key,
2. a date prefix in the file name,
3. a date in the deepest folder name.

The prefix is `YYYY-MM-DD`, `YYYY-MM`, `YYYY` or a quarter such as `2026-q4`.
Years run from 1970 to 2099, and the prefix must be a whole token. `2026-08-22_notes.md` is
a date; `2026-08-22x.md` is not. Every result reports `date_source` with the rung that
answered.

**Index freshness.** The index records a digest of the corpus it was built from. A read
walks the tree once and compares the digest. That sees an added, removed, renamed or edited
file. It does not see a content change that keeps the path, the size and the modification
time. `janus index --rebuild` recovers that case.

**Chunks.** Each body splits at headings into sections with byte spans. A chunk id is a
hash of its heading path and its normalised text. Editing frontmatter or renaming a file
re-keys nothing.

**Embeddings, opt-in.** The semantic half of hybrid search sits behind the non-default
`embeddings` feature, because it pulls ONNX Runtime through `fastembed`:

```bash
cargo build --release --features embeddings
```

`janus` never downloads a model. Put an `all-MiniLM-L6-v2` ONNX export in one directory and
point `[embedding] model_dir` at it, or set `JANUS_MODEL_DIR`. Without those files the tool
says so and searches lexically. Embedding is never required for correctness.

**Hybrid scoring.** `relevance = α · semantic + (1 − α) · lexical`, with α from
`[embedding] alpha`, default 0.5. Authority, the status gate and time decay apply on top.
`--no-embeddings` turns the semantic half off for one query and says so in `degraded`.

## Documentation

- [SPEC.md](SPEC.md) — the specification: domain model, temporal rules, scoring, interfaces,
  milestones.
- [docs/m5-breakdown.md](docs/m5-breakdown.md) — the temporal graph: identity, links,
  windows, and every decision behind them.
- [docs/m4-breakdown.md](docs/m4-breakdown.md), [docs/m3-breakdown.md](docs/m3-breakdown.md),
  [docs/m2-breakdown.md](docs/m2-breakdown.md), [docs/m1-breakdown.md](docs/m1-breakdown.md) —
  the earlier phase plans and the decisions taken while building them.
- [docs/ideas.md](docs/ideas.md) — parked ideas. Not plans, not decisions.

## Building and testing

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo doc --no-deps
scripts/eval.sh
```

## License

MIT
