# M2 — Chunk and preserve: phased breakdown

> Branch: `m2-chunk-spans`. Spec: `SPEC.md` §6.2, §6.4 (chunk ids), §6.5, §6.9, §7.2 (what chunks feed),
> §8.1, §8.3, §8.5, §9.1, §10, §11 (M2), §13, §14.
> Milestone statement (SPEC §11): “Heading-aware chunker with breadcrumbs and fence/table awareness; byte-span
> tracking; a `--dump-chunks` command. Start the golden frontmatter tests here (they will guide M7).”
>
> **Done when:** chunking is idempotent, no chunk exceeds the limit, and no fence is ever split.
> (Restated measurably in §6, because “no chunk exceeds the limit” has one unavoidable exception — see CH-6.)
>
> **Learn targets (SPEC §12.1):** `pulldown-cmark` event lifetimes, `Vec<T>` vs `&[T]`, `Cow<str>`, and the
> borrow-checker fight with a streaming parser — “the main teaching moment of this milestone” (§11). Plus
> byte vs char vs `u32` span arithmetic and property testing.

M1 is merged (`56f100c`). Everything M2 needs already exists: `Document.body` is the verbatim body with
frontmatter removed, `Document.span.end` locates the body inside the file, `ChunkId` exists and documents the
§6.4 formula, and `ChunkingConfig { max_chars, overlap }` is parsed and unused.

## 0. Scope

### In scope
- `pulldown-cmark` event scan of **one body** into a heading tree with byte spans (§6.5 rule 1).
- Section → chunk splitting at paragraph/sentence boundaries with ~15% overlap, never inside a fenced code
  block or a table, and only between items of a list (§6.5 rule 2).
- Breadcrumbs (§6.5 rule 4) and `chunk_id` per the §6.4 formula (D24).
- Body-relative byte spans stored per chunk; **no chunk text is stored** (§8.3 rule 1).
- `janus chunks [PATH...] [--json] [--text] [--limit N]` — the milestone's `--dump-chunks` debug surface.
- Frontmatter **preservation** goldens: the first half of the M7 golden corpus (block sequences, flow
  mappings, anchors, multi-line scalars, quoted scalars, comments) asserted byte-identical through read.

### Explicitly NOT in scope (do not build)
- Inverted index, tokenizer, BM25, string pool, `index.bin`, `janus index` (M3). M2 computes chunks; it
  persists nothing.
- Embeddings, `Embedder`, `vectors.f32`, embedding cache (M4). `embed_text()` is *shaped* here, never called.
- Scoring, decay, status gates, intents, `--as-of` (M3/M5).
- `read_document` returning a chunk by id (M6), though `Chunk::text()` is what it will use.
- The **patcher** and invariants I1–I7 (M7). M2 only records that the fixture encodes are readable and
  byte-preserved; it does not splice.
- Any write path. `janus chunks` never touches the vault.
- `proptest` over patch invariants, `criterion`, `watch`, mmap.

## 1. Dependency changes (`Cargo.toml`)

| Crate | Version | Why (one-line justification per §8.2 policy) |
|---|---|---|
| `pulldown-cmark` | 0.13.x (verify with `cargo search`; SPEC §8.2 names it) | The only maintained CommonMark parser that emits an event stream **with byte offsets** (`into_offset_iter`), which is the entire basis of span-based chunking. |
| dev: `proptest` | 1.x | §10 requires the chunker to be proven non-panicking and span-correct over arbitrary Markdown; §8.2 already lists it. |

Feature policy for `pulldown-cmark`: `default-features = false` plus only what is needed. No `html` feature
(we never render HTML, and it pulls the escape crate), no `simd` (not a bottleneck at these sizes). Record this
in the dependency comment so a future reader does not “helpfully” re-enable them.

No new crates for: sentence splitting (hand-rolled scan, ~15 lines), Unicode normalisation (see CH-9), regex
(nothing in M2 needs one).

## 2. Data model (write these first — §12.3.3)

```rust
// src/chunk/mod.rs
pub struct Chunk {
    pub id: ChunkId,
    pub doc_id: DocId,
    pub heading_path: Vec<String>, // ancestors incl. the chunk's own heading, excluding the title (§6.5)
    pub span: (u32, u32),          // byte offsets into Document.body
    pub ordinal: u16,              // display order only; never identity (§6.5)
}

// src/chunk/blocks.rs — Phase 1 output: a heading tree over the body, no chunking yet.
pub struct BlockSpan {
    pub span: (usize, usize),
    pub kind: BlockKind,
    pub items: Vec<(usize, usize)>, // non-empty for `List` only
}

pub enum BlockKind { Paragraph, CodeBlock, IndentedCode, Table, List, BlockQuote, Html, Rule }

pub struct Section {
    pub heading_path: Vec<String>, // [] for the preamble before the first heading
    pub span: (usize, usize),      // from the heading line through the last block of the section
    pub blocks: Vec<BlockSpan>,
}
```

**Naming deviation, deliberate.** SPEC §6.5 declares `char_span: (u32, u32)` while its own comment says “byte
offsets into the doc body, not chars”. Bytes are what the writer and `read_document` need, so the field is
named `span` here and documented as bytes. Renaming a field nobody has consumed is free now and confusing
later; the SPEC line gets corrected in the phase-4 docs commit (CH-1).

**What is not on `Chunk`.** No `text`, no `embed_text`, no `title`. §6.5/§8.3 are explicit that chunk text is
transient; the body is owned by the `Document` and chunks borrow from it via
`Chunk::text(&self, body: &str) -> &str`. That accessor is the whole of M6's `read_document` chunk path.

## 3. Decisions to settle in the design conversation (defaults chosen)

| # | Question | Default | Where it lands |
|---|---|---|---|
| CH-1 | `char_span` vs byte spans | **Byte spans**, field named `span`. SPEC §6.5's own comment specifies bytes; the field name is a typo. Additive DTO clarity, no api bump (nothing consumed it). | `src/chunk/mod.rs` |
| CH-2 | Which Markdown extensions are on | `ENABLE_TABLES | ENABLE_STRIKETHROUGH` only. Tables must be *recognised* to be atomic; strikethrough is free and harmless. **No** smart punctuation (it rewrites text, which must stay verbatim), no footnotes/tasklists (they change the event stream for no M2 benefit). | `src/chunk/blocks.rs` |
| CH-3 | Setext headings (`Title` + `===`) | **Recognised.** The event stream yields them as `Heading` and ignoring that would split sections incorrectly. Note in the module docs that a body line followed by `---` is a setext H2, not a thematic break — this is exactly the frontmatter trap F-2 already handles at offset 0. | `src/chunk/blocks.rs` |
| CH-4 | Does a section's span include its own heading line | **Yes** — the chunk's text starts with `## Decision`. The breadcrumb then names the *ancestors* (title › H1), not the chunk's own heading, so `embed_text` never repeats the heading (§6.5 rule 4). | `src/chunk/split.rs` |
| CH-5 | Does the document title enter `chunk_id` | **No.** `chunk_id = blake3(join(heading_path, "\0") ‖ 0x00 ‖ normalized_text)`; the title is prepended by `embed_text()` at index/search time, from `DocumentMeta`. Literal §6.5 rule 4 shows `title › H1 › H2` for the *embedding* breadcrumb, but §11-M4 requires a renamed-but-unchanged file to cost **zero** embedding work — and the default title is the filename stem, so including it would re-key every chunk on every rename. D24's whole point is that chunk ids are identity-free. | `src/chunk/split.rs` |
| CH-6 | A unit larger than `max_chars` | **Indivisible units become an oversized chunk**, reported as a note (`oversized_block`). Atomic units are fenced/indented code blocks and tables. Paragraphs are splittable (sentence → word → char), lists are splittable **between top-level items**. So “no chunk exceeds the limit” is true except for one code block or table that is itself larger than the limit; splitting those would corrupt fidelity, which §6.5 rule 2 forbids. The exception is loud, never silent. | `src/chunk/split.rs` |
| CH-7 | `max_chars` unit | **Unicode scalar values** (`chars().count()`), because the key is named `chars` and 1800 ≈ 450 tokens only holds for Latin text. Consequence recorded: a CJK document gets ~1800 *tokens* per chunk. If that proves bad, the fix is a token budget in M4, not a silent reinterpretation as bytes. Byte size is additionally bounded by `4 × max_chars` when slicing (defensive, never hit). | `src/chunk/split.rs`, `src/config.rs` docs |
| CH-8 | Overlap semantics | Overlap is **re-included whole units** from the previous chunk's tail, targeting `overlap × max_chars`, capped at half the previous chunk so progress is guaranteed. Overlapping spans are legitimate and expected; `ordinal` still increments. Never overlap a fenced block or table boundary that is already atomic. | `src/chunk/split.rs` |
| CH-9 | `normalized_chunk_text` for the id | `\r\n`/`\r` → `\n`; trailing whitespace stripped per line; leading/trailing blank lines removed. **No** Unicode NFC/NFKC (needs a crate, and would silently merge visually distinct text). Comment stays verbatim inside fences: only line endings and edge whitespace change, so two documents that differ only in CRLF share one id — desired, because the same bytes-on-a-derive are one chunk. | `src/chunk/split.rs` |
| CH-10 | Duplicate `chunk_id` inside one document | **Expected, not an error** (D24: two identical sections are one chunk content). Both records are kept, ids are allowed to repeat, and the M4 embedding cache dedupes by id. Nothing in the chunker asserts uniqueness. | `src/chunk/split.rs` |
| CH-11 | H1 as `Document.title` | **No.** A body H1 must not enter identity: §6.4 rung 3 keys on title, so editing a heading would re-key the document, exactly what §6.4 stability forbids (same reasoning as D26 for `doc_version`). The M1 comment “H1 arrives with M2” is superseded: the H1 becomes the first `heading_path` element instead, which gives the breadcrumb the H1 without touching identity. When the H1 equals the title (case-sensitive, after trim) it is omitted from the breadcrumb to avoid `undated › Undated`. | `src/chunk/split.rs`; M1 comment corrected |
| CH-12 | Command shape | New subcommand **`janus chunks`** (not a `scan --dump-chunks` flag). `scan` stays a pure inventory: adding chunking to it would change the M1 golden and make the cheapest command do the most work. Optional positional `PATH`s are resolved with `vault::confine` and matched by `rel_path` exactly — no globs, no new matcher. Additive CLI surface; SPEC §9.1 gets one line. | `src/cli.rs`, `src/chunk/dump.rs` |
| CH-13 | Does a chunk note belong on `Document.notes` | **Yes** — new `DocNote` variants (`OversizedBlock { chars }`, `BodyTooLarge`). R-8 already established one vocabulary for skips and irregularities; a second `ChunkNote` enum would force every renderer to learn two. Variants are additive in JSON (consumers switch on `code`). Only `janus chunks` emits them; `scan` never chunks, so its golden is unchanged. | `src/document.rs`, `src/chunk/split.rs` |
| CH-14 | Body larger than 4 GiB | `Chunk.span` is `(u32, u32)` per §6.5. Check `body.len() > u32::MAX` once per document → skip chunking, `BodyTooLarge` note. No markdown body is ever this large; the check exists so the cast is not a silent truncation. | `src/chunk/mod.rs` |

Record each settled decision in the phase log at the end of this file, not in a separate journal (AGENTS.md).

---

## Phase 1 — Event scan: body → sections with spans (½–¾ day)

**Goal:** one pure function `sections(body: &str) -> Vec<Section>` that turns Markdown into a heading tree
whose spans are byte-exact, with fenced code, indented code, tables, quotes, and lists identified so Phase 2 can
refuse to split them.

**Files:** `src/chunk/mod.rs` (new), `src/chunk/blocks.rs` (new), `src/lib.rs`, `Cargo.toml`.

**Build (~230 logic lines):**
1. `Parser::new_ext(body, options).into_offset_iter()` (CH-2). Drive it with an explicit `Vec` stack for open
   headings — no recursion (§12.1: “avoid gratuitous recursion”).
2. Heading collection: on `Start(Tag::Heading { level, .. })`, concatenate `Event::Text` and `Event::Code`
   until `End(Heading)`; trim; that is the heading's text. On a heading of level `L`, close every open section
   whose heading level is `>= L`.
3. Block spans at section depth: every top-level `Paragraph`, `CodeBlock`, `Table`, `List`, `BlockQuote`,
   `HtmlBlock`, `Rule`. Take `start` from the opening event's offset and `end` from the closing event's offset,
   then **extend to the end of the line** (pulldown-cmark's `End` offset stops at the last byte of the block's
   content, not after the newline). Assert `is_char_boundary` on both ends in debug.
4. Lists: record each `Start(Item)`/`End(Item)` pair in `items` so Phase 2 can split between items (CH-6).
   Distinguish `CodeBlockKind::Fenced` from `Indented`, because only the former has a fence to protect.
5. Preamble (content before the first heading) is a `Section` with an empty `heading_path`.
6. Empty sections are dropped here (§6.5 rule 3) — a heading whose only content is a nested heading produces
   no section, but its text is still in the parent's `heading_path`.

**Tests (~250 lines, unit):**
- Section tree: `# A › ## B › ### C` produces three sections with the right `heading_path`s; a `## B2` after
  `### C` closes `C` and stays under `A`.
- Heading inside a fenced block does **not** create a section; `#` inside a table cell does not either.
- Setext heading (`Title\n====`) is a section (CH-3); `text\n---` is a setext H2, not a rule (CH-3 caveat).
- Indented code and fence-with-tilde (`~~~`) are both `CodeBlock`/`IndentedCode`.
- Fenced block ending at EOF without a closing fence still yields one block whose span reaches EOF.
- Spans: `body[block.span]` reproduces the block text for every block in the fixture vault; every offset is a
  char boundary; blocks are ordered and non-overlapping inside a section; sections partition the body except for
  whitespace.
- Unicode: emoji/CJK adjacent to headings and fences (M1's F-* guarantee carried forward).
- Empty body, body of only whitespace, body of only headings → zero sections, no panic.

**Handoff to another AI (paste):** “Implement Phase 1 of M2 for janus-mem per `docs/m2-breakdown.md` Phase 1 and
CH-1…CH-3. Read `SPEC.md` §6.5 and `src/document.rs` first. Add `pulldown-cmark` with `default-features =
false` and a one-line justification. No `unwrap()` in lib code; no recursion; every span asserted to be on a
char boundary; tests first for the span and section rules. Decisions go in the M2 phase log.”

**Accept:** `cargo test` green; span round-trip test passes over every fixture document.

---

## Phase 2 — Section → chunks: splitting, overlap, ids (1–1½ day)

**Goal:** `chunk_body(doc_id, body, config) -> (Vec<Chunk>, Vec<DocNote>)` — the milestone's actual rule set.

**Files:** `src/chunk/split.rs` (new), `src/chunk/mod.rs`, `src/document.rs` (notes), `src/config.rs` (docs only).

**Build (~260 logic lines):**
1. `ChunkingConfig` validation at the point of use (not an error): `max_chars == 0` → treat as 1 and warn via
   note; `overlap` clamped to `0.0..=0.5`. Config stays fail-open (I-8).
2. Unit expansion per section: paragraph → sentences (scan for `. `, `! `, `? `, `.\n`, and the CJK `。`), then
   words, then chars; list → its `items`; code/table → one atomic unit.
3. Greedy packing: accumulate units while `chars <= max_chars`; on overflow emit a chunk ending at the last
   included unit's end, trimmed. Chunk spans are `trimmed(first.start .. last.end)` so no chunk begins with the
   blank line that separated it from its predecessor.
4. Overlap (CH-8): the next chunk starts by re-including whole tail units totalling ≥
   `overlap × max_chars`, capped at half the emitted chunk's length; assert the new start is strictly greater
   than the previous start (progress guard — a bug here loops forever on a pathological body).
5. Oversized unit (CH-6): emit alone + `DocNote::OversizedBlock`.
6. Identity: `normalize()` (CH-9) then `ChunkId::from_bytes(blake3(path_join ‖ 0x00 ‖ text))`, where the path
   join uses `0x00` between heading components, matching S-3's reasoning (NUL cannot appear in Markdown text or
   a filename). No title (CH-5), no `doc_id` (D24).
7. `ordinal` assigned after emitting, `u16` — a document with >65535 chunks is impossible at `max_chars` > 0
   unless the body is >~100 MB; saturate and note rather than panic.
8. `embed_text(title, chunk)` and `breadcrumb()` helpers: `title › A › B` joined with `›` and spaces, H1 omitted
   when it equals the title (CH-11). These do not affect ids and are not called by M2's own command — they exist
   so M3/M4 do not invent a second breadcrumb format.

**Tests (~350 lines; this is the “test-first for anything with a rule” zone §12.3.4):**
- `max_chars`: a 5,000-char body of paragraphs yields chunks all ≤ limit, with the tail re-included per CH-8.
- Never split: a 3,000-char fenced code block yields **one** oversized chunk + one note; same for a table; no
  chunk contains an odd number of lines matching `^\s*(```|~~~)`.
- Lists: a 4,000-char 20-item list splits **between items**; no chunk starts mid-item; no item is duplicated
  except by overlap.
- Heading spans: every chunk's text starts with its own heading when the section starts with one (CH-4).
- Breadcrumbs: `title › H1 › H2`; H1 == title is omitted; preamble chunk has `heading_path == []`;
  `embed_text` starts with the breadcrumb and contains the chunk text verbatim.
- Ids: same text + same heading path in two documents ⇒ identical `ChunkId` (D24); different heading path or
  text ⇒ different id; ids do not move when `DocId`, `rel_path` or `title` change (CH-5); CRLF vs LF body of the
  same text ⇒ same id (CH-9); duplicate ids within one document are accepted (CH-10).
- Ordinals: contiguous from 0, in span order; spans are monotonically increasing in `start` and never exceed
  `body.len()`.
- Idempotency/determinism: chunking the same document twice is `==` (ids, spans, ordinals).
- Coverage: union of chunk spans equals the body except whitespace between chunks and any dropped empty
  section — computed with a byte-set comparison, not by eyeballing.
- Property tests (`proptest`, added in this phase): arbitrary Markdown bodies (`\\PC*`, plus a generator mixing
  fences/tables/headings) never panic, never produce a span outside the body, never split a char, and always
  terminate (the progress guard). Shrinking must not require `unwrap` — use `prop_assert!`.
- Config edge: `max_chars = 0`, `overlap = 1.0`, `overlap = -0.2` → clamped, no panic, note emitted.

**Handoff to another AI (paste):** “Implement Phase 2 of M2 per `docs/m2-breakdown.md` Phase 2 and CH-4…CH-11.
Read Phase 1's `Section`/`BlockSpan` and `SPEC.md` §6.4 (chunk_id), §6.5 (rules 1–5), §8.3 (no text stored).
No `unwrap` in lib code; every fallback emits a `DocNote`; ids must be reproducible from
(heading_path, normalized text) alone. Tests first: the split/overlap/id rules are the point of the phase.”

**Accept:** all split/overlap/id tests green; `cargo clippy -D warnings` clean; no new dependency.

---

## Phase 3 — `janus chunks` (½–¾ day)

**Goal:** run the pipeline end to end over a corpus and print chunks; the debugging surface that makes Phase 2
verifiable by hand, and the seam M3 will call.

**Files:** `src/chunk/dump.rs` (new), `src/cli.rs`, `src/main.rs`, `src/scan.rs` (reuse DTO helpers where they
already exist), `src/lib.rs`.

**Build (~200 logic lines):**
1. Pipeline: `resolve_corpus` → `Config::load` → `walk` → `resolve_all` → `derive_identity` → `chunk_body` per
   document. Identical to `scan::run` up to the last step, which is the point: no second pipeline.
2. Optional positional `PATH`s: `vault::confine` each, then keep documents whose `rel_path` matches exactly.
   A path outside the corpus is `PathEscape` → exit 4, not a silent miss (this is the first user-facing use of
   M1's confinement).
3. Human output: per document a `rel_path` header, then `ordinal  id[..8]  bytes  chars  breadcrumb` rows;
   `--text` appends the chunk text, truncated at 120 chars with a trailing `…` (never truncated without
   `--text`; the default output is metadata-only, consistent with §8.5).
4. `--json`: `{ "api": 1, "vault": …, "corpus_id": …, "chunking": { "max_chars": …, "overlap": … },
   "docs": [ { "id", "rel_path", "title", "chunks": [ { "id", "ordinal", "start", "end", "chars",
   "heading_path": [...], "text"? , "notes": [...] } ] } ], "total_chunks": n }`. `text` appears **only** with
   `--text`; the versioned shape with `"api": 1` is unchanged by that flag, exactly as `--verbose` is for
   `scan` (J-11).
5. `--limit N`: cap chunks per document, with `"truncated": true` on that document. Exists so a 2 MB fixture
   does not flood a terminal or a test snapshot.
6. Notes surface as `code` + `message` pairs like `DocNote::code()` already does; chunk notes are attached to
   the document entry, not to a chunk, because “this document contains an indivisible 3,000-char fence” is a
   property of the document.
7. `main.rs` stays thin: `Command::Chunks => chunk::dump::run(...)`.

**Tests (~200 lines, `assert_cmd` + `insta`):**
- Fixture vault: chunk count and total are stable; `--json` parses, has `"api": 1`, and contains no `"text"`
  key unless `--text`.
- Golden `insta` snapshot of `janus chunks --json --limit 3` over the fixture (redacting nothing: chunk ids are
  content-addressed, so unlike `scan`'s ids they are machine-independent — a stronger golden than M1's).
- `--text` includes the body of a known chunk; default output does not.
- A `PATH` argument selects exactly one document; a path outside the vault → exit 4, empty stdout, `error:` on
  stderr; a path that does not exist in the corpus → exit 1 with a message naming it.
- The 2 MB fixture document chunks and `--limit` truncates it; the non-UTF-8 fixture still appears only as a
  read skip and never reaches the chunker.
- Tree unchanged after the command (same “no writes” assertion as M1's scan test).
- Determinism: two runs differ in nothing at all — no timestamps in this DTO, which is why `janus chunks`
  needs no redactor.

**Handoff to another AI (paste):** “Implement Phase 3 of M2 per `docs/m2-breakdown.md` Phase 3 plus CH-12/CH-13.
Read `src/scan.rs` for the established DTO/exit-code pattern and `SPEC.md` §8.5 (no content without an explicit
flag), §9.1 (exit codes), §9.4. stdout is results only; logs to stderr. Reuse `vault::confine` for PATH
arguments; never reimplement the pipeline.”

**Accept:** `janus chunks --json | jq .` works on the fixture vault; `janus chunks` on this repo's `docs/`
completes without panicking; exit codes match §9.1.

---

## Phase 4 — Frontmatter preservation goldens, fixtures, docs (½ day)

**Goal:** the second half of the milestone statement (“start the golden frontmatter tests here — they will guide
M7”) plus the docs and phase log. Logic-light, test-heavy, and deliberately last so the fixture changes one
snapshot in one PR.

**Files:** `tests/fixtures/vault/**` (new files + `janus.toml` if needed), `tests/frontmatter_preserve.rs` (new),
`tests/snapshots/golden_scan__scan.snap` (updated), `docs/m2-breakdown.md`, `README.md`, `SPEC.md`.

**Build (~60 logic lines, rest is fixtures/tests):**
1. New fixture documents covering the encodings §7.3 must splice or refuse: a block-sequence `tags`, a flow
   mapping (`owner: {name: me, team: infra}`), an anchor/alias pair, a `|` multi-line scalar, single- and
   double-quoted scalars with escapes, a trailing `#` comment on a key, CRLF frontmatter (extending
   `crlf-spec.md`), and a body whose first line is `---` after a valid frontmatter block.
   Each is a *document*, so it must also chunk sanely — one fixture does double duty.
2. `tests/frontmatter_preserve.rs`: for every fixture file, assert the isolation contract that M7 depends on —
   `raw_frontmatter` is byte-identical to the file's fenced region, `body` is byte-identical to the suffix after
   `span.end`, `span.start` follows a BOM but does not include it (F-1), and the typed view matches the expected
   values. This is a *read* golden; no patching.
3. Update the `scan` golden: adding fixtures changes document counts and ids. Redact exactly what J-7 names
   (`taken_ms`, `mtime`) — nothing wider.
4. Docs: README status line (M2), the docs list (`docs/m2-breakdown.md`), SPEC §9.1 (`janus chunks`), SPEC §6.5
   (`char_span` → `span`, bytes), and SPEC §15 rows for the two substantive deviations — **D48** chunk ids
   exclude the title (CH-5) and **D49** a body H1 never becomes the identity-bearing title (CH-11).
5. Phase log (§§7–10 below) filled with the decisions actually taken, including any deviation from this plan.

**Tests (~150 lines):** the preservation matrix over the fixture set, plus a proptest that random valid
frontmatter blocks round-trip through the *reader* unchanged (the writer's turn comes in M7).

**Handoff to another AI (paste):** “Implement Phase 4 of M2 per `docs/m2-breakdown.md` Phase 4. These fixtures
are the M7 golden corpus: do **not** add fixtures you cannot describe as a splice case from `SPEC.md` §7.3.
Update the scan golden only by redacting what J-7 names. Record every deviation in the M2 phase log, never in a
new `docs/notes/` journal.”

**Accept:** all gates green, including the Windows `cargo check`; `insta` snapshots accepted and reviewed;
README no longer claims M1.

---

## 4. Cross-cutting requirements (checked at the end and by review)

- **Span safety.** Every offset is a byte index into `Document.body`; every boundary is asserted
  `is_char_boundary` in debug; `start <= end <= body.len()`; `u32` conversion checked once per document
  (CH-14). No `as u32` without the check.
- **No text stored (§8.3).** `Chunk` holds spans and a heading path. Text exists only through
  `Chunk::text(body)`, and `--text` is the only surface that prints it (§8.5).
- **Determinism.** Sorted documents by `rel_path`, ordinals by span, ids content-addressed. No timestamps in the
  chunks DTO. Two runs are byte-identical.
- **Fail-open.** A body the parser cannot make sense of still yields the whitespace-delimited fallback section
  (one preamble section) rather than an error; every degradation is a `DocNote`.
- **No panics.** Indexing everywhere, no `unwrap()` in lib code, progress guards on both loops. `proptest` is the
  evidence, not the intention.
- **Cross-platform (§8.7).** Byte offsets, `/`-separated `rel_path`, no `std::os::*`, CRLF preserved verbatim in
  text and normalised only inside the id hash (CH-9).
- **No writes, no index, no network.** `janus chunks` asserts the tree is unchanged.

## 5. PR plan and size budget

Each phase is one branch from `main` after the previous merge (stacked PRs use `BASE=origin/<prev>`), one PR,
one logical unit:

| PR | Branch | Logic estimate | Tests estimate | Why it splits here |
|---|---|---|---|---|
| P1 | `m2-chunk-spans` | ~230 | ~250 | Pure parser layer; nothing downstream depends on chunking rules yet. |
| P2 | `m2-chunk-split` | ~260 | ~350 | The rule set; the borrow-heavy, decision-dense part. |
| P3 | `m2-chunks-cli` | ~200 | ~200 | Command surface; reuses the existing pipeline. |
| P4 | `m2-preserve-goldens` | ~60 | ~150 | Fixtures + docs; changes shared goldens, so it lands alone. |

All four are under the 600-line logic ceiling with room for review fixes. If P2 approaches 400, the overlap
rule (CH-8) ships as `m2-chunk-overlap` on its own — overlap is the only part of the chunker with no consumer in
M2's acceptance test.

## 6. Definition of done for the whole milestone

SPEC's three conditions, made checkable:

1. **Idempotent.** Chunking the same body twice yields equal `Vec<Chunk>`; chunking a corpus twice yields a
   byte-identical `janus chunks --json`.
2. **No chunk exceeds the limit** — except a single indivisible code block or table (CH-6), and every such
   exception appears as an `oversized_block` note in the report. A limit violation with no note is a bug.
3. **No fence is ever split.** Every chunk's text contains an even number of fence markers for each fence
   character; a property test asserts it over generated Markdown, and the fixture vault asserts it over real
   files.
4. `chunk_id` is content-addressed per D24: identical text + heading path ⇒ identical id across documents,
   unaffected by `DocId`, path, or title (CH-5).
5. Frontmatter preservation goldens exist for the M7 splice cases (CH-* fixtures) and pass on read.
6. No index is written, no vault mutation, no `unwrap()`/`expect()` in lib code, clippy `-D warnings` clean,
   Windows `cargo check` still compiles.
7. Decisions recorded in §§7–10 of this file; SPEC §15 carries D48/D49; README status updated.
8. Suggested commit sequence: `P1 chunk spans`, `P2 chunk splitting + ids`, `P3 janus chunks`,
   `P4 preservation goldens + docs` — each with a real message, each green.

## 7. Implementation log — Phase 1 (event scan)

<!-- Filled while building. Same shape as docs/m1-breakdown.md §§6–12: one row per decision, with *why*. -->

## 8. Implementation log — Phase 2 (splitting + ids)

## 9. Implementation log — Phase 3 (`janus chunks`)

## 10. Implementation log — Phase 4 (fixtures, goldens, docs)

## 11. Handoff to M3

<!-- What M3 inherits: chunk accessors, the pipeline seam, what is deliberately still missing. -->
