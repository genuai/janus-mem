# janus-mem — Project Specification

**A local-first temporal knowledge engine for AI agents, written in Rust.**

| | |
|---|---|
| Status | Draft v0.1 — awaiting review |
| Last updated | 2026-10-29 |
| Author | genuai |
| Repo | github.com/genuai/janus-mem |
| License | MIT |
| Primary goal | Learn Rust by shipping a tool its author uses daily |
| Secondary goal | Fill a real gap in the local-first AI memory ecosystem |

---

## 0. How to read this document

This spec is written for two audiences at once:

1. **The product.** Sections 1–9 define what janus-mem is, what it does, and how it behaves.
2. **The learner.** Sections 10–12 define *how it gets built* so that the primary goal (learning Rust) is actually served. Those sections are not optional garnish; they are requirements.

Anything marked **DECISION GATE** is not yet settled and must be resolved at the milestone where it first matters, with the resolution recorded in §15.

---

## 1. Summary

janus-mem is a single-binary, offline, read-mostly **temporal knowledge engine** over any directory tree of Markdown files — journals, specs, ADRs, meeting notes, research notes, or whatever structure a vault already has. It exposes them to AI agents over **MCP** and to humans over a **CLI**.

Its differentiators are narrow and deliberate:

- **Temporal truth.** Every document has a status, a date, and an optional supersession chain. Queries can ask for *current truth*, *historical truth*, or *how the truth changed*.
- **Authority-weighted retrieval.** A formal decision outranks a passing journal musing on the same topic, even if the journal is more semantically similar.
- **Byte-safe writes.** janus-mem never reformats, reorders, or drops frontmatter it did not intend to change. It is the *only* writer to the vault.
- **Zero infrastructure.** One binary, no Postgres, no Docker, no graph database, no API calls, no background LLM passes.

The release artifact is `janus` — a ~10–30 MB native binary that indexes a vault in seconds, answers a hybrid query in tens of milliseconds, and speaks MCP over stdio. Single-digit-millisecond queries are the aspiration and the likely steady state on small vaults, **not a v1 requirement**: correctness, safety, and learnability come first (§13).

---

## 2. Goals and non-goals

### 2.1 Goals

| # | Goal | Measure |
|---|---|---|
| G1 | Answer "what do I currently believe / have I decided about X?" from a local Markdown vault | Top-3 results are the active authoritative docs ≥ 90% of the time on a hand-labelled eval set |
| G2 | Answer "how did my thinking on X evolve?" | `lineage` returns a correct chronological chain for any doc with `supersedes` links |
| G3 | Never corrupt a source file | Byte-identical file except for intentionally patched frontmatter keys; enforced by tests |
| G4 | Be fast enough to call on every agent turn | Warm query p50 < 10 ms at 200k chunks; index p50 < 60 s for 20k docs |
| G5 | Be installable trivially | `cargo install janus-mem`, `brew install`, or a GitHub release tarball; no runtime deps |
| G6 | Teach the author Rust | Every milestone is completable by the author, with AI in a review/explain role (§12) |

### 2.2 Non-goals (v1.0)

- **Not** a cloud service, sync engine, or multi-user server.
- **Not** an Obsidian/Ollama/whatever plugin. It may be *wrapped* by one later.
- **Not** a note editor or note-taking UI. No TUI beyond `search`/`lineage` output.
- **Not** a chat app and does not call an LLM to summarize or "reflect." Retrieval must be deterministic and cheap.
- **Not** a general RAG framework or a library-first crate. It is an application that happens to expose a small library.
- **Not** a knowledge graph database. Entity/relation extraction is out of scope; supersession between *documents* is in scope. What this means in practice is explained below.
- **Not** a Python extension (no PyO3). Deliberate: the point is to write Rust, not to hide it.
- **No `unsafe`.** Not because it's forbidden, but because this project does not need it and it removes a whole class of distraction.
- **No multi-corpus federation** in v1 (one primary root per process), no remote embeddings, no cross-encoder reranking in v1. However, the corpus model is specified now (§6.11) and every persisted structure is corpus-scoped (declared `corpus_id`, vault-relative paths, one index per root), so archived corpora and a registry are additive later, not a format rewrite.

**What “no entity/relation extraction” means.** janus-mem does not try to identify people, organizations, concepts, or arbitrary relationships between them, and does not build a graph of “Alice works at X” edges. It indexes documents and the *document-level* temporal relations the frontmatter declares: this doc supersedes that doc, this doc is deprecated, this doc is a principle.

That is still good fact-and-decision querying, because the questions a working vault actually asks are document-shaped:

- “What do we currently believe about X?” → authority + status filter + hybrid retrieval. Strong.
- “Why did we change our mind?” → supersession chain + full document text. Strong.
- “What's the exact wording of the decision?” → chunk retrieval with source spans. Strong.
- “Which people on team A made decisions that affect project B?” → requires joining entities across documents. **Weak; out of scope.** Use a real graph system or a query over your task tracker.

Multi-hop *entity* queries are the capability traded away. Multi-hop *document lineage* queries are the capability kept.

### 2.3 Success test

> The author stops grepping their vault and stops letting agents `grep -r` in it, for a month.

If that doesn't happen, the project failed regardless of benchmark numbers.

---

## 3. Why this project exists (gap analysis)

| System | Model | Why it's not the right fit here | Verified |
|---|---|---|---|
| **basic-memory** | Local Markdown + wikilinks, hybrid BM25 + embeddings, MCP server | Treats notes as static documents: no validity windows, no decay, no supersession. Its write path can rewrite frontmatter and clobber user keys. | ✅ Author used it; ✓ write behavior observed directly |
| **Mnemosyne** | Local SQLite memory store with a `remember()`-style API | Stores *conversation-derived facts* in a DB rather than indexing an existing Markdown workspace. | ⚠ Several projects share this name; re-verify which one is meant |
| **Hindsight** | Hosted/self-hosted agentic memory with reflection passes over a vector store | Heavy for a single-user vault: server infrastructure plus continuous LLM work for consolidation. Reported strong long-horizon memory results. | ⚠ Details (Postgres/pgvector, call volume) come from a design conversation; re-verify before publishing any comparison |
| **Zep / Graphiti** | Bi-temporal knowledge graph over a graph database (Neo4j, FalkorDB, and others) | Excellent temporal edge model, but requires a graph DB and does not treat Markdown as the source of truth. Edges that expire without a replacement can remain active. | ✅ Public docs; backend list may have grown |
| **Mem0** | LLM-driven fact extraction with ADD/UPDATE/DELETE (plus optional graph memory) | Flattens evolving beliefs into current-state assertions, losing the history of *how* the belief changed. | ✅ Public docs |
| **Letta / MemGPT** | OS-style memory hierarchy; the agent pages memory in and out via tools | Paging is the agent's responsibility, so agents skip it and lose history; also conversation-first rather than document-first. | ✅ Public docs |
| **Generative Agents** | Memory stream scored by recency × importance × relevance | Pure mathematical decay penalizes timeless facts (principles, milestones) as hard as small talk. | ✅ Paper |
| **`grep` / ripgrep** | Literal match | No semantics, no authority, no recency, no lineage. The current fallback. | ✅ |

**Verification policy.** Rows marked ⚠ were characterized from the design conversation and must be re-checked against current upstream docs before any public claim, README comparison, or benchmark. Everything here is a *fit* judgment for this use case, not a quality judgment: several of these systems are excellent at what they target, and this table is not a competitive attack. Where a claim is uncertain, `⚠` wins over confidence.

The niche janus-mem occupies: **files are the source of truth, time is a first-class dimension, and retrieval is cheap enough to run on every turn.**

### 3.1 “Agent memory platform” vs. janus-mem

These terms sound adjacent, so the boundary is explicit:

| | Agent memory platform (Mem0, Letta, Zep, Mnemosyne, Hindsight) | janus-mem |
|---|---|---|
| **Source** | Conversation turns, tool traces, runtime observations | Files in the vault — **regardless of who typed them**: human, agent, or both |
| **Who decides what counts** | The system, usually an LLM: extract, summarize, consolidate, delete | The user, by keeping the file. janus-mem adds no knowledge and removes none |
| **Authorship** | Implicit; memory is captured, not authored | Explicit and auditable: every claim has a file, a date, and a git diff |
| **Lifecycle** | Owns it: decay, forget, reflect, rewrite | None. Status and supersession are declared in the files |
| **Truth** | The database | The Markdown files; the index is a rebuildable cache |
| **Writes** | Frequent, automatic | None by default; only explicit, etagged, surgical patches |
| **Output** | A prompt-ready memory blob for one agent | Ranked, sourced, dated evidence for any agent or human |
| **Failure mode** | Silent memory drift; history flattened | Stale links reported by `doctor`; files never corrupted |

The distinction is **not** who wrote the document. In this project's expected workflow, most documents are AI-authored from the user's directives, prompts, and journals, and that is a first-class, normal case. What separates the two categories is *how knowledge enters the system*:

- **Persistence, not capture.** A spec drafted by an agent from your prompt, saved into the vault, and visible in `git log` is durable knowledge: you chose to keep it, and it carries its own date, status, and version. A fact a memory platform extracted from a throwaway sentence is agent state, curated by a model.
- **The artifact is the unit.** janus-mem never decides that a sentence is worth remembering; it answers questions about files that already exist and are already under version control.
- **Provenance stays visible.** Whoever wrote a document, the answer comes back with a path, a date, a status, and (optionally) a declared `author`. Nothing arrives as an unattributed "memory".

They are complementary, not competing: a memory platform is right for ephemeral conversational state, janus-mem is right for durable, auditable, agent- or human-authored artifacts. Using both is the expected configuration. What janus-mem refuses to become is a system that *generates* memory — because generation is exactly where history gets lost.

---

## 4. Users and workflows

**Primary user: the author.** An existing Markdown directory tree — any structure, any folder names — written and maintained by a mix of the author and their AI agents. AI-authored documents generated from the author's directives, prompts, and journals are a normal and expected part of the corpus, not a special case. `decisions/`, `specs/`, `journals/` are examples of conventions the tool *understands*, never requirements it imposes.

**Rule: janus-mem adapts to the vault, not the other way around.** No required folder layout, no required filenames, no required frontmatter. Path-based type inference is a convenience with configurable patterns and a `note` fallback (§6.3). If a user wants a starting structure, a future `janus scaffold` command can generate one — but that is a separate, opt-in convenience, not a precondition for using the tool.

### 4.1 Workflows

| ID | Workflow | Actor | Surface |
|---|---|---|---|
| W1 | "What is our current database decision?" | Agent | MCP `search` (intent `current`), `authoritative_facts` |
| W2 | "Why did we move off Postgres?" | Agent | MCP `lineage`, then `read_document` |
| W3 | "What are all my principles about pricing?" | Agent | MCP `authoritative_facts` with tags |
| W4 | "Record that we now use SQLite; retire the Postgres ADR." | Agent, on explicit instruction | MCP `supersede_document` |
| W5 | "Show me how my journal's opinion of remote work changed." | Human | CLI `janus lineage --topic "remote work"` |
| W6 | "Keep the index fresh after I edit files by hand." | Human | CLI `janus index` (manual) or `janus watch` (M8) |
| W7 | "Which docs are stale or orphaned?" | Human | CLI `janus doctor` (index health, broken links, missing dates) |

Non-workflow: janus-mem is never invoked as part of a *write* path the user did not ask for. No background mutation of content, ever.

---

## 5. Design principles

1. **Files are truth; the index is a rebuildable cache.** Delete `.janus/` and nothing is lost. The index is derived data with an explicit `schema_version`; a mismatch triggers a rebuild, never a migration.
2. **Read-only by default for agents.** `search`, `lineage`, `read_document` are always safe. Write tools are opt-in per process (`--allow-write`) because an agent with a filesystem is a liability.
3. **Surgical writes only.** A write either succeeds without touching a single unintended byte, or it fails loudly. There is no "best effort" YAML re-serialization.
4. **One writer.** janus-mem is the only sanctioned writer of frontmatter in the vault. Agents should not edit `.md` files directly for metadata changes.
5. **Deterministic.** Same vault + same index + same query = same ranked list, byte for byte. No wall-clock in scoring without an injected `Clock`; no hash-map iteration order in output.
6. **Offline.** No network at query time. Network allowed only for first-run model download, and only with `--download` / `JANUS_ALLOW_DOWNLOAD=1`.
7. **Boring technology.** No async where sync suffices, no unsafe, no custom macros, no distributed anything.
8. **Small surface.** Four read tools, four write tools. Every additional tool is a maintenance and prompt-budget cost.
9. **Git is a co-tenant, not a bystander.** The vault is version-controlled. janus-mem never touches `.git/`, never stages or commits, respects `.gitignore`, keeps its own state in one ignorable directory, produces minimal diffs, and preserves line endings so `git diff` stays readable (§8.6).

### 5.1 Engineering principles (SOLID, DRY, TDD, DI)

These exist to keep the project maintainable *and* to give the Rust learning a shape beyond "make it compile":

- **Single responsibility.** `vault` parses, `index` stores, `score` scores, `write` writes, adapters adapt. If a function needs the word "and" twice in its doc comment, split it.
- **Dependency inversion where it buys testability.** The engine takes its dependencies explicitly — `Clock`, `Embedder`, `Store`, `Config` — rather than reaching for `SystemTime::now()`, a global, or a hard-coded model. `FixedClock` and `HashEmbedder` make every scoring and retrieval test deterministic.
- **Open for extension, closed for modification.** New query intents, embedders, and index backends arrive behind traits or new enum variants; they do not require editing the scoring core or the CLI parser.
- **DRY, with judgment.** Duplicate code is cheaper than the wrong abstraction, especially in Rust where a premature trait can drag lifetimes everywhere. Three similar lines are fine; the third *occurrence in a different module* is the signal to extract.
- **TDD for rules, not for glue.** Anything with an invariant — scoring, decay, status gating, span patching, identity resolution — gets a failing test first. CLI formatting and serde plumbing are allowed to be written glue-first.
- **No hidden globals.** No `static mut`, no ambient singletons, no reading `std::env` deep inside the engine. Environment and config are resolved once at the boundary and passed down. `cargo clippy` plus a code-review checklist enforces this socially until it can be enforced mechanically.

---

## 6. Domain model

### 6.1 Vault

A vault is a directory tree. Config lives at `<vault>/janus.toml` or `<vault>/.janus/config.toml`; the index lives in `<vault>/.janus/`. Root resolution order: `--vault <path>` → `JANUS_VAULT` → nearest ancestor containing `janus.toml` or `.janus/` → error.

Default ignore rules: `.git/`, `target/`, `node_modules/`, `.janus/`, `.obsidian/`, plus `.gitignore` semantics (use the `ignore` crate).

**Only `.md` and `.markdown` are indexed.** Images, media, PDFs, archives, binaries, and plain text (`.txt`, `.org`, `.rst`) are never opened or parsed. Indexing non-Markdown text is a deliberately deferred feature: without frontmatter those files have no type, status, date, or supersession, so they can only enter as low-authority `note` documents and would dilute results from a vault where the meaningful content is Markdown. (Tracked in §15 as an open question, not built now.)

Exclusion is configurable; inclusion of other formats is not:

```toml
[index]
exclude = ["archive/private/**", "**/*.draft.md"]
```

`exclude` globs win over everything; `.janus/` and `.git/` are always excluded. The index summary and `doctor` report how many files were skipped and by which rule, so "why isn't this file searchable?" is answerable in one command.

### 6.2 Document

```rust
pub struct Document {
    pub id: DocId,                  // stable identity, see §6.4
    pub path: PathBuf,              // absolute, canonicalized, inside vault
    pub rel_path: String,           // vault-relative, '/'-separated, stable in output
    pub content_hash: Blake3Hash,   // of the full file bytes
    pub frontmatter: Frontmatter,   // typed view over a subset of keys
    pub raw_frontmatter: Option<String>, // YAML text, verbatim
    pub body: String,               // Markdown body, verbatim
    pub span: FrontmatterSpan,      // byte offsets of the patched region
    pub mtime: SystemTime,
    pub size: u64,
}
```

`raw_frontmatter`, `body`, and `span` exist so the writer never has to re-serialize what it did not change (§7.3).

### 6.3 Frontmatter contract

janus-mem reads a *subset* of frontmatter keys and ignores — and preserves — everything else. Key names are configurable because real vaults are not uniform.

Canonical keys (defaults; aliases configurable per §16.1):

```yaml
---
id: 01JABC...            # optional; any opaque string. Omit it and identity is derived (§6.4)
author: "claude-opus"     # optional provenance; opaque, never inferred, not scored in v1
title: "Local Caching Architecture"
type: decision           # open set; defaults: decision|spec|principle|finding|journal|idea|note
status: active           # active | draft | superseded | deprecated
date: 2026-10-29         # authored/decided date; falls back to mtime
effective_from: 2026-11-01  # optional valid-time start; defaults to date
effective_to: null          # optional valid-time end (exclusive); usually derived
supersedes: decisions/2024-11-02-postgres-cache.md
superseded_by: null      # maintained by janus-mem when superseding
doc_version: "2.0"       # human-facing doc revision
schema_version: 1        # frontmatter schema revision (Claude convention)
tags: [database, rust, architecture]
timeless: false          # opt out of decay; default true for principle/decision
---
```

Rules:

- Missing `type` → inferred from **configurable path patterns** (defaults recognize `decisions/`, `adr/`, `specs/`, `principles/`, `research/`, `findings/`, `ideas/`, `journals/`), else `note`. A vault with folders named `notes/`, `archive/`, or `2026/Q4/` loses nothing: everything falls back to `note` and authority weighting stays flat until the pattern list is extended.
- Missing `status` → `active` for every type (never `draft` by default).
- Missing `date` → parse `YYYY-MM-DD` prefix from filename, else frontmatter `created`, else filesystem mtime. Record the *source* of the date (`filename | frontmatter | mtime`) and surface it in responses so the agent can judge confidence.
- `effective_from` / `effective_to` are optional valid-time keys with their own semantics (§6.10). They never fall back to mtime; if absent, the window is derived from `date` and the supersession chain. When the only available date came from mtime, the document has no trustworthy valid time and is excluded from `as_of` queries with a `no_validity` note.
- Unknown `status` is not an error: it maps to `status: unknown` and gets the 0.8× gate factor plus a `doctor` note. Unknown `type` keeps its name (types are an open registry, §6.6.1) and gets the fallback weight 0.80 plus a `doctor` note. Fail-open on read, fail-closed on write.
- Files with no frontmatter are fully supported and are **never** mutated to add one unless the user explicitly calls `write_document` with frontmatter for that path.
- `doctor` reports: duplicate `id`, missing dates, `supersedes` targets that don't exist, broken chains (A superseded_by B, B superseded_by C but C doesn't supersede B), and `superseded_by` without a reciprocal `supersedes`.

### 6.4 Identity

Identity is **matched from metadata the document already contains**. Nothing is written to the vault, and a fresh vault works with zero setup — no migration command, no frontmatter mutation, no prompt.

Two mechanisms, in this order:

1. **Carry-forward (the common case).** A document that keeps its path keeps its `DocId`, whatever happens to its content. Identity is decided once and not recomputed on every index pass. A title edit, a date correction, or a `doc_version` bump in place changes nothing.
2. **Derivation (new path, or brand-new document).** `DocId = blake3(match_key)`, using the first *unambiguous* key below. On a move, the key is used to attach the new path to a vanished document and **inherit** its `DocId`; the id is not re-derived from scratch.

Match keys, in order. The first key that yields exactly one unmatched candidate wins; two or more candidates is ambiguous and falls through to the next key. Never merge.

Match keys are namespaced by `corpus_id` (§6.11), so an archived document and a primary document with the same title and date can never collide. Whether `scope` also enters the key is an **open decision** (§15 Q18) with real churn consequences, so it is deliberately not decided here.

| Order | Key | Notes |
|---|---|---|
| 1 | declared `id` | strongest claim; aliases from `[frontmatter] id` (`id`, `uid`, `uuid`, `zettel_id`, …) |
| 2 | `content_hash` | byte-identical move — proof, not inference |
| 3 | title + date | date from frontmatter or filename; never mtime |
| 4 | title + `doc_version` | reached only when no date is available |
| 5 | title alone | only if unique; templated duplicate titles collapse and fall through |
| 6 | `rel_path` | last resort; a move with no other evidence is a new document |

Why `doc_version` is tier 4 and not tier 3:

- `doc_version` is *increased deliberately as the document evolves*. Using it as a primary identity key would break continuity exactly when the document changes, which is the opposite of what identity is for.
- It is still a useful corroborating key: many undated docs have a version, and `title + version` is specific enough to match a moved document.
- `title + date` outranks it because a date is authored once and rarely revised. Combining both would make either field's absence change the key, so the keys are ordered fallbacks rather than a composite.

What if everything is missing: no `id`, no date, no version, and the title is not unique (or is absent) → `rel_path`. An undated `Untitled.md` or a vault full of `Daily note` files resolves by path. That is fail-closed: those documents lose move continuity, but two unrelated templates are never merged into one identity. `doctor` reports `duplicate_identity` when a key is ambiguous.

Other properties:

- Title derivation (no write): frontmatter `title` → first H1 → filename stem.
- Date uses §6.3's hierarchy, but **mtime is excluded** here because clones, checkouts, and copies churn it. If mtime is the only date source, that key is unavailable.
- Existing ids from other systems (Obsidian/Zettelkasten timestamps, UUIDs, cuid2, basic-memory ids) are used verbatim at key 1 once their key name is in `[frontmatter] id`. Values are opaque and never rewritten.
- A user who wants maximum stability can add an `id`; janus-mem never does it on their behalf. `janus ids` shows the resolved ID, which key matched, and which document it inherited from.
- **Sidecar-loss caveat.** `.janus/` caches identity decisions. Rebuilding after deleting it re-derives IDs from current state, which is identical for unmoved documents but may differ for a document that moved while the sidecar was absent. Path-first link resolution keeps the impact bounded. This is the price of zero-write identity; it is not a hidden failure mode.

Consequence for moves (§6.9): an agent that renames `2024-11-02-postgres.md` and edits its body keeps both title and date, so key 3 preserves identity with no file changes and no confirmation. If the date was never there but `doc_version` was, key 4 does the same. If both changed, the document is treated as new and gets a continuation hint.

`path_history: BTreeMap<DocId, Vec<PathEntry>>` records every path a document has occupied (with content hashes, first/last seen, tombstones). It exists for link resolution and reporting, not for identity.

Chunk IDs are content-addressed and **do not include `doc_id`**:

```
chunk_id = blake3(heading_breadcrumb ‖ 0x00 ‖ normalized_chunk_text)
```

So unchanged chunks reuse their embeddings across edits, moves, renames, and copies — no move detection is required for cache efficiency. Identical text in two documents shares one embedding by construction, which is correct: same text, same vector.

### 6.5 Chunks

Chunking is structural, not fixed-window:

1. Split the body by ATX headings (`#`..`######`) using `pulldown-cmark` events, so headings inside fenced code blocks do not split.
2. Each section becomes a chunk. If a section exceeds `max_chars` (default 1800 ≈ 450 tokens), split at paragraph boundaries with ~15% overlap; never split inside a fenced code block, table, or list.
3. Drop chunks whose normalized text is empty.
4. Prepend a **breadcrumb** used *only* for embedding and lexical indexing, never for display:
   `title › H1 › H2` — this is the cheap, deterministic version of "contextual retrieval" and materially improves recall on short sections.
5. Store the byte span in the body so `read_document` can return exact source ranges without storing the text.

Chunk payload. Note that text is **not** persisted in the index (see §8.3); `text`/`embed_text` are transient values that exist only while indexing.

```rust
pub struct Chunk {
    pub id: ChunkId,
    pub doc_id: DocId,
    pub heading_path: Vec<String>,   // interned in the index's string pool
    pub char_span: (u32, u32),       // byte offsets into the doc body, not chars
    pub ordinal: u16,                // 0-based position within the doc; display order only
}
```

`ordinal` is purely a rendering hint: it lets `read_document` return a document's chunks in source order and gives stable display ordering in results. It is **not** identity — `chunk_id` is the content hash — and it must never be used to match chunks across edits.

### 6.6 Authority

Authority is a multiplier applied after relevance, derived from `type` and `status`:

| Type | Default weight | Rationale |
|---|---|---|
| `decision` | 1.50 | An explicit, committed choice |
| `spec` | 1.35 | Committed intent |
| `principle` | 1.20 | Durable, but general |
| `finding` | 1.10 | Evidence from research, measurement, or observation. Trustworthy, but not binding |
| `journal` | 1.00 | Baseline, personal, provisional |
| `note` | 0.90 | Unclassified |
| `idea` | 0.85 | A proposal or speculation. Lowest bindingness; often short-lived |
| unknown type | 0.80 | Fail-low, not fail-silent |

Status gate (before scoring):

| Status | `intent=current` | `intent=history` | `intent=evolution` |
|---|---|---|---|
| `active` | ✅ | ✅ | ✅ |
| `draft` | ✅ (0.7×) | ✅ | ❌ |
| `deprecated` | ❌ | ✅ | ✅ |
| `superseded` | ❌ | ✅ | ✅ |
| `unknown` | ✅ (0.8×) | ✅ | ✅ |

**`finding` and `idea` are first-class types, not journal variants.** They carry the distinction that matters for reasoning: *what we observed* (finding) versus *what we might do* (idea) versus *what we chose* (decision). That distinction only pays off because of supersession: the canonical chain is `idea → decision`, and `finding → finding` when new evidence arrives. Modeling them as generic notes would throw away exactly the transitions the engine exists to track.

Decay defaults: `decision`/`spec`/`principle` are `timeless: true` (no decay); `finding` decays slowly (365-day half-life — evidence ages, but slower than commentary); `journal`/`note` at 180; `idea` fast (90 days — an unexplored idea should not outrank current work). An explicit `timeless:` or per-type override in config always wins.

### 6.6.1 Types are an open registry, not an enum

`type` is a string resolved against a config-driven registry, so adding a domain type (`runbook`, `retro`, `customer-interview`) never requires a code change or a release:

```toml
[types]                     # alias → canonical type
adr = "decision"
rfc = "spec"
research = "finding"
proposal = "idea"

[authority]                 # canonical type → weight
finding = 1.10
idea = 0.85

[decay.by_type]             # overrides the global half-life
finding = 365
journal = 180
idea = 90

[paths]                     # optional path-pattern → type hints
"decisions/" = "decision"
"research/" = "finding"
"ideas/" = "idea"
```

The Rust side resolves each type once into a `TypeSpec { authority, timeless, half_life_days }` at config load and passes it to the scorer. Unknown types get weight 0.80, the global half-life, and a `doctor` note — never a hard failure. `type` never influences identity (§6.4).

### 6.7 Supersession

- **Declared, never inferred.** A link exists only when frontmatter says so (`supersedes` / `superseded_by`) or a write tool created it. janus-mem may *suggest* candidates (`janus suggest-links`) but v1 never applies them automatically.
- Links are **symmetric in effect**: `A.superseded_by = B` implies `B.supersedes = A` for traversal, even if only one side is declared. `doctor` reports one-sided links.
- Chains are followed transitively (`A → B → C`); cycles are detected, reported by `doctor`, and broken deterministically (link ignored, warning emitted) rather than panicking.
- Supersession is orthogonal to `status`: a doc can be `superseded` without a `superseded_by` (retired with no replacement) — this is the "quiet expiry" case that Graphiti-style systems get wrong. `lineage` still reports it as a terminal state.
- **Validity windows.** A document's `valid_to` is `min(effective_to, earliest successor's effective_from)`. The chain is a *derivation fallback*, never the only source: explicit `effective_from`/`effective_to` win (§6.10).
- **Cross-corpus links are qualified and opt-in.** `supersedes: "third-brain:/decisions/old.md"`; unqualified paths resolve only inside the document's own corpus, and qualified links require `allow_cross_corpus_links = true` (§6.11). An archived decision can never supersede a current one by accident.

### 6.8 Versions

Three distinct versions, never conflated:

| Version | Where | Meaning | Owned by |
|---|---|---|---|
| `schema_version` | frontmatter | Frontmatter schema revision | Human/agent |
| `doc_version` | frontmatter | Content revision of this document | Human/agent |
| index `format_version` | index header | Binary layout of the sidecar | janus-mem |

`janus-mem` bumps `doc_version` **only** when the user asks it to (`--bump-version`), and never invents a versioning scheme for docs that lack one.

### 6.9 Moves, renames, and reorganization

Agents and “dream cycles” move, split, and rewrite files. janus-mem does **not** try to infer identity from filesystem events. It makes identity independent of location (§6.4), then treats movement as bookkeeping.

On each index pass, diff the observed path set against `path_history`. For every vanished path and every appeared path:

1. **A match key resolves to exactly one vanished document** (declared id, content hash, title+date, title+version, or a unique title) → same document, new location; the old `DocId` is inherited. Applied automatically: the key came from the file itself, not from a similarity score. Costs zero embedding work.
2. **Exact `content_hash` match** against a vanished document → same document, new location. Applied automatically: bytes are identical, so there is nothing to guess. Embeddings are already reused because chunk IDs are content-addressed.
3. **Similar but not identical** (move + edit) → recorded as a **continuation hint**: `probable_continuation_of: DocId`, with evidence and confidence. This is an annotation shown by `doctor` and `janus moves`. It never reassigns identity, never mutates the link graph, never fires a prompt, and never writes to the vault.
4. **No match** → new document (and a tombstone for the vanished side).

Hint confidence is the weighted score:

```
0.50 · jaccard(chunk_ids) + 0.20 · title_match
+ 0.15 · basename_match + 0.15 · tag_jaccard
```

Anything `≥ 0.50` is displayed. Nothing is applied.

**Why this needs no confirmation.** Correctness never depends on case 3:

| What must survive a move | Carried by | Needs move detection? |
|---|---|---|
| Supersession chains | `DocId` matched/inherited from document metadata (§6.4) | No |
| Embedding cache | content-addressed `chunk_id` | No |
| Path-valued links (`supersedes: …`) | path alias table | No |
| “Where did this doc go?” reporting | continuation hints | Nice to have |

So there is nothing for a user or LLM to confirm, because no inference is load-bearing. The alternative — auto-merging structural matches — risks silently grafting one document's lineage onto another, which is the exact failure this project exists to prevent. The alternative of requiring confirmation assumes an actor that will never show up. Deriving identity from metadata the document already has removes the dilemma — and, unlike a declared-`id`-only design, it requires no edits to anyone's vault.

If an agent or user *does* want to assert that a rewritten document continues an older one, it calls `update_frontmatter` to set `id`. That is an explicit, etagged write — a claim, not a guess.

Other cases:

- **Path alias table.** Every path ever seen maps to the `DocId` that occupied it, including historical paths. Link resolution order: current path → historical path → declared `id` → dangling warning. A `supersedes: decisions/old-name.md` still resolves after `old-name.md` moves. Indexing never rewrites link text.
- **Zero migration.** There is no identity setup step and no `adopt-ids`. A brand-new vault, a vault from another tool, and a vault with no frontmatter all work identically on first `janus index`. `janus ids` explains any document's resolved identity; if a user *chooses* to pin one, `janus set --key id=<value>` is an ordinary surgical write.
- **Tombstones.** A vanished document with no successor keeps its id, last path, and last-seen date. Excluded from search; `lineage` still reports it as retired — the “quiet expiry” case (§3).
- **Copies.** Identical content at two paths is two documents. If they resolve to the same identity key, both fall through to the `rel_path` key and `doctor` reports `duplicate_identity`; the link graph deterministically prefers the oldest `path_history` entry.
- **Moves out of / into the vault** are tombstone / create. Re-entry with the same identity key resurrects the identity.
- **Identity churn.** If a rewrite changes both title and date (or deletes an `id`), the document resolves to a new `DocId`. `doctor` detects the probable predecessor via the tombstone's content hash and reports it as a continuation hint. It reports; it never silently restores.
- **Consistent snapshots.** The index hashes the bytes it actually read; if the file changes mid-read, it re-reads (bounded retries). It never trusts mtime alone.

### 6.10 Temporal semantics: valid time and transaction time

Two clocks, never conflated:

| Axis | Question | Where it lives |
|---|---|---|
| **Valid time** (world time, business time) | When was this true *in reality*? | Frontmatter `date`, `effective_from`, `effective_to`; supersession chain |
| **Transaction time** (system time, ingestion time) | When did *we* learn or record it? | Git history; never in frontmatter |

Four dates, and they routinely differ:

| Date | Source | Question |
|---|---|---|
| **Authored / decided** | `date` | When did we decide it? |
| **Effective** | `effective_from` | When did it start being true? |
| **Recorded** | git commit, index observation | When did it enter the vault / index? |
| **Observed-superseded** | index pass | When did we notice a successor? |

Frontmatter keys (all optional):

```yaml
date: 2025-01-14            # authored/decided (existing)
effective_from: 2025-04-01  # valid-time start; defaults to `date`
effective_to: 2026-03-01    # valid-time end, exclusive; usually derived
```

Rules:

1. `effective_from` defaults to `date`. **Never to mtime.**
2. `effective_to` is exclusive. If absent, it is derived as the earliest successor's `effective_from` (§6.7). The chain is the *fallback*; explicit keys win.
3. `valid = [effective_from, valid_to)` where `valid_to = min(effective_to, earliest successor's effective_from)`.
4. `valid_to <= effective_from` → `invalid_validity` (`doctor` error). The window is treated as empty; the document still appears in `history` and `exhaustive`.
5. `current(now) = effective_status ∈ {active, draft} AND effective_from <= now AND (valid_to is None OR valid_to > now)`, where `effective_status` is defined in rule 6.
6. **Effective status is derived, not hand-maintained.** If `superseded_by` resolves to an existing document, the document is superseded even when `status: active` is declared. If `status: superseded` has no successor, it is quiet-expired (still superseded). `doctor` reports disagreements as `status_mismatch` and never rewrites them.
7. **`as_of DATE` uses valid time only.** It includes documents where `effective_from <= T < valid_to`; decay is off; **status is returned but does not gate**. A document that is superseded today may have been current in June 2025 — gating on today's status would silently drop the correct answer.
8. Decay in the `current` intent is anchored on `effective_from`, not `date`: a decision that took effect yesterday is recent even if it was drafted a year ago.
9. Missing dates entirely → excluded from `as_of` (reported as `no_validity`), included in `current`.

**Why explicit keys, not chain-only derivation.** Deriving windows from the chain fails in four real cases: in-place rewrites (no successor, so no new window); effective ≠ decided (“decided in January, effective in April”); backdated imports (negative windows); and future branching, where one fact is superseded differently in different scopes. The chain still supplies the default — it just isn't the only source.

**Transaction time is git's job.** janus-mem does not store it, and it must not write system timestamps into frontmatter: that would mutate user files, pollute diffs, and be wrong after a sidecar rebuild. Git already records every version with a commit timestamp and survives index loss. The index may keep diagnostic timestamps (`first_seen_at`, `observed_superseded_at`) for `doctor` and freshness reporting, but they are derived, rebuildable, and never used in scoring or written to the vault. A `--git-history` enrichment (pure-Rust `gix`, no shelling out) can answer “what did we know at time T” after 1.0.

Scope-relative validity — a global fact superseded only for one org — depends on the scope decision and is deferred (§15 Q18).

### 6.11 Corpora: primary and archived vaults

A **corpus** is a root directory with its own index. Its `role` determines whether its facts may answer “what is true now”.

```toml
[[corpora]]
id = "main"
root = "."
role = "primary"

[[corpora]]
id = "third-brain"
root = "~/old-vault"
role = "archived"
read_only = true

[search]
allow_cross_corpus_links = false
archived_weight = 0.8
```

Rules:

- `corpus_id` is **declared and stable**, never derived from a path. Default `main`. (`vault_id`, a root-path hash, remains a manifest locator only — moving the repo must not change identity.)
- One index per corpus at `<root>/.janus/`. A combined index is unnecessary: scoring is per-document and independent, so merging per-corpus top-K by `final` is exact.
- **Archived corpora are hard-excluded by default**, not down-weighted. A multiplier still lets an old fact into a top-3 answer, which is exactly the failure being prevented.
- Query selects corpora: `--corpus primary` (default) | `all` | `<id>`. Every result carries `corpus`.
- When archived corpora are explicitly included, `search.archived_weight` applies (default 0.8; primary is 1.0) and appears in the score breakdown.
- **Cross-corpus links are opt-in and qualified.** `supersedes: "third-brain:/decisions/old.md"`. Unqualified paths resolve only within the document's own corpus. Qualified links take effect only when `allow_cross_corpus_links = true` (default false); otherwise `doctor` reports `cross_corpus_link_ignored`.
- **Promotion is an explicit write, never a background consequence:**
  - `janus promote third-brain:/path --link-from decisions/new.md` — patches the primary document's `supersedes` to include the qualified source (etagged, dry-run by default).
  - `janus promote third-brain:/path --adopt` — creates a new primary document citing the source. The archived document is untouched.
  - Promotion never moves, rewrites, or deletes archived documents.
- **Archived corpora are read-only** unless `read_only = false` is set deliberately; write tools refuse paths outside a writable corpus.
- Identity keys are namespaced by `corpus_id` (§6.4), so identical documents in different corpora cannot collide.
- Graph/entity edges carry `corpus_id` (§8.9): analysis spans corpora without conflating authority.
- The point of an archive is analysis, not truth: `--corpus third-brain --intent history` gives pattern analysis over old ideas and decisions with zero risk of them being served as current.

---

## 7. Behaviour

### 7.1 Query intents

| Intent | Decay | Status filter | Use |
|---|---|---|---|
| `current` (default) | on, 180-day half-life | active, draft, and valid at `now` | "What's true now?" |
| `history` | off | all statuses, including expired | "What did we ever say?" |
| `evolution` | off | all | Feed `lineage`; ordered oldest→newest |
| `exhaustive` | off | all, including `unknown` | Debug/audit |

`--as-of DATE` is a **parameter, not an intent**, and it overrides the intent's semantics: it switches to valid-time filtering (§6.10), turns decay off, and stops gating on status. `--as-of` always implies history ordering.

CLI flags can override parameters individually: `--decay-half-life 30`, `--include-superseded`, `--types decision,spec`, `--corpus all`.

### 7.2 Scoring

Computed per chunk, in this exact order:

```
semantic   = cosine(q_vec, c_vec)                        ∈ [-1, 1] → clamped to [0, 1]
lexical    = bm25_norm(q, c)                             ∈ [0, 1] via sigmoid squash
relevance  = α · semantic + (1 − α) · lexical            α = 0.65 (0.0 if no embedder)
authority  = AUTHORITY[type] · STATUS_FACTOR[status]
decay      = exp(−ln(2) · age_days / half_life_days)     = 1.0 if timeless or half_life ≤ 0
final      = relevance · authority · decay
```

- **Retrieval is candidate-scoped.** The scorer signature is effectively `score(query, candidates: Option<&CandidateSet>)`, where a candidate set is a bitset or sorted `ChunkId` list. v1 always passes `None` (all chunks). This exists so a future graph expansion, tag filter, or “only these docs” restriction reuses the exact same scoring path instead of growing a second one (§8.9).
- `age_days` uses `effective_from` (falling back to `date`) and the injected `Clock`, clamped at 0 (future-dated docs get decay = 1.0).
- `as_of` requests bypass the status gate and set `decay = 1.0`; inclusion is decided by the valid-time window alone. The response still reports declared and effective status so the agent can see what the document's standing was.
- When multiple corpora are explicitly queried, `final` is multiplied by `search.archived_weight` and the corpus name is reported per result. Primary-only queries apply no corpus weight.
- Vectors are L2-normalized at index time, so cosine is a dot product.
- A doc's score is the max over its chunks; the returned chunk is the argmax. (No document-level score aggregation in v1.)
- Optional MMR-style diversity pass (`--diversity 0.3`, default off) to avoid five chunks of the same doc.
- Ties break by `final`, then `date` desc, then `chunk_id` — fully deterministic.

**Hard requirement:** `score()` is a pure function with a `Clock` and config as inputs. Its tests are table-driven and cover: decay monotonicity (`age↑ ⇒ decay↓`), authority ordering, status gating, α endpoints, future dates, missing dates, and `timeless` override. Property tests with `proptest` assert `final ≥ 0` and `final ≤ 1.5 · 1.0 · 1.0`.

**Two independent layers, not alternatives.** `fastembed` provides the *semantic* vector (M4); BM25 provides the *lexical* signal (M3). Neither replaces the other:

- Vectors are fuzzy. Ask for `JANUS_WRITE`, `bincode`, `blake3`, an error code, a person's name, or “the exact sentence about time decay” and cosine similarity is mediocre; exact term matching is excellent.
- Lexical is brittle. Ask “how do I keep old decisions from confusing the agent?” and the words you use may share nothing with the words in the doc.
- Hybrid (α = 0.65) is the default because the failure modes are complementary. The lexical layer also keeps the tool fully useful with `--no-embeddings`, before the model is downloaded, and on machines where ONNX Runtime won't build.

**DECIDED (D20): hand-written BM25 for now.** Inverted index, Unicode-aware tokenizer, optional Porter stemmer — ~200 lines, no heavy dependency, and it teaches `HashMap`, tokenization, and score normalization in a way `tantivy`'s API does not. At M4, compare nDCG@10 against `tantivy` on the eval set; swap only if the hand-rolled version is >10% worse. `tantivy` remains the documented escape hatch, not the plan.

### 7.3 Write semantics

Writes operate on **text spans**, not on parsed YAML round-trips.

```
file bytes
 ├─ optional BOM
 ├─ "---\n"                        ← opening fence (LF or CRLF, detected)
 ├─ frontmatter YAML text          ← the only region we may rewrite
 ├─ "---\n"
 └─ body                           ← untouched, byte for byte
```

**What “fail closed” means.** janus-mem never runs the file through a YAML serializer. It splices the smallest possible byte range. If a requested patch cannot be expressed as a splice, it refuses and writes nothing. Reformatting a file the user didn't ask to reformat is treated as data loss.

Worked example — patch `{status: "superseded", superseded_by: "decisions/2026-10-29-sqlite.md"}`:

```yaml
# before                                   # after
---                                        ---
# owner: platform                          # owner: platform
title: "DB Choice"                       title: "DB Choice"
status: active   # current                 status: superseded   # current
tags:                                      tags:
  - db                                       - db
  - rust                                     - rust
owner: {name: me, team: infra}             owner: {name: me, team: infra}
                                           superseded_by: decisions/2026-10-29-sqlite.md
---                                        ---
<body, untouched>                          <body, untouched>
```

Two bytes-ranges change (`active` → `superseded`) plus one appended line. The comment stays, the block list stays, the flow mapping stays, key order stays, the body is copied verbatim. The same patch against `tags` (a block sequence) is refused with `UnsupportedPatch { key: "tags" }` rather than silently rewritten as `tags: [db, sqlite]`.

Algorithm for `update_frontmatter`:

1. Read bytes; compute `blake3` → `current_hash`.
2. **Require `expected_hash` for any existing file.** Absent → `Error::PreconditionRequired` plus a dry-run preview (an agent that can't prove it read the current version gets a preview, not a mutation). Present but ≠ `current_hash` → `Error::Conflict { expected, current }`. This is the entire race-condition defense: the hash the agent saw on `read_document`/`search` is the hash it must present on write.
3. Isolate the frontmatter region: detect BOM, opening `---`, newline style, closing `---`. Everything outside this region is recorded as an opaque byte suffix/prefix.
4. Parse the YAML region with a **span-preserving** parser (see gate below) into `(key → byte range of its value)` for top-level keys.
5. For each patch key:
   - Key exists, new value is a **scalar** (string/bool/int/float/null), a **flow sequence of scalars**, or `null` → splice exactly that value's byte range. Key order, comments, quoting of *other* keys, and indentation are untouched.
   - Key exists as a block sequence/mapping, multi-line scalar (`|`/`>`), alias/anchor, or the new value is a nested structure → **refuse** with `Error::UnsupportedPatch { key }` and suggest `write_document` (or an explicit `--reformat-key`, dry-run by default).
   - Key absent → append one well-formed line before the closing fence, using the file's detected newline and indentation. Insertion position is deterministic (before the fence, after the last key).
6. If no frontmatter exists → create one only when `create_frontmatter: true`; otherwise `Error::NoFrontmatter`.
7. Take the advisory lock, re-read the file and re-verify `current_hash` (the authoritative check, step 2 is only the fast path), write a temp file in the same directory, `fsync`, `rename`, `fsync` the parent directory. Preserve mode and (on Unix) ownership hints.
8. Return a receipt: `{path, changed_keys, old_hash, new_hash, dry_run, bytes_before, bytes_after}`.

Invariants (each backed by a test):

- I1: bytes outside the patched spans are identical.
- I2: newline style (CRLF/LF) and BOM presence are preserved.
- I3: a failed or refused patch leaves the file byte-identical to before.
- I4: the output still parses as valid frontmatter + body.
- I5: unknown keys, comments, ordering, anchors, and multi-line strings survive untouched.
- I6: concurrent modification (detected via the re-read hash under the lock) aborts the write with `Conflict` and leaves both versions intact.
- I7: a write without `expected_hash` on an existing file is impossible via every surface (CLI, MCP, JSON-RPC), not just discouraged by documentation.

DECISION GATE: span-preserving YAML. `serde_yaml` is unmaintained and lossy by design (comments, ordering, quoting). Candidate order: (a) a comment/span-preserving YAML crate if a maintained one exists at implementation time, (b) a narrow hand-written line-scanner that handles only `key: scalar` at column 0 plus flow sequences, refusing everything else, (c) fall back to `yaml-rust2`/`saphyr` events with manual span tracking. (b) is the safe default and is itself a great learning exercise; the spec does **not** mandate a YAML round-trip. Verify crate availability before M7.

### 7.4 `supersede_document(old, new, ..)`

A convenience wrapper over two `update_frontmatter` calls, executed as a **transaction**:

1. Dry-run both patches; validate both docs exist, are inside the vault, and that the resulting graph has no cycle.
2. Stage both temp files.
3. Rename both (old first, then new). If the second rename fails, roll back the first from its staged backup and return an error naming the inconsistent state.

4. Return both receipts plus the resolved canonical chain.

No identity keys are inserted. `supersede_document` writes only the keys the operation is about (`status`, `superseded_by`, `supersedes`); chain stability across later moves comes from derived identity (§6.4), not from modifying the documents.

`--in-place` (default) patches the old doc's `status` and `superseded_by`. `--append-only` instead creates a new file and leaves the old untouched. Both are supported; only `--in-place` is the default because that's what the vault owner asked for.

---

## 8. Architecture

### 8.1 Crate and module layout

Single binary crate, small library, no workspace in v1 (workspaces are a later refactor exercise, not a starting point):

```
janus-mem/
├── Cargo.toml
├── janus.toml                  # example config, shipped
├── SPEC.md
├── README.md
├── src/
│   ├── main.rs                 # clap wiring, tracing init, exit codes only
│   ├── lib.rs                  # pub mod re-exports for tests
│   ├── config.rs               # janus.toml, env, defaults, validation
│   ├── error.rs                # JanusError (thiserror) + exit-code mapping
│   ├── clock.rs                # Clock trait, SystemClock, FixedClock
│   ├── vault/
│   │   ├── mod.rs              # scan, walk, ignore rules, path confinement
│   │   ├── frontmatter.rs      # span-preserving read + patch
│   │   └── document.rs         # Document, metadata resolution hierarchy
│   ├── chunk.rs                # heading-aware chunker, breadcrumbs
│   ├── index/
│   │   ├── mod.rs              # Index facade, incremental reindex
│   │   ├── store.rs            # in-memory store + on-disk format
│   │   ├── lexical.rs          # BM25 inverted index
│   │   └── vector.rs           # flat f32 buffer, rayon dot products, top-k
│   ├── embed.rs                # Embedder trait, FastEmbed, HashEmbedder (test)
│   ├── score.rs                # weights, decay, status gate, intents
│   ├── query.rs                # search, lineage, authoritative_facts
│   ├── write.rs                # update_frontmatter, write_document, supersede
│   ├── mcp/
│   │   ├── mod.rs              # server bootstrap
│   │   └── tools.rs            # tool router, JSON schemas, DTOs
│   └── cli/                    # one file per subcommand
├── tests/
│   ├── fixtures/vault/         # ~40 realistic docs, git-tracked
│   ├── cli_*.rs                # assert_cmd
│   ├── frontmatter_preserve.rs # golden + proptest
│   └── eval/queries.jsonl      # hand-labelled retrieval eval
├── benches/
│   ├── score.rs
│   └── search.rs
└── docs/
    ├── notes/                  # learner journal, one file per milestone (§12)
    └── adr/                    # decisions about janus-mem itself
```

### 8.2 Crate stack

| Concern | Crate | Notes |
|---|---|---|
| CLI | `clap` 4 (derive) | Learning: derive macros, `Args`/`Subcommand` |
| Errors | `thiserror` (lib), `anyhow` (binary) | Learning: typed errors vs. context |
| Serialization | `serde`, `serde_json`, `toml` | Frontmatter YAML via gate in §7.3 |
| Markdown | `pulldown-cmark` | Event-stream parsing |
| Dates | `chrono` | `NaiveDate` for doc dates, `DateTime<Utc>` for age |
| Walking | `ignore` | Respects `.gitignore`; parallel walker |
| Hashing | `blake3` | Content hashes, chunk IDs |
| IDs | `ulid` | Sortable IDs for generated docs |
| Parallelism | `rayon` | Vector math, embedding batches |
| Logging | `tracing`, `tracing-subscriber` | stderr only; **never** stdout (would corrupt MCP) |
| Embeddings (opt-in) | `fastembed` | Feature `embeddings`; ONNX Runtime via `ort` |
| MCP | `rmcp` (DECISION GATE) | Official Rust MCP SDK; hand-rolled JSON-RPC fallback |
| Index access (M8) | `memmap2`, `bytemuck` | Zero-copy read of `vectors.f32` (§8.3) |
| Dev | `insta`, `proptest`, `criterion`, `tempfile`, `assert_cmd`, `predicates` | |

Dependency policy: every dependency needs a one-line justification in `Cargo.toml`. Prefer std. No dependency that requires a C toolchain unless behind an opt-in feature. `cargo deny` in CI for licenses/advisories (stretch).

### 8.3 Index format (`.janus/`)

**Vectors are always on disk.** Memory is a cache, never the source of truth. The index is split so that the two large, differently-accessed things — metadata and vectors — can be loaded independently.

```
.janus/
├── manifest.json     # human-inspectable: counts, formats, embedder id, offsets, checksums
├── index.bin         # bincode: string pool, docs, chunks (spans only), links, path history
├── vectors.f32       # raw little-endian f32, row-major, L2-normalized, dim known
└── lock              # advisory lock for index/write critical sections
```

```rust
pub struct Index {
    pub format_version: u16,           // bump on layout change ⇒ rebuild
    pub schema_version: u16,           // frontmatter schema this index understands
    pub embedder: Option<EmbedderId>,  // model, dim, normalization, quantization
    pub built_at: DateTime<Utc>,
    pub corpus_id: CorpusId,           // declared, stable, never path-derived
    pub corpus_role: CorpusRole,       // Primary | Archived
    pub vault_id: VaultId,             // hash of canonical root; locator only, never identity
    pub vault_root: PathBuf,
    pub strings: StringPool,           // interned rel_paths, titles, headings, tags
    pub docs: Vec<DocumentMeta>,       // ~120 B each, string-pool handles
    pub chunks: Vec<ChunkMeta>,        // id, doc_id, byte span, heading handles
    pub edges: Vec<Edge>,              // v1: only EdgeKind::Supersedes; generic (§8.9)
    pub path_history: BTreeMap<DocId, Vec<PathEntry>>,
    pub vectors: VectorSection,        // offset, len, dim, dtype into vectors.f32
}
```

Two rules make this cheap:

1. **No document text in the index.** A chunk stores `(doc_id, byte_span)`. Retrieval reads those bytes from the source file on demand (a small per-query file cache avoids repeated opens). At 200k chunks × ~1.5 KB, storing text would cost ~300 MB of heap for data that is already on disk.
2. **Vectors are one flat, mmap-able array.** `vectors.f32` is mapped read-only (`memmap2`); chunk `i` occupies `[i·dim .. (i+1)·dim]`. Those pages are OS page cache — evictable, shared, and not on the Rust heap. Only the pages a query actually touches are resident.

This also means the on-disk format does not change between M4 (simple `read` of the whole file) and M8 (mmap + optional quantization): only the load path changes.

- Rebuild triggers: `format_version`/`schema_version`/`embedder`/dim/dtype mismatch, chunking config change, or `--rebuild`.
- Incremental: skip a file when `content_hash` is unchanged; reuse chunks whose `chunk_id` still exists (across moves, too — §6.9); only embed the delta.
- Embedding is never required for correctness. With `--no-embeddings` (or if the model is absent and download is disallowed), janus-mem runs lexical-only, omits `vectors.f32`, and says so in every response (`degraded`).
- Corruption: blake3 checksums for both files live in `manifest.json`; mismatch ⇒ rebuild from source with a warning. A truncated or partial index is never loaded.
- Atomic index update: write `index.bin.new` / `vectors.f32.new`, fsync, rename both, then rewrite `manifest.json` last. Readers see the old manifest or the new one, never a mix.

Every stored path is vault-relative. The manifest carries two different identifiers, and they must not be confused:

- `corpus_id` — declared in config, stable, participates in identity (§6.4).
- `vault_id` — a hash of the canonical root, used only to locate/index a corpus. Moving the repository changes `vault_id` and must **not** change any `DocId`.

v1 loads one primary corpus per process, but nothing in the format assumes a single corpus: a registry is a list of corpus descriptors plus per-corpus indexes merged at query time (§6.11).

### 8.3.1 RAM budget

Reference vault: 20k docs, 200k chunks, 384 dimensions.

| Component | Size | Where it lives |
|---|---|---|
| Vectors, f32 | 200k × 384 × 4 ≈ **307 MB** | mmap'd page cache, evictable |
| Vectors, f16 (`--quantize f16`) | ≈ **154 MB** | same |
| Vectors, i8 (`--quantize i8`, M8 stretch) | ≈ **77 MB** | same |
| Chunk metadata | 200k × ~48 B ≈ **10 MB** | heap |
| Doc metadata | 20k × ~120 B ≈ **2.4 MB** | heap |
| String pool | ~**5 MB** | heap |
| BM25 postings | ~**40–80 MB** | heap |
| Document text | **0 B** (read on demand) | disk |
| **Steady-state heap** | **≈ 60–100 MB** | |

Techniques used (each is a concrete Rust learning target):

- Flat `Vec<f32>` / `&[f32]` with `chunks_exact(dim)`; never `Vec<Vec<f32>>` (per-row allocation, pointer chasing, poor cache locality).
- Interned strings plus `u32` handles instead of `String` in `DocumentMeta`/`ChunkMeta`; IDs are `[u8; 16]`, not hex `String`.
- `Box<[T]>` for structures that are immutable after build (no spare capacity, no accidental growth).
- Top-k without a full sort: a bounded buffer + `select_nth_unstable_by`, reused across queries.
- No per-query allocation: scratch buffers passed as `&mut`, or a thread-local arena for rayon workers.
- `#[repr(C)]` POD records + `bytemuck` for zero-copy reads over the mmap (no deserialize step at all).
- `madvise(SEQUENTIAL)` during full scans and `RANDOM` in the long-running server (M8, via `memmap2`'s advice API; `#[cfg(unix)]`, a no-op on Windows).
- Text fetched lazily with a bounded LRU keyed by `doc_id`, so a 10-result query opens at most 10 files.

### 8.4 Concurrency and process model

- **Sync core.** No Tokio in the core. Embedding runs on rayon; the MCP server (if `rmcp` forces async) isolates async at the transport boundary and calls the sync core via `spawn_blocking`.
- Index is immutable per process after load; writes invalidate and re-load the affected doc, not the whole index.
- `watch` mode: `notify` + debounce, single-threaded reindex queue, writes to a shadow index then swaps. Stretch goal (M8).
- The MCP adapter (`rmcp` is async/Tokio-based) holds `Arc<Engine>`; async exists only inside that adapter, and tool handlers call the sync core via `spawn_blocking`. The only mutable state is a `RwLock`-guarded index handle plus a write mutex.

### 8.5 Safety and privacy

- No telemetry, no network at query time, no embedding of vault content leaves the machine (models run locally).
- Path confinement: every user-supplied path is joined to the vault root, canonicalized, and checked with `starts_with(root)`; reject symlinks that escape the root; refuse to touch `.git/`, `.janus/`, and (by default) dot-directories.
- Response sizes are capped (`max_chars`, default 4000; `max_results`, default 10) to protect the agent's context window.
- Secrets: janus-mem never logs document content at `info`; `debug` requires `--log-content` explicitly.

### 8.6 Git coexistence

The vault is assumed to be a git repository (or a subdirectory of one). janus-mem must be a well-behaved co-tenant:

1. **Never touch `.git/`.** It is excluded from walking, from watching, and from every write path, unconditionally — it cannot be re-included by config.
2. **Never stage, commit, checkout, stash, or run hooks.** janus-mem has no git dependency and shells out to nothing. Git remains the user's tool.
3. **Everything janus-mem owns is ignorable.** The index lives in `.janus/`; `janus init` offers to add one line to `.gitignore` (shown as a diff, applied only on confirmation). If the user commits `.janus/` anyway, nothing breaks — it is still a cache.
4. **Respect `.gitignore` semantics** when walking (`ignore` crate), so build output, vendored trees, and private notes are not indexed by accident.
5. **Minimal, reviewable diffs.** Writes splice bytes (§7.3), preserve CRLF/LF and BOM, and never reorder keys — so a frontmatter patch shows as one changed line in `git diff`, not a whitespace storm. Golden tests assert exactly this.
6. **No lockouts.** Advisory locks are per-target-file and held for milliseconds around a rename; they must never block a `git checkout` or `git status`. If a lock cannot be acquired quickly, fail with a clear error rather than waiting indefinitely.
7. **Concurrent git operations are normal.** A `git checkout`/`git pull` can rewrite hundreds of files mid-scan. Reads hash the bytes they actually read and retry on change (§6.9); the index is written atomically via renamed temp files (§8.3), so an interrupted scan leaves the old index intact.
8. **Worktrees, submodules, and vault-as-subdirectory** all work because paths are vault-relative and confined, and no assumption is made that the vault root is a repo root.
9. **No `.gitattributes` or line-ending rewriting**, ever. If the user's repo normalizes EOLs, that is git's decision; janus-mem just preserves what it read.

### 8.7 Platform support

| Platform | v1.0 | Notes |
|---|---|---|
| macOS (aarch64, x86_64) | **Required** | Primary development target |
| Linux (x86_64, aarch64) | **Required** | glibc baseline; musl static build is a stretch goal |
| Windows (x86_64) | Optional, must stay *possible* | No Windows-only code, but no CI gate in v1 |

Cross-platform rules (these are cheap now and expensive to retrofit):

- No `std::os::unix` / `std::os::windows` without `#[cfg]`; no assumptions about `/`, `~`, drive letters, or case sensitivity. All path handling through `Path`/`PathBuf`; vault-relative paths serialized with `/` and converted on the way in.
- Atomic replace differs: on Unix `rename` over an existing file is atomic; on Windows it fails if the destination is open, so use `tempfile`'s `persist` (which handles `ReplaceFile` semantics) or a `#[cfg(windows)]` retry loop.
- File locking via a cross-platform crate (`fs2` or `fd-lock`), not `flock` directly. Never rely on locks being mandatory — they are advisory on both platforms.
- Preserve CRLF vs LF (§7.3) — Windows checkouts with `core.autocrlf` are a realistic vault.
- `madvise` and Unix permission bits are `#[cfg(unix)]`; on Windows, preserve attributes where cheap and otherwise leave the file alone.
- CI matrix: macOS + Linux from M0. Adding Windows is a one-line matrix change when we want it.
- The `.janus/` directory name is dot-prefixed (valid on Windows, just hidden differently) — no shell-scripting assumptions in Rust code.

### 8.8 Distribution and installation

Primary: **`cargo-dist`** (axodotdev) generating GitHub Releases on tag:

```sh
# shell installer (macOS / Linux)
curl -LsSf https://github.com/genuai/janus-mem/releases/latest/download/janus-installer.sh | sh

# or, with a Rust toolchain present
cargo binstall janus-mem     # prebuilt binary, no compile
cargo install janus-mem --locked   # compile from source
```

Also produced per release: `janus-aarch64-apple-darwin.tar.xz`, `janus-x86_64-unknown-linux-gnu.tar.xz`, `sha256.sum`, and a Homebrew tap formula. Design constraints that make this trivial:

- Single binary, no runtime deps, no config required to start (`janus init` writes one).
- The embedding model is **not** bundled (it would add ~100 MB); it is downloaded on first `index` with an explicit prompt, cached in a platform config dir, or supplied offline via `JANUS_MODEL_DIR`. `--no-embeddings` never needs a network.
- The installer script is small, pinned to a release tag, verifies checksums, and supports `--no-modify-path`. We publish it, but we also document `cargo binstall` as the preferred path for Rust users, because piping curl to bash is a trust decision the user should make consciously.

### 8.9 Extension contract: the future entity graph

v1 builds no entity graph, but the data model is deliberately shaped so one can be added without a rewrite. These invariants are the contract:

| Invariant (already true) | Why it enables a graph |
|---|---|
| `chunk_id` is a content hash, not `doc_id`-scoped | An entity mention anchors to a chunk and survives moves and edits; re-extraction is scoped to changed chunks |
| Every chunk stores a byte span | Every mention can cite exact source text — no unattributed “the graph says so” |
| `DocId` is stable across moves, with `path_history` | Extracted facts inherit the document's date, status, and lineage, and survive renames |
| The index is derived and versioned (`format_version`) | Entity/edge tables are a format bump + rebuild, not a migration of user data |
| Edges are a generic list with a `kind` | Supersession is just one kind; `Mentions`, `RelatesTo`, `PartOf` are additive |
| Retrieval can take a candidate set (§7.2) | Graph expansion yields chunk/doc IDs that feed the *same* scorer, preserving authority, decay, and status gates |
| Derived data is explicitly tagged | Machine-extracted edges can never masquerade as user-declared facts |

Sketch of the future shape:

```rust
pub enum EdgeKind { Supersedes, Mentions, RelatesTo { predicate: String } }
pub enum EdgeSource {
    Declared,                                              // frontmatter
    Derived { extractor: String, version: String, confidence: f32 },
}
pub struct Edge {
    pub kind: EdgeKind,
    pub from: NodeId, pub to: NodeId,                     // Doc | Chunk | Entity
    pub source: EdgeSource,
    pub valid_from: Option<NaiveDate>, pub valid_to: Option<NaiveDate>,
    pub doc_id: DocId, pub chunk_id: Option<ChunkId>, pub span: Option<(u32, u32)>,
}
```

A future `graph_query` tool would traverse `entity → mentions → docs/chunks → existing scorer`, returning answers with the same provenance fields plus the edge trail. Scoring, authority, decay, and status gates do not change.

Honest constraints, stated up front:

- **Extraction needs a model.** Entity/relation extraction requires an LLM or NER pass. That is index-time only, opt-in (`--features graph`), versioned in the manifest, and invalidates only derived edges when the extractor changes. Retrieval stays deterministic.
- **Entity resolution is its own hard problem.** Aliases, pronouns, and the same name for two things. Expect a declared-entity convention in frontmatter before any auto-resolution.
- **Declared beats derived.** When a user-written `supersedes` and an inferred relation disagree, the declared one wins, and both are visible.
- **Confidence is not optional.** Every derived edge carries extractor identity, version, and confidence; agents see the difference between “the doc says” and “a model inferred”.

What we deliberately do **not** do now: no empty graph tables, no entity enum, no extraction pipeline, no traversal API. Only the cheap forward-compatible choices above — generic edge kind, stable node IDs, candidate-set retrieval — which cost almost nothing and prevent a rewrite later.

---

## 9. Interfaces

### 9.1 CLI

```
janus init [--vault PATH]                    # create janus.toml + .janus/, add to .gitignore
janus index [PATHS...] [--rebuild] [--no-embeddings] [--dry-run]
janus search <QUERY> [--intent current|history|evolution|exhaustive]
                     [--as-of DATE] [--corpus primary|all|<id>]
                     [--limit N] [--types T,..] [--tags T,..]
                     [--decay-half-life DAYS] [--include-superseded]
                     [--json] [--no-color]
janus lineage <PATH|--topic QUERY> [--json]
janus facts [--tags T,..] [--query Q] [--json]         # authoritative_facts
janus read <PATH> [--json]
janus set <PATH> --expect-hash H --key status=superseded [--key k=v ...] [--dry-run]
janus write <PATH> --frontmatter f.yaml (--body -|f.md)
            [--create | --overwrite --expect-hash H] [--dry-run]
janus supersede <OLD> <NEW> [--append-only] [--dry-run]
janus promote <corpus>:<path> (--link-from <path> | --adopt) [--dry-run]
janus corpora [--json]                       # configured corpora, roles, index freshness
janus doctor [--json]                        # index health, broken links, duplicates
janus ids [PATH...] [--json]                 # resolved DocId, tier, and key for inspection
janus suggest-links [--threshold 0.85]       # candidates only; never applies
janus moves [--since <date>] [--json]        # continuation hints + confirmed moves; reports only
janus mcp [--allow-write] [--vault PATH]
janus rpc [--allow-write] [--vault PATH]     # line-delimited JSON on stdin/stdout; language-agnostic
janus completions <shell>
```

Exit codes: `0` ok, `1` runtime error, `2` usage error, `3` conflict/stale hash, `4` refused (unsafe or unsupported operation). Human output is a compact table; `--json` is stable and versioned.

### 9.2 MCP read tools

| Tool | Params | Returns |
|---|---|---|
| `search` | `query`, `limit?`, `intent?`, `as_of?`, `corpus?`, `types?`, `tags?`, `include_superseded?`, `decay_half_life_days?`, `max_chars?` | Ranked results with score breakdown, age, declared + effective status, validity window, corpus, path |
| `get_lineage` | `path?` **or** `topic?` (exactly one) | Ordered chain with relation edges and terminal state |
| `get_authoritative_facts` | `query?`, `tags?`, `types?`, `limit?` | Active decisions/specs/principles only |
| `read_document` | `path`, `max_chars?`, `chunk_id?` | Frontmatter as JSON + body (or a specific chunk's span) |

Plus `janus_status` (read-only, no params): index freshness, doc/chunk counts, embedder, vault root, whether writes are enabled. Cheap, and it stops agents from guessing.

### 9.3 MCP write tools (only with `--allow-write`)

| Tool | Params | Semantics |
|---|---|---|
| `update_frontmatter` | `path`, `patch`, **`expected_hash`**, `dry_run?` | Surgical key patch; refuses unsupported shapes |
| `write_document` | `path`, `frontmatter`, `body`, `mode` (create/overwrite), `expected_hash` (required for overwrite), `dry_run?` | Whole-file create/replace |
| `supersede_document` | `old_path`, **`expected_hash`**, `new_path?`, `new_title?`, `dry_run?` | Transactional dual patch (or append-only with `new_path` created first) |
| `promote_document` | `source` (`corpus:path`), `target_path?`, `mode` (`link`/`adopt`), `expected_hash?`, `dry_run?` | Declares an archived document as lineage for a primary one; never moves or rewrites the archive (§6.11) |

`expected_hash` is the `content_hash`/etag returned by every read tool. It is **required** for `update_frontmatter`, `supersede_document`, and for `write_document` in `overwrite` mode. Without it, the call returns `PreconditionRequired` and a dry-run preview; it never mutates. All write tools return receipts, and a hash mismatch returns `Conflict` with both hashes so the agent can re-read and retry.

### 9.5 Controller-agnostic surfaces

The engine must not live in the MCP layer. Everything is a thin adapter over one sync core:

```
                         ┌──────────────────────────┐
                         │  core::Engine (sync)     │  no printing, no protocol,
                         │  index/search/lineage/   │  no process state; takes
                         │  facts/read/write        │  &mut Request → Result<Response>
                         └────────────┬─────────────┘
              ┌───────────────┬───────┴────────┬──────────────────┐
           CLI adapter    MCP adapter     JSON-RPC adapter    (future) PyO3/
           (clap)         (rmcp/stdio)    (`janus rpc`, stdio)  Python wrapper
```

Rules:

- Adapters only parse input, call the engine, and format output. If an adapter contains a decision the other adapters need, it belongs in the core.
- One DTO module defines request/response/error types; CLI `--json`, MCP tool output, and `janus rpc` share them and the same stable error codes (`conflict`, `precondition_required`, `refused`, `not_found`, …).
- `janus rpc` is line-delimited JSON: one request object per line in, one response object per line out, same shapes as the MCP tools. This is deliberately the simplest possible interop contract: any language can drive janus-mem with `subprocess` + `json` — no FFI, no bindings to maintain. A Python wrapper (or a Hermes tool, or an Emacs client) is then ~50 lines.
- `status` is always available and returns `api_version`, index freshness, and capability flags (`writes_enabled`, `embeddings`), so a controller can feature-detect instead of assuming.
- Exit codes and error codes are part of the public API and are versioned; changing one is a breaking change.

### 9.4 Response envelope

Every tool returns the same outer shape, so agents learn one contract:

```json
{
  "ok": true,
  "intent": "current",
  "took_ms": 3,
  "degraded": ["embeddings disabled; lexical only"],
  "notes": ["4 superseded results hidden; use intent=history to see them"],
  "results": [ { "path": "...", "title": "...", "status": "active",
                 "effective_status": "active",
                 "type": "decision", "date": "2026-03-15", "age_days": 228,
                 "effective_from": "2026-03-15", "valid_to": null,
                 "corpus": "main",
                 "author": "claude-opus",
                 "date_source": "frontmatter", "doc_version": "2.0",
                 "scores": { "semantic": 0.81, "lexical": 12.4, "relevance": 0.78,
                             "authority": 1.5, "decay": 0.93, "corpus": 1.0,
                             "final": 1.09 },
                 "chunk_id": "…", "heading": "## Decision", "text": "…",
                 "truncated": false } ]
}
```

Errors are `{ "ok": false, "code": "conflict", "message": "...", "hint": "..." }` — never a bare panic or a stack trace. The `hint` field is for the agent, not the human.

---

## 10. Testing and quality

| Layer | Tool | What it must prove |
|---|---|---|
| Unit | `cargo test` | Scoring math, parsers, chunker, status gates, type-registry resolution (alias → canonical → TypeSpec, unknown types) |
| Golden | `insta` | Frontmatter patches: input → output byte-for-byte |
| Property | `proptest` | Patch invariants I1–I7; decay monotonicity; chunker never panics on arbitrary UTF-8, CRLF, BOM, unclosed fences, and `---` inside bodies |
| Integration | `assert_cmd` + fixture vault | `index → search → lineage → supersede → search` end to end |
| Concurrency | `tempfile` + threads | Concurrent `set` on one file: exactly one wins, the loser gets `Conflict` |
| Temporal | unit + integration | `as_of` returns what was valid at T; a doc superseded today is still returned for a past T and is absent from `current`; `valid_to <= effective_from` is reported as `invalid_validity`; `effective_from` defaults to `date` and never to mtime; the chain is used only when explicit keys are absent; decay is anchored on `effective_from` |
| Corpora | integration | Archived documents never appear in a default query; they appear with `--corpus all`/`<id>`; an unqualified cross-corpus link is ignored unless `allow_cross_corpus_links`; promotion never mutates an archived file; every result carries `corpus` |
| Identity/moves | integration + property | Derivation is deterministic for new docs; identity is carried forward for in-place edits (title/date/version changes do not churn); title+date **and** title+version both survive move+edit with **zero file writes**; ambiguous keys fall through to path; an id-less, frontmatter-less vault indexes correctly; stale textual links resolve through the alias table; no confirmation step is ever required |
| Platform | CI matrix (macOS, Linux) + `#[cfg]` tests | Path handling with spaces/unicode/case-only renames, CRLF preservation, atomic replace, advisory locks. Windows must at least compile (`cargo check --target x86_64-pc-windows-msvc`) even though it is not gated |
| Retrieval quality | `tests/eval/queries.jsonl` | ≥30 hand-labelled queries; report nDCG@10 and "active top-1" rate; run in CI as a non-blocking report |
| Performance | `criterion` | Search p50/p95, index throughput, memory (via `--profile`) |
| Lint | `cargo fmt --check`, `cargo clippy -- -D warnings` | Non-negotiable from M0 |
| Docs | `cargo doc` with `#![warn(missing_docs)]` on public items | Forces the learner to articulate APIs |

Rules:

- Every bug fix lands with a failing test first (this is also the best way to learn ownership: reproduce the borrow error in isolation).
- No `unwrap()`/`expect()` in library code except where an invariant is proven and the message is a real explanation. `clippy::unwrap_used` is enabled as a warn in CI.
- Tests never touch the network and never rely on wall-clock time (`FixedClock`).
- Fixture vault is committed and deliberately ugly: mixed CRLF/LF, a BOM, a doc with no frontmatter, a doc with comments in YAML, a cycle, a broken link, a 2 MB doc, and a non-UTF-8 file that must be skipped with a warning.

---

## 11. Milestones

Each milestone is a **vertical slice**: it ends with something runnable, tested, and explainable. Sizes are estimates, not commitments.

### M0 — Skeleton (½–1 day)
`cargo init`, edition 2024, `rust-toolchain.toml`, CI (fmt + clippy + test), `JanusError` + exit codes, `--version`, `--vault`, tracing to stderr, `janus init`.
**Done when:** `cargo clippy -- -D warnings` passes in CI and `janus init` creates a valid config.
**Learn:** cargo, modules, `Result`, `?`, `From`, clap derive, `thiserror`.

### M1 — Scan and parse (2–3 days)
Recursive walk with `ignore`, path confinement, frontmatter isolation (fences, BOM, CRLF), metadata resolution hierarchy, `janus scan --json` (no index yet).
**Done when:** the fixture vault parses into the expected `Document` list, including all the ugly cases, and every doc reports where its date came from.
**Learn:** `PathBuf`/`OsStr`, `&str` vs `String`, lifetimes in parsing functions, iterators, `Option` combinators, byte-index slicing and UTF-8 boundary safety.

### M2 — Chunk and preserve (2–3 days)
Heading-aware chunker with breadcrumbs and fence/table awareness; byte-span tracking; a `--dump-chunks` command. Start the golden frontmatter tests here (they will guide M7).
**Done when:** chunking is idempotent, no chunk exceeds the limit, and no fence is ever split.
**Learn:** `pulldown-cmark` event lifetimes, `Vec<T>` vs `&[T]`, `Cow<str>`, borrow-checker fights with a streaming parser (the main teaching moment of this milestone).

### M3 — Lexical search, daily usable (3–4 days)
Hand-written BM25 (inverted index, tokenizer, optional stemmer); scoring module with authority, status gates, and intents; `janus search` and `janus facts` with human + JSON output.
**Done when:** the author uses `janus search` instead of `rg` for a day, and the eval set's "active top-1" rate is ≥80% (lexical only).
**Learn:** `HashMap`/`BTreeMap`, `Eq`/`Hash` derives, sorting with `sort_by`, `&mut self` accumulation, iterator chains, float comparison pitfalls, `#[derive]` and newtypes (`DocId`, `ChunkId` — no stringly typed IDs).

### M4 — Embeddings and hybrid index (4–6 days)
`Embedder` trait (`fn embed(&self, batch: &[&str]) -> Result<Vec<Vec<f32>>>`), `HashEmbedder` for tests, `FastEmbed` behind a feature; flat vector storage written to a **separate `vectors.f32` file from day one** (so M8's mmap is a load-path change, not a format change); rayon top-k; `Index` metadata via `bincode`; incremental reindex; exact-hash move detection (`path_history`); `janus index`.
**Done when:** second index run touches zero embeddings for unchanged docs; a moved-but-unchanged file is recognized as a move and costs zero embedding work; queries are <10 ms warm at 100k chunks; `--no-embeddings` degrades cleanly.
**Learn:** traits, trait objects vs. generics, `Send`/`Sync`, rayon, slices and `chunks_exact`, serde/bincode, feature flags, `Option<&T>` in structs, `Arc`.

### M5 — Temporal graph (3–4 days)
Supersession parsing, transitive chains, cycle detection, derived validity windows, `--as-of` valid-time queries (§6.10), effective-status derivation, `janus lineage` (chain + terminal state), `janus doctor` link checks, `suggest-links` (similarity candidates, no writes), `janus ids`, and §6.9 move handling: derived-identity and exact-hash moves, path alias table, continuation hints, tombstones, `janus moves`.
**Done when:** `lineage` renders a correct 5-link chain from the fixture vault, `search --as-of` returns the old value for a past date and the new one today, detects the seeded cycle, and survives a simulated “dream cycle” that (a) moves a linked doc unchanged, (b) moves and edits a linked doc with no `id` anywhere, and (c) rewrites a third body — all without breaking any chain, touching any file, or requiring any confirmation step.
**Learn:** recursive functions and explicit stacks (avoid gratuitous recursion), `HashMap` graph representation, `enum` state machines, borrowing across a traversal, `impl Display` for output types.

### M6 — MCP server, read-only (3–5 days)
`rmcp` (or hand-rolled JSON-RPC) over stdio: `search`, `get_lineage`, `get_authoritative_facts`, `read_document`, `janus_status`. Same core also exposed as `janus rpc` (line-delimited JSON) so a future Python wrapper or any other controller needs no bindings. Register in Claude Desktop and Hermes. Stdout is protocol-only; all logs to stderr.
**Done when:** an agent answers W1–W3 using janus-mem with `grep` disabled, and `--allow-write` absence provably blocks writes.
**Learn:** JSON-RPC framing, `serde` DTOs, `Arc`/`RwLock`, error → protocol-error mapping, `#[serde(rename_all)]`, `serde_json::Value` for schemas, async only at the transport edge.

### M7 — Safe writes (5–7 days)
Span-preserving frontmatter patcher (§7.3), atomic write + locks, optimistic concurrency, `set`, `write`, `supersede`, dry-run receipts, `--allow-write` gate on MCP. This is the highest-risk milestone; do it after the read path is stable and the golden tests exist.
**Done when:** invariants I1–I7 are green, including the CRLF/BOM/comment fixtures, the mandatory-etag test, and the concurrent-write test (exactly one winner, loser gets `Conflict`), and a 200-file dry-run diff against the real vault is empty except for intended keys.
**Learn:** byte manipulation, `&[u8]` vs `&str`, `fs` + `File` + `OpenOptions`, `rename` atomicity, file locking (`fs2`/`fd-lock`, cross-platform advisory locks), `Drop`-based cleanup guards, `?` with custom errors, testing filesystem failure with permissions.

### M8 — Polish and scale (ongoing)
`watch`, mmap + `madvise` load path, optional `--quantize f16`/`i8` (§8.3.1), `doctor` in CI, eval harness + nDCG report, benchmarks, `cargo dist` release binaries, README with a 60-second quickstart, example `janus.toml`, MCP client config snippets for Claude Desktop / Hermes, and a "how to write docs the engine understands" guide.
**Done when:** a stranger can install and get a useful first search in under five minutes.

---

## 12. The learning plan

### 12.1 What this project teaches, and where

| Rust concept | Introduced | Practised |
|---|---|---|
| Ownership, moves, borrows | M0–M1 | Everywhere after |
| `Option`/`Result` and `?` | M0 | `error.rs`, all IO |
| Lifetimes (elided and explicit) | M1–M2 | parser, chunker borrowing `&str` |
| Structs, `impl`, newtypes | M1 | `DocId`, `ChunkId`, `Config` |
| Enums with data + `match` | M3 | `Status`, `QueryIntent`, `JanusError` |
| Iterators, closures, combinators | M1–M3 | scoring, filtering, top-k |
| Traits and trait objects | M4 | `Embedder`, `Clock`, `Store` |
| Generics (mild) | M4 | `top_k<T: Ord>`-style helpers |
| Concurrency without the GIL | M4 | rayon over 200k dot products |
| Serde in anger | M1, M4, M6 | frontmatter, index, MCP DTOs |
| Cargo features | M4 | `embeddings` |
| `Arc`, `RwLock`, interior mutability | M6 | MCP server state |
| Async (only as needed) | M6 | transport boundary |
| Byte-level IO and atomicity | M7 | writer |
| Memory-mapped IO, zero-copy, endianness | M8 | `vectors.f32`, `bytemuck` |
| Protocol design and adapter seams | M6 | core + CLI/MCP/JSON-RPC |
| Testing discipline | M0 onward | unit, golden, property, integration |

### 12.2 What this project deliberately does not teach

Say this out loud, so scope creep doesn't pretend otherwise: no `unsafe`, no custom `macro_rules!` or proc macros, no self-referential structs, no arena allocators, no `Pin`, no custom async runtime, no WebAssembly, no FFI. Those are all valuable; they are not this project.

### 12.3 AI collaboration protocol

The failure mode for an experienced Python dev using AI on Rust is **selling the compiler pass without buying the understanding**. Rules:

1. **Pair-program slowly, explanation-first.** AI may write code alongside the learner, one function at a time, but every line must be explainable before it is committed. The rule is comprehension, not authorship: if the learner cannot explain a line, it gets rewritten — not merged and forgotten. Start each function with two questions: *what does this own, and what does it borrow?*
2. **The compiler error is the curriculum.** Paste the *full* `cargo check` error and ask "explain what the borrow checker objects to and give me three options, without giving me the code." Then choose.
3. **Design before code.** Every milestone starts with a 20-minute "explain the data structures to me" conversation. Write the conclusion in `docs/notes/NN-<milestone>.md` (~10 lines: what I built, what confused me, what I now understand, what I'd do differently).
4. **Test-first for anything with a rule.** Scoring, frontmatter patching, chunking. Tests are also the cheapest way to ask AI for a *specification* instead of an implementation.
5. **No code you can't defend.** If asked "why `&mut self` here?", the answer must exist. Otherwise delete it and rewrite.
6. **Weekly refactor pass with AI as reviewer, not author:** "here is my `score.rs`, what would an experienced Rustacean change, and why?"
7. **Never let AI 'fix' a lifetime by cloning or sprinkling `Arc` without an explanation of the cost.** Add `clippy::clone_on_ref_ptr`-style discipline.

### 12.4 Weekly rhythm

| Day | Activity |
|---|---|
| 1 | Milestone design conversation; write the structs; `cargo check` until it compiles |
| 2–3 | Implement the happy path with tests; fight the borrow checker on purpose |
| 4 | Edge cases from the fixture vault; error paths; `clippy` clean |
| 5 | AI review of the diff; refactor; write `docs/notes/NN.md`; commit with a real message |

Cadence assumption: 5–10 focused hours per week ⇒ v1.0 in roughly 3–4 months. M3 (daily-usable) lands in ~2 weeks, which is the motivation checkpoint.

---

## 13. Performance targets

Measured on an M-series MacBook, release build, vault of 20k docs / ~200k chunks.

| Metric | Target | Notes |
|---|---|---|
| Cold index (embeddings on) | ≤ 60 s | dominated by ONNX inference |
| Cold index (lexical only) | ≤ 5 s | |
| Incremental reindex (1 changed doc) | ≤ 150 ms | no embedding of unchanged chunks |
| Warm query, index loaded | p50 ≤ 100 ms, p95 ≤ 250 ms | **Aspirational** p50 ≤ 10 ms; do not optimize before M6, and never at the cost of correctness |
| Cold query (index load included) | p95 ≤ 500 ms | mmap or fast bincode load |
| Steady-state RSS (heap) | ≤ 150 MB | vectors are mmap'd page cache, not heap; text is read on demand (§8.3.1) |
| Peak RSS during a full scan | ≤ 1 GB | page cache + embedding batch buffers |
| Binary size (release, stripped) | ≤ 30 MB | or ≤ 15 MB without `embeddings` |
| Startup to first output | ≤ 50 ms | CLI muscle-memory threshold |

**Performance policy.** These are guardrails against accidental O(n²) behavior, not product goals. The v1 bar is “feels instant on a laptop”: anything under 100 ms is indistinguishable in practice for an agent turn or a CLI query. Optimization work is deferred until the eval harness exists (M8) and only happens where a measurement says it matters. Single-digit milliseconds remain the design aspiration — the architecture (flat vectors, rayon, no per-query allocation) is chosen to make them attainable — but no feature gets cut or complicated for them.

If the targets conflict with the learning goal, the learning goal wins and the target gets revised in §15.

---

## 14. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| ONNX/`fastembed` build or first-run download friction on macOS | Med | Med | Feature-gate embeddings; lexical path must be fully useful alone; document `JANUS_MODEL_DIR` for offline model placement |
| Span-preserving YAML patching is harder than it looks | High | High | Fail-closed narrow scanner (§7.3 gate); golden + proptest corpus; refuse rather than reformat |
| The vault's frontmatter is inconsistent, so inference is wrong | High | Med | Surface `date_source` and `doctor` warnings; explicit overrides in `janus.toml`; never guess destructively |
| Scope creep toward "agent memory platform" | High | High | §2.2 non-goals; four read tools, three write tools; anything else goes to a `FUTURE.md` |
| Learning stalls on ownership during M2 | Med | Med | M2 is deliberately a parser (borrow-heavy); pair with AI explanation, allow an owned `String` version first, then refactor to borrows as an exercise |
| MCP SDK churn (`rmcp`) | Med | Med | Keep the protocol surface tiny; 5 tools are hand-rollable in ~300 lines if needed |
| Hand-rolled BM25 quality is poor | Med | Med | Eval harness at M3; decision gate at M4 to swap in `tantivy` |
| Loss of motivation after the novelty fades | Med | High | M3 delivers daily use in ~2 weeks; dogfood in the real vault from M3 onward |
| Index bloat / stale index confusion | Low | Med | `janus_status` always reports freshness; auto-rebuild on version mismatch; `doctor` checks index mtime vs vault mtime |
| Archived/old facts leak into current answers | Med | High | Hard corpus filter (not a weight) for archived roles; every result is labeled; cross-corpus links are qualified and off by default (§6.11) |
| `status` and validity windows disagree | High | Med | Derive `effective_status`; report `status_mismatch` and `invalid_validity` in `doctor`; never rewrite declarations |

---

## 15. Decision log

| # | Decision | Rationale | Reversible? |
|---|---|---|---|
| D1 | Name: `janus-mem`, binary `janus` | "Janus" = dual temporal perspective; `-mem` places it in the AI memory namespace; crate/binary split keeps typing pleasant | Yes (costly) |
| D2 | Single binary, no external services | Core differentiator vs. Hindsight/Zep | No |
| D3 | Markdown + frontmatter is the source of truth; index is a cache | Works with existing vault, Hermes, Claude, ChatGPT, git | No |
| D4 | Read-only default for MCP; writes need `--allow-write` | Agent safety; asymmetric cost of mistakes | No |
| D5 | Surgical in-place frontmatter patch (Strategy B), not append-only | The vault owner wants one file per decision, not version-suffixed duplicates | Yes, per-call (`--append-only` also supported) |
| D6 | Generic write primitives, not domain verbs like `append_journal_entry` | Tool must work for ADRs, specs, principles, journals alike | Yes |
| D7 | No auto-supersession; suggestions only | Silent graph mutation destroys trust in the vault | Yes (add opt-in later) |
| D8 | Authority multipliers + explicit status gate, not pure decay | A 3-year-old decision outranks today's passing thought | Tunable via config |
| D9 | Decay parameterized by half-life, not raw λ | Intuitive configuration; testable monotonicity | Yes |
| D10 | Lexical-first (M3) before embeddings (M4) | Daily utility and motivation before build complexity | No |
| D11 | Sync core, async only at the MCP edge | Keeps the learning surface small | Yes |
| D12 | No `unsafe` | Not needed; keeps focus | Yes |
| D13 | `bincode` sidecar, not SQLite | Teaches serialization, memory layout, and file formats; zero infra | Yes, behind a `Store` trait |
| D14 | Chunk breadcrumbs instead of LLM contextual retrieval | Deterministic, free, offline | Yes |
| D15 | Identity is matched, not written: declared id → content hash → title+date → title+version → unique title → path; identity is carried forward while the path is stable | Move resilience with zero vault modifications and zero migration; in-place edits never churn identity | Yes (key order is configurable) |
| D16 | Controller-agnostic core with CLI, MCP, and line-delimited JSON-RPC adapters | The vault owner uses multiple AIs; a Python wrapper and other controllers must not require reimplementing logic or FFI | No |
| D17 | Vectors always persisted in a separate flat `vectors.f32`, mmap'd; document text never stored in the index | Bounds RAM, makes cold start cheap, and avoids a format rewrite between M4 and M8 | Yes (format bump) |
| D18 | Writes require `expected_hash` (etag) from a prior read; enforced at every surface | Prevents lost updates when an agent writes based on a stale view | No |
| D19 | Links keyed by `DocId`; frontmatter paths resolved through a path alias map; moves detected by content hash / ID / chunk overlap | Agents and dream cycles reorganize files constantly; path-keyed links would rot immediately | No |
| D20 | Hand-written BM25 for the lexical layer; `fastembed` for the vector layer | Complementary signals (§7.2); the lexical layer is useful without any model, and the implementation is a deliberate Rust exercise | Yes (swap in `tantivy`) |
| D21 | macOS + Linux required, Windows must remain possible but unsupported in v1 | Covers the author's machines; `#[cfg]`-clean code and cross-platform crates keep Windows a later matrix change, not a rewrite | Yes (add CI later) |
| D22 | `cargo-dist` releases + curl installer + Homebrew tap; model downloaded separately, never bundled | Single binary, no runtime deps; keeps the download small and the offline path honest | Yes |
| D23 | Identity comes from document metadata (exact keys), never from similarity or move inference; similarity produces display-only continuation hints | Correctness must not depend on a human or LLM confirming a prompt, and must not require editing anyone's files to get started | No |
| D24 | `chunk_id` is content-addressed and excludes `doc_id` | Embedding cache reuse across edits, moves, and copies becomes automatic and identity-free | No |
| D25 | janus-mem never inserts an `id` or any other identity key; `adopt-ids` is removed | Writing metadata the user didn't ask for is the behavior that drove them off basic-memory. Derived/matched identity makes the migration unnecessary | No |
| D26 | `doc_version` is a fallback match key (after title+date), never the primary identity key | Versions are bumped as documents evolve, so keying identity on them would break continuity exactly when the document changes | Yes |
| D27 | Markdown only (`.md`, `.markdown`); no other formats in v1 | Non-Markdown text has no temporal metadata, can only pollute results, and adds parsing paths for no benefit at this stage | Yes (add include globs later) |
| D28 | Generated ids use cuid2 at length **16** (when id generation is enabled at all) | 16 chars ≈ 79 bits ≈ 1.2×10²⁴ values: a 99.9%-collision-free bound at ~4.9×10¹⁰ ids, far beyond any local vault. See §15 Q12 for the math and why length is not the real safety net | Yes |
| D29 | Performance is a guardrail, not a goal: p50 < 100 ms is the v1 bar; single-digit ms is aspirational; no micro-optimization before M8 measurements | The project exists to be useful and to teach Rust, not to win a benchmark; premature optimization also teaches the wrong lessons | Yes |
| D30 | Git coexistence is a hard requirement (§8.6) | Vaults are version-controlled; a tool that fights git is unusable regardless of retrieval quality | No |
| D31 | All persisted data is corpus-scoped: a declared, stable `corpus_id` in the manifest (identity) plus a path-hash `vault_id` (locator only) | Makes archived corpora and a multi-corpus registry additive rather than a format rewrite, while keeping identity independent of filesystem location | No |
| D32 | Document-level temporal relations only; no entity/relation graph | Keeps the model learnable and the index small; the high-value questions are document-shaped, and entity extraction is a different (LLM-heavy) product | Yes |
| D33 | Path-pattern type inference is configurable and best-effort; no folder layout is ever required | The tool must work on an existing vault as-is; a future `janus scaffold` is opt-in convenience | No |
| D34 | Add `finding` and `idea` as first-class types, and make the type set an open, config-driven registry (`TypeSpec`) rather than a closed enum | Findings (evidence) and ideas (proposals) are the two most common agents of change; `idea → decision` and `finding → finding` are the supersession chains the engine exists to track. An open registry lets domain types be added without a release | Yes (weights are config) |
| D35 | Model document links as generic `EdgeKind` values instead of a `SupersessionGraph` type | v1 uses only `Supersedes`, but a graph extension becomes additive rather than a format rewrite (§8.9) | Yes |
| D36 | The scorer takes an optional candidate set from day one | Graph traversal, tag filters, and “restrict to these docs” all reuse one scoring path, so a second, subtly different scorer never appears | Yes |
| D37 | Future entity/relation extraction is opt-in, index-time, versioned, and tagged `Derived`; declared edges always win | Keeps retrieval deterministic and offline while making machine-inferred knowledge auditable and visibly distinct from the user's claims | No |
| D38 | Valid time is explicit (`effective_from`/`effective_to`, optional) with chain-derived windows as a *fallback*, not the source | Derivation alone fails for in-place rewrites, decided-vs-effective dates, backdated imports, and branching. Explicit keys win, the chain fills gaps | Yes |
| D39 | Corpora have declared ids and roles (`primary`/`archived`); one index per corpus; archived corpora are hard-filtered from default queries | A weight still lets old facts into top-3 answers. Filtering by role is the only reliable guarantee, and per-corpus indexes make merge exact and failure isolated | Yes |
| D40 | Transaction time is delegated to git; janus-mem never writes system timestamps into frontmatter | Frontmatter timestamps mutate user files, pollute diffs, and are wrong after a sidecar rebuild. Git already stores every version with timestamps | Yes (add `--git-history`) |
| D41 | `as_of` queries bypass the status gate and set decay to 1 | Status is relative to *now*. Gating a historical query on today's status silently drops the correct answer | No |
| D42 | Cross-corpus links must be qualified (`corpus:path`) and are off by default; promotion is an explicit, etagged write | An archived decision must never supersede a current one by accident, and "manual confirmation" must be an action, not a prompt someone may ignore | No |

### Open questions (to resolve at the milestone in brackets)

1. **[M4]** Which embedding model ships as the default: `AllMiniLML6V2` (384d, fast) or `BGE-small-en-v1.5` (384d, better retrieval)? Add a `--model` override regardless.
2. **[M6]** `rmcp` vs. hand-rolled JSON-RPC stdio? Hand-rolled is ~300 lines and teaches the protocol; `rmcp` is less work and more future-proof.
3. **[M6]** Should the MCP server keep a warm index (fast, long-running) or reload per call (simple, always fresh)? Proposal: warm + `notify`-based invalidation, with `janus_status` reporting staleness.
4. **[M7]** Does the vault owner want `supersede` to also bump `doc_version` automatically, or only on request? Proposal: only on request (D8 spirit: never invent metadata).
5. **[post-1.0]** Multi-corpus: is a registry worth it, or is one primary vault plus one archive per agent config enough?
6. **[M8]** Add cross-encoder reranking? Would need an ONNX reranker; conflicts with "no LLM in the loop" only in spirit, not in fact. Default: no.
7. **[ongoing]** Should there be a `--format obsidian` output mode (wikilinks, block refs)? Post-1.0.
8. **[M3]** Is `rg`-style regex search worth exposing alongside BM25 (`janus grep`)? Leaning no — `rg` already exists.
9. **[M8]** Which quantization ships as a supported option — f16 only, or i8 too? f16 is nearly lossless and halves memory; i8 needs per-vector rescaling and an eval run to prove recall holds.
10. ~~**[M5]** Is 0.80 the right auto-apply threshold for structural move detection?~~ Resolved by D23/D25: no threshold is load-bearing. Identity is derived from document metadata; similarity only produces annotations.
11. **[post-1.0]** Python wrapper: thin `subprocess` client over `janus rpc` (no bindings), or PyO3? Proposal: `janus rpc` first — it also validates the adapter design.
12. ~~**[M8]** Should `write_document` generate ids? cuid2, UUIDv7, or ULID?~~ Resolved: cuid2, **16 chars**, opt-in via `--new-id` (D28). Collision math, with alphabet size 36 and the birthday bound `p ≈ N² / (2·36^L)`:

    | Length | Values | 99.9%-safe up to |
    |---|---|---|
    | 8 | 2.8×10¹² | ~75k ids |
    | 10 | 3.7×10¹⁵ | ~2.7M ids |
    | 12 | 4.7×10¹⁸ | ~97M ids |
    | **16** | **1.2×10²⁴** | **~49 billion ids** |
    | 24 (cuid2 default) | 2.2×10³⁷ | absurd |

    12 chars is already mathematically sufficient for any realistic vault; 16 costs nothing and removes the question. Length is *not* the real safety net, though: ids imported from other systems can collide regardless of length, so `doctor`'s duplicate detection stays mandatory.
13. **[post-1.0]** Index non-Markdown text (`.txt`, `.org`, `.rst`) behind an `[index] include` list? Deferred by D27. If added, those files need a defined default type (`note`), no date unless the filename parses, and a lower authority weight.
14. **[post-1.0]** Corpus registry UX: one process serving many roots, `--corpus` defaults, and whether archived corpora are read-only by default in every case.
15. **[post-1.0]** `janus scaffold`: generate an opinionated starter layout (`decisions/`, `specs/`, `principles/`, `journals/`) plus a `janus.toml` for users who don't have a vault yet. Strictly opt-in and separate from the indexing path (D33).
16. **[post-1.0]** Entity graph: which extractor (small local LLM, NER model, or agent-supplied edges at write time)? The last option is the cheapest and most trustworthy — the agent that writes a `finding` could declare its entities via frontmatter — but it depends on agent cooperation. Deferred; §8.9 defines the contract either way.
17. **[post-1.0]** Entity identity: normalized name + type, declared entity nodes in frontmatter, or both? Proposal: start with declared entities, add auto-resolution only with confidence and provenance.
18. **[OPEN — decide before M1]** Scopes, and whether `scope` participates in the identity match key. This is the one decision with real churn consequences: adding a field to identity later re-keys every scoped document. Everything else about scopes (flat tags, global default, filter-not-weight) is low-risk.
19. **[post-1.0]** `--git-history` enrichment: read commit timestamps with pure-Rust `gix` (no shelling out) to answer "what did we know at time T" and to make `as_of` + transaction time composable. Cost and performance unknown.
20. **[post-1.0]** Should explicitly-included archived corpora have a configurable weight, or a separate result section ("archived matches") so they can never rank above a primary answer?
21. **[M7]** Does `deprecated` still earn its place now that tombstones and `effective_status` exist? Candidate merge: `deprecated` = retired with no successor; keep the declared field for authoring, derive the rest.
22. **[M7]** When `supersede` patches a document, should it set `effective_to` on the old doc to the new doc's `effective_from`, or continue deriving it from the chain? Explicit is more correct but writes an extra key the user did not ask for.

---

## 16. Appendix

### 16.1 Example config (`janus.toml`)

```toml
index_dir = ".janus"   # the vault root is the directory containing this file

[frontmatter]                 # key aliases; first match wins
id = ["id", "uid", "uuid", "zettel_id", "permalink"]
title = ["title", "name"]
author = ["author", "generated_by", "model"]
type = ["type", "kind", "doc_type"]
status = ["status", "state"]
date = ["date", "created", "decided"]
effective_from = ["effective_from", "valid_from", "effective"]
effective_to = ["effective_to", "valid_to", "expires"]
supersedes = ["supersedes", "superseded"]
superseded_by = ["superseded_by", "superseded-by"]
tags = ["tags", "keywords"]
version = ["doc_version", "version"]

[defaults]
type = "note"
status = "active"

[types]                       # alias → canonical type
adr = "decision"
rfc = "spec"
log = "journal"
research = "finding"
proposal = "idea"

[authority]
decision = 1.50
spec = 1.35
principle = 1.20
finding = 1.10
journal = 1.00
note = 0.90
idea = 0.85

[decay]
half_life_days = 180
timeless_types = ["decision", "spec", "principle"]

[decay.by_type]
finding = 365
journal = 180
idea = 90

[paths]                       # optional path-pattern → type hints
"decisions/" = "decision"
"specs/" = "spec"
"principles/" = "principle"
"research/" = "finding"
"journals/" = "journal"
"ideas/" = "idea"

[[corpora]]                   # v1 loads the primary corpus; others are post-1.0
id = "main"
root = "."
role = "primary"

[search]
allow_cross_corpus_links = false
archived_weight = 0.8

[index]
exclude = ["archive/private/**", "**/*.draft.md"]

[chunking]
max_chars = 1800
overlap = 0.15

[embedding]
enabled = true
model = "AllMiniLML6V2"
batch_size = 32

[mcp]
allow_write = false
max_chars = 4000
```

### 16.2 Example MCP configuration

```json
{
  "mcpServers": {
    "janus-mem": {
      "command": "/Users/me/.cargo/bin/janus",
      "args": ["mcp", "--vault", "/Users/me/vault"],
      "env": { "RUST_LOG": "warn" }
    }
  }
}
```

### 16.3 Glossary

- **Agent memory platform** — a system that captures and curates an agent's conversational memories (extraction, consolidation, forgetting). janus-mem is not one; see §3.1.
- **As-of** — a query mode that filters by valid time instead of current status: what was true on a given date.
- **Authority** — multiplier reflecting how binding a document is (decision > spec > principle > journal).
- **Breadcrumb** — `title › H1 › H2` preamble added to embedding text only.
- **Chunk** — a heading-delimited section of a document, the unit of retrieval and embedding.
- **Corpus** — a root directory with its own index and a declared `corpus_id` and role (`primary` or `archived`).
- **Doc ID** — identity of a document, matched from declared metadata and carried forward while its path is stable (§6.4).
- **Match key** — the ordered metadata evidence used to attach a new path to a vanished document: declared id, content hash, title+date, title+version, unique title, then path.
- **mtime** — filesystem modification time. Used only for cheap change detection, `doctor` freshness reports, and as a *last-resort* date source. Never used for identity, because clones, checkouts, and copies churn it.
- **Intent** — a named bundle of decay + status-filter settings (`current`, `history`, `evolution`, `exhaustive`).
- **Ordinal** — a chunk's 0-based position within its document. Display ordering only; never identity.
- **Transaction time** — when the system recorded or learned a fact. Delegated to git history; never stored in frontmatter.
- **Valid time** — when a fact was true in the world: `effective_from` to `valid_to`. Distinct from authored date and from transaction time.
- **Sidecar** — the `.janus/` directory; derived, rebuildable index data.
- **Supersession** — declared replacement of one document by another; the graph edge behind `lineage`.
- **Timeless** — exempt from time decay (principles, active decisions).
- **Vault** — the root directory janus-mem indexes.
- **Vault ID** — a hash of the vault's canonical root path. A manifest locator only; never part of identity.
- **Validity window** — `[effective_from, valid_to)`: the interval during which a document's claim is true in the world.
- **Effective status** — status derived from the supersession graph (a document with a live successor is superseded), independent of the declared `status` field.

### 16.4 Definition of Done for v1.0

- All of M0–M7 shipped, `clippy -D warnings` clean, tests green, no `unsafe`.
- W1–W7 work end to end against the real vault, with `grep` disabled for a week.
- `doctor` is clean (or every warning is understood).
- A dry-run write pass over 200 real files produces zero unintended diffs.
- README quickstart works from a clean machine in under five minutes.
- `docs/notes/` has one honest entry per milestone.
- At least one non-author uses it without hand-holding (the real release gate).
