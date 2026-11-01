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

Settled while building it: the scanner is `src/chunk/{mod,blocks}.rs` as planned, and the acceptance test
is `tests/chunk_sections.rs`, which runs the real pipeline (walk → resolve → sections) over the committed
fixture vault rather than over hand-written strings.

| # | Decision | Why |
|---|---|---|
| E-1 | Spans come straight from `pulldown-cmark`'s own `Start`/`End` offsets; the planned “extend to end of line” step is **dropped**. | Verified against 0.13.6: every block kind's event range already covers the closing fence, the last table row and the trailing newline. Re-deriving the end by scanning for a newline would be a second implementation of the parser's block boundaries, and its failure mode — an end after the last content line but *before* the closing fence — is exactly the “fence is split” bug this milestone forbids. |
| E-2 | A heading opens a section only when **no block is open**; a heading inside a quote or a list item does not. | §6.5 rule 1 says “split by ATX headings”, but the intent is document sections. `> # Quoted` is a quote's content, and promoting it to an H1 would make everything after the quote its child. `pulldown-cmark` already refuses to see a fenced `#` as a heading, so nesting is the only structural rule left for us to enforce. |
| E-3 | Span type is `Range<usize>` internally; `Chunk.span` stays `(u32, u32)`. | Internal code slices and compares, where `.start`/`.end` beats `.0`/`.1`, and nothing internal is serialized. The chunk tuple is the persisted/DTO shape that must match §6.5, so the conversion happens once at the boundary. |
| E-4 | Containers that are never emitted (items, footnote definitions, nested headings) push a `Frame::Nested` marker. | The stack must stay balanced so each block-level `End` pops exactly one frame. A separate depth counter would be a second thing to keep in sync; `Nested` carries no state beyond being a frame. |
| E-5 | Heading text is flattened and whitespace-collapsed (`split_whitespace().join(" ")`). | A heading's text is a breadcrumb component, not rendered content: inline code and emphasis must not leak markup (`` # `a` *b* `` → `a b`), and a heading wrapped across lines must not put two spaces in a breadcrumb. |
| E-6 | Setext headings are sections, and `text` followed by `---` is an H2, not a thematic break. | The event stream yields them as `Heading`, so ignoring them would mis-scope every section below. CH-3 is confirmed by a test rather than assumed — and it is the same `---` ambiguity F-2 resolves at offset 0. |
| E-7 | Sections with no blocks are dropped in `finish()`, not by the splitter. | §6.5 rule 3 drops chunks with no text; doing it one layer earlier means no consumer has to special-case `blocks.is_empty()`. A heading that only names its children still reaches their `heading_path`, because the path is built before the drop. |
| E-8 | `IndentedCode` is its own `BlockKind` and is atomic, like a fenced block. | Indented code has no closing marker, so it needs no fence *protection* — but a chunk starting mid-block is still a fragment with no context. Keeping the kind distinct preserves the parser's own distinction (it costs nothing) while `is_atomic` answers the splitter's only question. |

**Deviations from the Phase 1 plan:** only E-1 and E-3, both narrowings rather than widenings; the planned
`extend_to_line_end` helper does not exist because it turned out to be unnecessary.

## 8. Implementation log — Phase 2 (splitting + ids)

Settled while building it: the splitter is `src/chunk/split.rs`, with the rule tests in its `#[cfg(test)]`
module, the invariants in `tests/chunk_properties.rs` (proptest) and the fixture sweep in
`tests/chunk_split.rs`. The two shrunk proptest inputs live in `tests/chunk_properties.proptest-regressions`
and stay committed even though they now pass.

| # | Decision | Why |
|---|---|---|
| K-1 | **The heading line is an atom**, prepended to the section's blocks before packing. | `Section.blocks` deliberately starts *after* the heading (phase 1), so without this the first chunk of every section would silently lose the `## Decision` line that CH-4 promises. A test caught it immediately; the fix is one span, not a change to `Section`. |
| K-2 | Atom kinds: **heading / whole block / list item / sentence / word / character**, in that order of preference. | Splitting is only *allowed* at an atom boundary, so "never inside a fence, table or item" is a property of the data rather than a check in the loop. Paragraphs get the finer atoms only when they are already too large, which keeps ordinary documents at one chunk per section. |
| K-3 | **Bug (found by running the property tests):** `overlap_start` returned the chunk's own first atom when the chunk consisted of exactly one atom, so a document with an oversized fence followed by any content looped forever. Fixed by an `earliest = first + 1` floor, and pinned by `an_oversized_chunk_with_overlap_still_advances`. | The plan predicted this class of bug ("a bug here loops forever"); the guard was written for the multi-atom case only. A hang is the worst failure mode a scanner can have, so the floor is now unconditional and the regression test is the *second* atom after a single-atom chunk — the exact shape that hung. |
| K-4 | The limit and the boundary search **ignore the CR of a CRLF pair** (`chars_in`, `char_slices`). | A carriage return is line-ending noise, not content. Counting it made an `autocrlf` checkout split at different sentences than an LF checkout, which changed chunk ids for identical documents — precisely what CH-9 exists to prevent. The property test `crlf_bodies_produce_the_same_chunk_ids` is the guard. |
| K-5 | `chunk_id` NUL-terminates each path component and **length-prefixes the text**. | Without the length prefix, `heading_path = ["a","b"]` with text `"x"` and `["a"]` with text `"b\0x"` hash identically — and a body may legally contain a NUL byte. The prefix costs eight bytes and removes the ambiguity by construction. |
| K-6 | `normalize` returns `Cow<str>`, borrowing when the text is already clean. | The common case is the borrow (chunks are trimmed and most files are LF), and the M2 learn list names `Cow` explicitly. The slow path is two allocations and only runs for CRLF or trailing whitespace. |
| K-7 | `DocNote` gained a **third** variant, `TooManyChunks`, beyond CH-13's two. | Ordinal saturation past 65 535 chunks is reachable with a tiny `max_chars`, and silently reusing `u16::MAX` would hide an ordering collapse. One note per document, not per chunk, so it cannot bury the real ones. |
| K-8 | `OversizedBlock { chars }` reports the **measured chunk length**, and notes are appended in chunk order. | The count is what a reader needs to judge the config; document order keeps the report deterministic. Only an indivisible atom can trigger it, because paragraphs are split down to single characters first — the note is never a rounding error. |
| K-9 | `embed_text` drops **any** heading equal to the title, not just a first-level one. | `Section.heading_path` stores names, not levels, and levels are not needed for identity — `Undated › Undated` is the thing being avoided, and equality is the property that produces it. A level-based rule would have required a parallel `levels` vector on every `Section` for one display nicety. |
| K-10 | The 4 GiB check is `body_size_note(len)`, a free function on a length. | The branch is otherwise untestable without allocating four gigabytes; splitting it from `chunk_body` lets `an_oversized_body_is_refused_by_length_not_by_allocation` prove the boundary at `u32::MAX`. |

**Deviations from the Phase 2 plan:** the "unit" concept is called an *atom* (the plan used "unit"), and the
paragraph splitter prefers sentences before words — both naming-level. Real additions: K-4 (ignoring `\r` in the
limit) and K-7 (a third note variant). No public shape changed.

## 9. Implementation log — Phase 3 (`janus chunks`)

Settled while building it: the command is `src/chunk/dump.rs`, with the surface tests in `tests/cli_chunks.rs` and
an `insta` golden in `tests/golden_chunks.rs` over the committed fixture vault. `scan`, `chunks` and every later
report now share `document::NoteDto` instead of each defining the same `{code, message}` pair.

| # | Decision | Why |
|---|---|---|
| C-1 | The report's `api` is `scan::API_VERSION`, imported rather than re-declared. | AGENTS.md's versioning table has **one** API version for every `--json` response, not one per command. Two constants in two modules are two things to forget to bump together. |
| C-2 | `NoteDto` and `DocNote::dto()` live in `document.rs`; `scan` was switched to them and its private copy deleted. `ReadSkipDto` was **left duplicated** on purpose. | This is the second occurrence in a different module and M3 would be the third, which is exactly the signal AGENTS.md's DRY rule names. §9.5 asks for one DTO module; putting the pair on the note type itself is the smallest version of that, and shape and field order are unchanged, so M1's `scan` golden did not move. `ReadSkipDto` was attempted the same way and reverted: it is ten lines that would have to be *deleted* from `scan` and re-added in `vault::resolve`, and the PR-size script counts a deleted line as a logic line, so the move cost ~40 budget lines for a ten-line saving. The third occurrence (M3) is the point to do it, with the budget to match. |
| C-3 | `total_chunks` counts chunks **emitted**, and `truncated` appears only where `--limit` actually hid chunks (a `false` flag is omitted). | A consumer sums `docs[*].chunks` for its own total; a report-level number that counted hidden chunks would disagree with the rows right above it. Omitting `false` keeps an untruncated report free of a key nobody needs. |
| C-4 | Chunk notes go on the **document** entry, after the resolve/identity notes; the per-chunk `"notes"` in the phase-2 sketch of the DTO does not exist. | CH-13 already settled this: “this document contains an indivisible 3 000-character fence” is a property of the document. One flat list also means `janus chunks` shows *everything* odd about a document in one place instead of splitting irregularity across two levels. |
| C-5 | `chars` comes from a new `Chunk::char_count`, which shares `count_chars` with the splitter; the renderer never recounts on its own. | A report that counted the `\r` of a CRLF pair would show `chars = 1801` against `max_chars = 1800` for a document that is merely checked out with `autocrlf` — a limit violation that is not one. The rule lives once, in `split`. |
| C-6 | A `PATH` argument is confined first, then matched by `rel_path`. A relative path is read **twice** — against the working directory, then against the corpus root — and the escape refusal (exit 4) outranks “no document” (exit 1, new `JanusError::DocumentNotInCorpus`). | Both readings are things a caller means: `janus chunks notes/undated.md` from inside `decisions/` wants the working directory, while a `rel_path` copied out of a report wants the corpus root. Both spellings are confined, so neither is a way out. Naming the corpus-root reading is what keeps the “excluded” answer (`archive/private/secret.md`, exit 1) distinct from the “escape” answer (exit 4). Matching by `rel_path` rather than by absolute path also keeps a document reached through a symlink addressable by the link's path, which is how M1's walk identifies it. |
| C-7 | `read_skips` is in the DTO, although the phase plan's sketch omitted it. | Phase 3's own test — “the non-UTF-8 fixture still appears only as a read skip and never reaches the chunker” — is only assertable if the report carries the skip. It is also the difference a caller needs: a file that was admitted then failed to decode is a different problem from one the walk excluded. |
| C-8 | The golden redacts exactly one field, the absolute `vault` root of the temporary fixture copy. | The plan said “redacting nothing”, and for ids, spans, ordinals and ordering that holds — the snapshot is a real golden, not a shape check. The corpus root cannot be machine-independent, and redacting more (doc ids, titles) would stop the snapshot noticing a real change (J-7). |
| C-9 | `--text` renders a preview with whitespace collapsed to single spaces and capped at 120 characters plus `…`; human rows are `ordinal id[..8] bytes chars breadcrumb`, with `-` for the preamble's empty breadcrumb, and notes print as `! code: message` under the document. | One chunk then occupies one terminal row, and the cap is **marked**: a silent cut would misreport the text, while an uncapped one lets a 3 000-character fence flood a shell. The `-` matters because an empty column reads like a rendering bug, and the preamble chunk is the only chunk with no heading. |
| C-10 | `chunking` reports the values **as configured in `janus.toml`**, not the clamped values the splitter used. | It answers “what did the config say”, which is what a reader checking a surprising `oversized_block` needs. A clamped run still says so loudly through a note on the document, and inventing a second “effective config” DTO for a case a note already covers would be a contract to version for nothing. |

**Deviations from the Phase 3 plan:** the DTO gained `read_skips` (C-7) and lost the per-chunk `notes` (C-4); the
`vault` field is redacted in the golden (C-8); `NoteDto` moved to `document.rs` (C-2); `Chunk::char_count` and
`count_chars` were extracted (C-5); `PATH` resolution reads a relative path twice (C-6); and the human
“unreadable” line is per file (`unreadable: <rel_path> (<code>)`) instead of `scan`'s per-code counts, because
here the question is which document is missing, not how many. `janus chunks` is not listed in SPEC §9.1 yet —
that, the README status line and D48/D49 belong to phase 4's docs commit.

## 10. Implementation log — Phase 4 (fixtures, goldens, docs)

Settled while building it: six new fixture documents under `notes/` and `specs/`, one extended (`crlf-spec.md`),
the preservation matrix in `tests/frontmatter_preserve.rs`, both goldens regenerated, and the docs updated. The
matrix is a **read** matrix: it asserts the contract M7's patcher will rely on and splices nothing.

| # | Decision | Why |
|---|---|---|
| F-1 | Six fixtures, one per §7.3 shape: `notes/block-sequence.md`, `notes/flow-mapping.md`, `notes/anchor-alias.md`, `notes/multiline-scalar.md`, `notes/quoted-scalars.md`, `specs/body-starts-with-rule.md`. | Each encodes a case the patcher must splice or refuse, so the corpus doubles as the refusal matrix: block sequence and multi-line scalar are `UnsupportedPatch`, flow mapping and quoted scalar are spliceable *around*, anchor/alias must not be inlined, and the `---` body is the F-2 trap that decides where `span.end` is. |
| F-2 | `specs/crlf-spec.md` was extended in place with a **block sequence in CRLF** rather than adding a seventh file. | CRLF was already covered; CRLF *plus indentation* was not, and that combination is what a naive `\n`-splitter gets wrong. Extending the existing file also proves the interaction in the one document that already asserts byte-preserved CRLF. |
| F-3 | The matrix walks the filesystem, not `vault::walk`. | It must cover fixtures the walk excludes (`archive/private/secret.md`, `target/`, `node_modules/`). Exclusion from the corpus says nothing about whether those bytes would survive a patch once admitted, and the excluded files are exactly the ones no other test reads. |
| F-4 | The central assertion is `prefix + raw + body == file`, not field-by-field equality. | That is invariant I1 in one line: if the reader accounts for every byte, a patch that replaces the raw region cannot lose text. Field checks are then only about *where* the boundaries are. |
| F-5 | Three `proptest` properties: arbitrary text, a generated fenced block, and CRLF. All assert reconstruction; only the curated fixtures assert that frontmatter was *recognised*. | Recognition over random bytes is not a property anyone can state without defining "valid YAML", and a flaky generator would spend the shrinking budget on the wrong thing. Reconstruction is the invariant M7 depends on, and it holds for any input. |
| F-6 | Both goldens changed, not just `scan`'s. `golden_chunks__chunks.snap` moved only in `total_chunks` (15 → 21) and gained the new documents: **not one existing chunk id changed**, including `crlf-spec.md`'s, whose frontmatter gained four lines. | Phase 3 did not exist when this phase was planned. The unchanged ids are a free, strong proof of D48/CH-5: editing frontmatter changed the document's `content_hash` and left every chunk id alone, which is exactly the embedding-cache property §11-M4 needs. Worth having in the golden rather than only in a unit test. |
| F-7 | `SPEC.md` got four edits beyond the plan: §6.4's `chunk_id` formula now spells out the NUL-termination and length prefix (K-5), §9.1 lists `scan` as well as the new `chunks` line, and D48/D49 were added to §15. | A spec that disagrees with the code is worse than no comment: the length prefix is the kind of detail a future reader would "simplify" away. `scan` was missing from §9.1's block even though M1 shipped it, so listing only `chunks` would have implied `scan` was never built. |

| F-8 | The fixture document count is now `common::FIXTURE_DOCUMENTS` instead of a literal in each sweep. | Adding six fixtures failed three tests (`fixture_scan`, `chunk_sections`, `chunk_split`) with the same assertion, which is the second time this has happened. A fixture change is supposed to move the goldens and *one* number; three literals in three crates is a maintenance tax with no value, and the constant names the arithmetic (committed + generated − read skip − walk skips) so the next fixture lands in one place. |

**Deviations from the Phase 4 plan:** six new fixtures plus one extension instead of a file per shape (F-1, F-2);
the chunks golden moved too (F-6); SPEC §6.4 and §9.1's `scan` line were corrected on top of the planned §6.5,
§9.1 and §15 edits (F-7); three fixture sweeps needed their document count updated, now through one shared
constant (F-8). No production code changed in this phase — the only non-test, non-doc edits are SPEC and README.

## 11. Handoff to M3

M3 (inverted index, tokenizer, BM25, `index.bin`, `janus index`) inherits:

* **The chunk accessors.** [`Chunk::text`](src/chunk/split.rs) slices the body from the span,
  `Chunk::breadcrumb` and `Chunk::embed_text` produce the §6.5-rule-4 breadcrumb (title-free in the chunk,
  title-prefixed for indexing), and `Chunk::char_count` is the single definition of "length" the report and the
  limit share. Do not re-derive any of them in the index.
* **The pipeline seam.** `chunk::dump::run` is one call away from an indexer: `vault::walk` → `resolve_all` →
  `identity::derive_identity` → `chunk_body` per document. M3 adds persistence after the last step, not a second
  pipeline (§9.5).
* **A stable report shape.** `--json` is `"api": 1` with no timestamps; `NoteDto`/`DocNote::dto()` is the one
  `{code, message}` pair, shared by `scan` and `chunks`. `ReadSkipDto` is still duplicated in each report module:
  M3 is the third occurrence and the right time to lift it into `vault::resolve` (C-2).
* **Content-addressed chunk ids** (D48, K-5): a chunk's id depends on `heading_path` and normalized text only, so
  frontmatter edits, renames and moves re-key nothing. The M4 embedding cache gets this for free — but only if M3
  persists `id` and the span, and never the text (§8.3 rule 1).
* **What is deliberately still missing:** no index, no tokenizer, no string pool, no `index.bin`, no `janus index`,
  no scoring, no `--as-of`, no embeddings, no `read_document`, no patcher, no writes. M2 persisted nothing, so
  `format_version` is still whatever M3 first writes.
* **The fixture corpus is now the golden corpus.** `tests/fixtures/vault/` holds the §7.3 splice shapes and
  `tests/frontmatter_preserve.rs` pins their read-side contract; M7 adds the write side against the same files.
  Adding a fixture changes both goldens and `tests/fixture_scan.rs`'s list, by design (F-6).
