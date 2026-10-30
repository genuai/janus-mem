# M1 — Scan and parse: phased breakdown

> Branch: `m1-scan-parse`. Spec: `SPEC.md` §6.1–§6.4, §6.6.1, §6.12, §8.1–§8.7, §9.1, §10, §11 (M1), §16.1.
> Milestone statement (SPEC §11): “Corpus-local config parsing (`janus.toml` with aliases/types/authority/decay; unknown keys
> tolerated), recursive walk with `ignore`, path confinement, frontmatter isolation (fences, BOM, CRLF), metadata resolution
> hierarchy including `scope`/`shared_with`, `janus scan --json` (no index yet). Workspace-file *shape* is reserved but not implemented.”
>
> **Done when:** the fixture vault parses into the expected `Document` list, including all the ugly cases, and every doc
> reports where its date came from.
>
> **Learn targets (SPEC §12.1):** `PathBuf`/`OsStr`, `&str` vs `String`, lifetimes in parsing functions, iterators,
> `Option` combinators, byte-index slicing and UTF-8 boundary safety, `Cow<str>`. Newtypes: `DocId`, `ChunkId`, `Config`.

## 0. Scope

### In scope
- Corpus-local `janus.toml` parsing per §16.1 (aliases, types, authority, decay, paths, index excludes; unknown keys tolerated).
- Vault-root resolution per §6.1 (ancestor discovery); **workspace mode reserved, not implemented**.
- Recursive walk honoring default ignore rules + config excludes; only `.md`/`.markdown` indexed.
- Path confinement for walk entries and future user-supplied paths (§8.5).
- Frontmatter isolation: fences, BOM, CRLF; YAML parse with comments; fail-open on malformed YAML.
- Metadata resolution hierarchy: type, status, date (+`date_source`), effective_from/to, scope/shared_with, timeless.
- Stateless `DocId` derivation (spec §6.4 ladder) against the scan's own document set.
- New subcommand `janus scan [--json] [--verbose]`. No index is written.

### Explicitly NOT in scope (do not build)
- Workspace config / `--workspace` / `JANUS_WORKSPACE` (§6.11: shape reserved only).
- `janus index`, sidecar, `path_history`, carry-forward identity, moves (M4/M5).
- Chunking (M2), scoring/decay runtime (M3), embeddings (M4), `effective_status`/validity windows (M5), doctor/lineage (M5).
- Any write path (M7). **Scan must never write to the vault.**
- Async, rayon, mmap, any `unsafe`, network, shelling out.

## 1. Dependency changes (Cargo.toml)

Add (versions are current at the time of writing; verify with `cargo search`):

| Crate | Version | Why (one-line justification per §8.2 policy) |
|---|---|---|
| `serde` + `serde_json` | 1.x | DTOs for `scan --json`; config deserialize |
| `serde` `derive` feature | — | derives on Config/DTOS |
| `toml` | 1.x | corpus config parsing (§16.1) |
| `serde_yaml_ng` | 0.10 | YAML frontmatter READ. **Note:** spec's stack says `serde_yaml`, which is archived/deprecated; `serde_yml` is unmaintained per crates.io. `serde_yaml_ng` is the maintained fork. If it vanishes, fall back to a hand-rolled subset parser for the frontmatter keys — do not add a heavy YAML 1.2 crate. |
| `chrono` | 0.4 | `NaiveDate` for dates (no timezone juggling in M1) |
| `ignore` | 0.4 | walk with .gitignore semantics (§6.1, §8.2) |
| `globset` | 0.4 | `[index] exclude` + `[paths]` pattern matching |
| `blake3` | 1.x | content hash and DocId/chunk-id derivation (§6.2, §8.3.1) |
| `dunce` | 1.0 | canonicalize without Windows `\\?\` verbatim prefixes (path confinement on Windows, §8.7) |
| dev: `insta` | 1.x | golden tests over fixture vault (§8.2, §10) |

Keep: `clap`, `thiserror`, `tracing`, `tracing-subscriber`, `assert_cmd`, `predicates`, `tempfile`.

## 2. Data model (write these first — “design before code” §12.3.3)

```rust
// src/id.rs  (minor addition to the §8.1 layout; keeps parser files clean)
pub struct DocId([u8; 16]);      // blake3(match_key)[..16]; Eq/Hash/Ord/Clone/Copy; Display = hex
pub struct Blake3Hash([u8; 32]); // full-file content hash; Display = hex
pub struct ChunkId([u8; 32]);    // defined now (learning plan M1), used in M2; content-addressed, no doc id in it

// src/vault/document.rs
pub struct Document {
    pub id: DocId,
    pub id_source: IdentityTier,     // which §6.4 key won
    pub path: PathBuf,               // absolute, canonical
    pub rel_path: String,            // vault-relative, '/'-separated (cross-platform rule §8.7)
    pub content_hash: Blake3Hash,    // of full file bytes
    pub frontmatter: Frontmatter,    // typed view over the aliased key subset (Option values)
    pub raw_frontmatter: Option<String>, // verbatim block incl. fences (decision D-FM-3 below)
    pub body: String,                // verbatim markdown after the closing fence
    pub span: FrontmatterSpan,       // byte offsets of the frontmatter region (serves M7)
    pub mtime: SystemTime,
    pub size: u64,
    // ── resolved metadata (hierarchy applied) ──
    pub title: String,               // frontmatter → filename stem in M1 (H1 arrives with pulldown-cmark in M2)
    pub type_: TypeSpec,             // canonical type + weight/timeless/half-life resolved at config load (§6.6.1)
    pub status: Status,              // Active | Draft | Superseded | Deprecated | Unknown
    pub date: Option<NaiveDate>,
    pub date_source: DateSource,     // Frontmatter | Filename | Mtime | None
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub scope: String,               // normalized; "global" fallback (§6.12 rule 1)
    pub notes: Vec<DocNote>,         // warnings: unknown status/type, invalid scope, broken YAML, …
}

pub struct FrontmatterSpan { pub start: usize, pub end: usize } // byte offsets into the FILE

pub struct Frontmatter { /* Option<_>/Vec<_> per §6.3 canonical keys:
    id, author, title, type, status, date, effective_from, effective_to,
    supersedes: Vec<String>, superseded_by: Vec<String>, doc_version,
    schema_version: Option<u32>, tags: Vec<String>, timeless: Option<bool>,
    scope: Option<String>, shared_with: Vec<String> */ }
```

**Design rules.** `Frontmatter` holds the *typed view over a subset of keys* (raw `Option` values, straight from YAML —
nothing inferred). Resolution (type/status/date/scope/title/timeless) happens in a separate `resolve()` step the produces
the Document-level fields above. `Document.notes` collects every fail-open warning. Never panic on a weird value — fail
open, warn, fall back.

Newtypes per the M1 learn plan: `DocId`, `ChunkId`, plus a `Config` newtype is overkill — `Config` is a struct.

## 3. Decisions to settle in the 20-minute design conversation (defaults chosen)

| # | Question | Default | Where it lands |
|---|---|---|---|
| D-FM-1 | `raw_frontmatter` contents | Full verbatim block **including** `---` fences; `span` = byte range of that block incl. trailing newline when present. M7 will refine exact splice semantics. | vault/frontmatter.rs |
| D-FM-2 | No closing `---` fence found | Treat as **no frontmatter** (whole file = body), note it. Never error, never guess. | vault/frontmatter.rs |
| D-FM-3 | Duplicate YAML keys | YAML dup-keys = malformed → typed view `None`, note `yaml_duplicate_key`. (serde_yaml_ng errors on dupes; catch and fall back.) | vault/frontmatter.rs |
| D-FM-4 | Frontmatter key case | Match aliases case-sensitively (YAML keys are case-sensitive). No lowercasing. | vault/frontmatter.rs |
| D-FM-5 | Date parsing | `%Y-%m-%d`, then ISO-8601 datetime prefix (`%Y-%m-%dT…`), else invalid → absent + note. | vault/document.rs |
| D-FM-6 | `[paths]` pattern semantics | Treated as gitignore-style globs over the '/' rel_path via `globset` (same engine as `[index] exclude`). `"decisions/"` matches any path containing that segment sequence. First match wins; only consulted when frontmatter `type` is absent. | config.rs |
| D-FM-7 | Case-insensitive extension filter | Accept `.MD`/`.MARKDOWN` too (Windows/macOS filesystems are case-insensitive). | vault/mod.rs |
| D-ID-1 | Identity in M1 | **Stateless derivation only**: `DocId = blake3(match_key)[..16]`, first key that is *unique across the scan's own doc set* wins (spec §6.4 ladder, including the “identical copies fall through to rel_path” rule). Carry-forward, `path_history`, `janus ids` are M5. Safe because nothing is persisted until M4. | vault/document.rs; two-pass scan |
| D-ID-2 | Tiers 3–5 namespacing | `corpus_id ‖ scope ‖ title ‖ date` per §6.4. `corpus_id` = config `corpus_id` (default `"main"`). | vault/document.rs |
| D-WALK-1 | Skip attribution in `scan` summary | `WalkBuilder` with `.hidden(false)` so *we* classify hidden entries; two categories (`hidden`, `gitignore`) reported separately; `gitignore` count = total-md − seen-md − attributed-skips (one cheap second enumeration, no file reads). Simpler fallback: merge into `hidden_or_gitignored`. | vault/mod.rs |
| D-WALK-2 | Symlinks | Never follow dir symlinks (`follow_links(false)`, default). File symlinks: canonicalize target; inside root → index, outside → skip + warn (`symlink_escape`). | vault/mod.rs |
| D-WALK-3 | Non-UTF-8 path | Skip + warn. Cannot be represented as `rel_path: String`. (Effectively never happens on macOS/Windows.) | vault/mod.rs |
| D-CFG-1 | Missing `janus.toml` but `.janus/` present | Error (`corpus config missing; run janus init`) with exit 1. Config is required to know conventions. | config.rs |
| D-CFG-2 | `janus init` full config? | **Yes** — M1 updates init to emit the full §16.1 example config (aliases become discoverable) while keeping `create_new` no-overwrite semantics. M0's minimal file must remain parseable (compat test). | config.rs |
| D-CFG-3 | `[index] exclude` matching | `globset` over '/' rel_path. `.janus/` and `.git/` always excluded regardless of config (§6.1). Exclude globs win over everything. | config.rs + vault/mod.rs |
| D-CFG-4 | Alias → canonical cycles in `[types]` | Resolve transitively with a cycle guard; on cycle → fall back to raw string + note. | config.rs |
| D-OUT-1 | `--workspace`/`JANUS_WORKSPACE` | Absent from CLI in M1. `Config` module doc reserves the shape (corpus id/role, index_dir, search policy per §16.2) in a comment only. | config.rs |
| D-SC-1 | Is path confinement only about filesystem escape? | Also reject paths that resolve INTO `.git/`/`.janus/` (defense in depth, §8.6 rule 1). | vault/mod.rs (`confine`) |

Record each settled decision in `docs/notes/01-m1.md` per §12.3.3 (≈10 honest lines, including “what would I do differently”).

---

## Phase 1 — Config parsing (½ day)

**Goal:** `Config::load(root)` turns `<root>/janus.toml` into the full typed corpus config with §16.1 defaults, unknown keys
tolerated, and no behavior beyond parsing.

**Files:** `src/config.rs` (extend), `src/error.rs`, `Cargo.toml`.

**Build:**
1. Serde structs mirroring §16.1 tables: `index_dir`, `[frontmatter]` (alias lists per canonical key, §16.1 defaults when absent),
   `[defaults]` (type `note`, status `active`), `[types]` (alias→canonical), `[authority]` (canonical→weight),
   `[decay]` (global `half_life_days`, `timeless_types`), `[decay.by_type]`, `[paths]`, `[index]` (exclude globs),
   `[chunking]`/`[embedding]`/`[mcp]` (parse-and-store only; consumed in M2/M4/M6). `corpus_id` with default `"main"`.
   - No `#[serde(deny_unknown_fields)]` anywhere — unknown keys are tolerated by construction.
   - `#[serde(default)]` on every table so a minimal `index_dir = ".janus"` file (M0) deserializes unchanged.
2. `TypeSpec { authority: f64, timeless: bool, half_life_days: f64 }` + `Config::resolve_type(&self, raw_or_canonical: &str) -> TypeSpec`:
   alias → canonical (transitive, cycle-guarded) → authority (unknown canonical ⇒ 0.80 + note) → timeless
   (frontmatter wins per-doc; else `timeless_types` membership; unknown ⇒ global default) → half-life (`[decay.by_type]` else global).
3. `Config::validate()`: warn (not error) on non-positive authority weights, non-positive half-life (≤0 means timeless, §7.2),
   empty alias lists, resolve `index_dir` against the vault root later (M4) — M1 only stores it.
4. Errors: `JanusError::ConfigRead/ConfigParse { path, source }` → exit 1. Config missing → `ConfigMissing` → exit 1 (D-CFG-1).
5. Update `janus init` to write the full §16.1 config (D-CFG-2), keeping `create_new`. Keep M0's output parseable forever (compat test).

**Tests (unit, table-driven where possible):**
- Full §16.1 config deserializes; defaults applied when keys absent; unknown top-level and nested keys ignored.
- M0 minimal config still parses; defaults come out.
- Alias first-match-wins ordering (both `date` and `created` present ⇒ first wins).
- Type registry: `adr → decision → spec{1.5, timeless}`, unknown `runbook → 0.80 + note`, alias cycle `a→b→a` falls back safely.
- `[paths]` glob matching: `decisions/`, `**/*.draft.md`, `research/` select correct types; no match ⇒ `note`.
- `janus init` emits §16.1 shape; double-init still refused (existing test must keep passing).
- Invalid values (negative weight, half-life 0) parse with warnings, no panic.

**Handoff to another AI (paste):** “Implement Phase 1 (config parsing) of M1 for janus-mem per
`docs/m1-breakdown.md` Phase 1. Read `SPEC.md` §6.6.1, §16.1, §11-M1, and `src/config.rs` first. Add serde/toml/serde_yaml_ng
deps with one-line justifications. All structs `#[derive(…)]` + `#[serde(default)]`; no `deny_unknown_fields`; no `unwrap()`
in lib code; `cargo clippy -- -D warnings` must pass; tests first for anything with a rule. Design conversation output goes
to `docs/notes/01-m1.md`.”

**Accept:** `cargo test` green; existing `tests/cli_init.rs` untouched and passing.

---

## Phase 2 — Vault root resolution + walk + confinement (¾ day)

**Goal:** resolve the corpus root per §6.1, walk it with ignore rules + config excludes, enforce path confinement, produce
per-file skip attribution.

**Files:** `src/vault/mod.rs` (new), `src/config.rs`, `src/error.rs`, `src/main.rs`.

**Build:**
1. `resolve_vault` upgrade (§6.1): explicit `--vault` (M0 behavior, keep) → `JANUS_VAULT` (clap already reads it) → nearest
   ancestor containing `janus.toml` **or** `.janus/` → error with hint. Canonicalize via `dunce::canonicalize` (D-*: Windows
   `\\?\` prefixes must not break `starts_with`). Keep `VaultNotDirectory`, `VaultResolve`.
2. `confine(root: &Path, candidate: &Path) -> Result<PathBuf, JanusError>` — canonicalize, require `starts_with(root)`,
   refuse `.git/`/`.janus/` presence anywhere in the canonical components (D-SC-1). Unit-test this hard; the write/read
   tools of M6/M7 will reuse it. New error `PathEscape { path }` → **exit code 4** (refused, §9.1).
3. `scan_vault(root, config) -> ScanReport`:
   - `ignore::WalkBuilder::new(root)`: `.hidden(false)` (we attribute hidden: D-WALK-1), `.git_ignore(true)`+global+exclude,
     `.follow_links(false)`, `.parents(true)`.
   - `filter_entry` rule ladder, counting per rule: always-excluded (`.janus`, `.git`, `target`, `node_modules`, `.obsidian`),
     hidden (dot-prefix on any component), config-exclude globs (`globset` over '/' rel_path), extension filter
     (`.md`/`.markdown`, case-insensitive, D-FM-7).
   - For each admitted file: `confine` re-check, symlink-target check (D-WALK-2), then hand to Phase 3's reader.
   - Read-consistency helper (§6.9): read bytes; if `bytes.len() != metadata.size()`, retry, max 3; exhausted ⇒ skip + warn
     (`changed_during_read`). Never trust mtime alone.
   - Non-UTF-8 content ⇒ skip + warn (`non_utf8_content`); non-UTF-8 path ⇒ skip + warn (D-WALK-3); unreadable (permissions/
     transient IO) ⇒ skip + warn (`unreadable`), not a fatal error.
   - `ScanReport` carries per-rule skip counts + the doc list (filled in Phase 3/4).
4. Wire `janus scan` in `src/cli.rs` (add `Command::Scan` with `--json`, `--verbose`) — keep M0 `init` working.

**Tests:**
- Ancestor discovery: root, nested dir, `JANUS_VAULT` env, explicit path; “nothing found” error mentions the hint.
- Walk: fixture tree with `.git/`, `target/`, `.obsidian/`, hidden `.private/x.md`, gitignored `draft.md`, `archive/private/**`,
  `**/*.draft.md`, a `.txt`, a `.MD` (the 2 MB doc), a non-UTF-8 `.md` byte file, symlink-inside, symlink→/etc/passwd.
  Assert: which files were indexed, which skipped, and the skip counts per rule.
- `confine`: outside-root paths, symlink escapes, `.git/` internals, Windows-verbatim-style prefixes (simulate `\\?\` prefix
  by calling `dunce` on a fake path).
- Read-consistency retry: fake reader returning short first, full second; exhaustion path skips + warns.
- Cross-OS: run locally on macOS; CI already runs Linux + soft Windows `cargo check` (must not break).

**Handoff to another AI:** (same pattern as Phase 1, referencing SPEC §6.1, §8.5, §8.6, §8.7 and this doc's Phase 2 +
D-WALK-*. “No `unsafe`; no `std::os::unix`/`windows` without `#[cfg]`; use `ignore` for the walk; keep `.janus/`/`.git/`
always-excluded regardless of config.”)

**Accept:** fixture-tree test passes; `scan` prints a summary with counts; no writes to the vault (assert tree unchanged).

---

## Phase 3 — Frontmatter isolation + metadata resolution (1 day)

**Goal:** per admitted file, isolate frontmatter (fences/BOM/CRLF), parse YAML fail-open, resolve the metadata hierarchy
(§6.3) incl. `scope`/`shared_with` (§6.12), and derive a stateless `DocId` (§6.4).

**Files:** `src/vault/frontmatter.rs` (new), `src/vault/document.rs` (new), `src/id.rs` (new).

**Build:**
1. `extract_frontmatter(bytes: &[u8]) -> FrontmatterIsolation { raw, body_range, span, has_bom }`:
   - Strip a single leading UTF-8 BOM (`EF BB BF`) **for detection only** — never for storage; hash covers raw bytes.
   - Frontmatter exists iff the file starts (after BOM) with a line matching `^---\s*(\r?\n|$)` — accept `---`, trailing
     spaces, CRLF; LF or CRLF line endings preserved in output.
   - Search for the closing line `^---\s*$` (byte-wise, line starts only ⇒ slicing can never split a UTF-8 sequence; the
     fence is pure ASCII). No closing fence ⇒ no frontmatter + note (D-FM-2).
   - Fence caveats: a `---` line *inside* a YAML block string or a doc whose body begins with `---` for other reasons —
     frontmatter only ever starts at offset 0 (first line after BOM), so body-embedded fences are irrelevant.
2. YAML parse of the inner text via `serde_yaml_ng::from_str::<serde_yaml_ng::Mapping>` (or `Value`): comments natively OK.
   Fail-open: mapping that is not a string-keyed map, duplicate keys (D-FM-3), or syntax error ⇒ typed view `None` + note
   `yaml_unparseable`, raw block retained so M7 could still patch later. Unknown keys inside frontmatter are ignored
   (the contract is “reads a subset, preserves the rest”, §6.3).
3. Alias mapping (first match wins, config-driven): map raw values into `Frontmatter` typed view. Scalars coerced with
   tolerance (`id: 123` ⇒ `"123"`), lists must be lists (`tags` as scalar ⇒ single-element or note), `timeless` bool coerce.
4. Metadata resolution (§6.3) — the teaching moment for `Option` combinators; each rule emits a note when it falls back:
   - **title:** frontmatter `title` else filename stem (H1 deferred to M2 — see §0 note; identity is not persisted before M4 so
     the later H1 upgrade cannot break anything).
   - **type:** frontmatter type (alias→canonical) else `[paths]` hint else `[defaults]`. Unknown canonical ⇒ keep name +
     `TypeSpec{0.80, …}` + note (§6.6.1).
   - **status:** frontmatter (aliases) else `[defaults]` (`active`). Unknown values ⇒ `Status::Unknown` + note (never fail).
   - **date:** frontmatter date-alias keys (first match wins) → filename `YYYY-MM-DD` prefix (stem starts with the date; next
     char is `-`/`_`/`.`) → `mtime`. Record `date_source: Frontmatter|Filename|Mtime`. All dates `NaiveDate`; unparseable
     frontmatter date ⇒ treated absent + note; no date at all ⇒ `None` + `DateSource::None` (+`no_validity` semantics land M5).
   - **effective_from/to:** parse only if present; **never** fall back to mtime; no derivation in M1.
   - **scope:** normalize (trim, lowercase, collapse internal whitespace); validate `[a-z0-9][a-z0-9._:-]*`; invalid ⇒
     `"global"` + note (fail-open, §6.12 rule 1/9). **shared_with:** set semantics — dedupe, keep document order; never
     enters identity.
   - **timeless:** frontmatter bool else `decay.timeless_types` membership of the canonical type else false.
5. `derive_identity(&mut [DocMeta]) -> Vec<IdentityOutcome>` — two-pass over the scan's doc set (D-ID-1): pass 1 parses all;
   pass 2 computes, per doc, the first ladder key unique across the set: declared `id` → content_hash → `corpus‖scope‖title‖date`
   → title+doc_version → unique title → rel_path. Return `id` + `IdentityTier` + `duplicate_identity` notes. (Two passes over
   ~20k parsed docs is fine; it is exactly the "iterators" lesson. Do this in memory; no sidecar.)
6. `Document::from_raw(...)` assembles §6.2 struct + resolved fields + spans. Byte slicing at ASCII fence boundaries is
   necessarily UTF-8-safe; add a debug_assert `is_char_boundary` as a guard, and an explicit test with multi-byte content
   (e.g. CJK/emoji around the fences).

**Tests:** (table-driven; this is the “test-first for anything with a rule” zone §12.3.4)
- Isolation matrix: no frontmatter, BOM+CRLF, LF, `--- ` with trailing space, unclosed fence, YAML with comments, dup keys,
  non-map YAML, YAML with nested flow structures (should be ignored/preserved, not crash), body with `---` lines, emoji/CJK
  around the closing fence (byte-boundary safety), CRLF preserved byte-for-byte in `raw`/`body`.
- Resolution matrix: every type/status/date/scope branch, incl. unknown status/type, invalid scope, alias order, filename
  date `2024-11-02-postgres.md`, `2024_11_02.md` (no match), mtime fallback, `created` via alias before filename (D-FM-6
  ambiguity note: with default aliases, frontmatter `created` wins over filename prefix).
- Identity: same title+date twice ⇒ both fall to rel_path + `duplicate_identity`; identical content at two paths ⇒ rel_path;
  declared id unique ⇒ tier 1; title+date collision across scopes ⇒ distinct ids; doc with neither date nor version/unique title
  ⇒ rel_path; `scope` change re-keys (different DocId) but `shared_with` change does not (§6.12 obligations 4–6, 9).
- Frontmatter golden: byte-preservation of raw/body for every fixture.

**Handoff to another AI:** (paste Phase 3 + D-FM-*, D-ID-* + SPEC §6.2–§6.4, §6.12, “no `unwrap` except provable
invariants; keep raw bytes and spans; fail-open everywhere on read; every fallback writes a `DocNote`.”)

**Accept:** fixture-vault goldens (Phase 5) parse; every doc's `date_source` is reported; identity deterministic across runs.

---

## Phase 4 — `janus scan` command + JSON DTO (½ day)

**Goal:** `janus scan [--json] [--verbose]` prints a human table or stable JSON; exit codes per §9.1.

**Files:** `src/scan.rs` (new: CLI handler + DTOs), `src/cli.rs`, `src/main.rs`.

**Build:**
1. `ScanReport` JSON (versioned `"api": 1`; **never include `body` text** — privacy §8.5, size §13): vault, corpus_id,
   config path, taken_ms, `docs[]` (id, id_source, path, rel_path, title, type, status, date + date_source,
   effective_from/to, scope, shared_with, tags, author, doc_version, schema_version, supersedes/superseded_by,
   timeless, content_hash hex, size, mtime UTC ISO, has_frontmatter, notes[]) and skip per-rule counts +
   (only with `--verbose`) skipped paths with reasons.
2. Human output: compact aligned table (path, type, status, date+source), then a one-line skip summary.
3. Exit codes: `0` ok; `1` runtime (config missing, IO); `4` refused (path escape); usage errors → `2` via clap. Errors print
   to stderr with `error: ...` prefix (M0 convention) and never panic.
4. `main.rs` stays thin: match `Command::Scan => scan::run(...)`.

**Tests:** `assert_cmd` integration: JSON parses (`Value`), `"api": 1`, doc count matches fixture expectation, every doc has
`date_source`, `--json` is table-free (stdout parses as JSON), skipped non-UTF-8 file present in summary; human output is
non-empty; unknown flag ⇒ exit 2; `--vault` points outside nothing weird happens. `insta` snapshot of the JSON over the
fixture vault.

**Handoff to another AI:** (paste Phase 4 + SPEC §9.1 exit codes + fixture list §10 + “stdout: results only; all logs/tracing
to stderr (MCP rule, §8.2); DTOs live in `src/scan.rs`, stable and versioned.”)

**Accept:** `janus scan --json | python3 -m json.tool` works; `janus scan` on the real repo's docs dir (spec + notes) runs
without panicking.

---

## Phase 5 — Fixture vault, goldens, docs, polish (½–1 day)

**Goal:** committed ugly fixture vault (§10), golden tests, CI green, `docs/notes/01-m1.md`, clean clippy.

**Build:**
1. `tests/fixtures/vault/` (git-tracked, §10 requirements exactly): mixed CRLF/LF file(s), a BOM file, no-frontmatter doc,
   YAML-with-comments doc, supersession **cycle** + **broken link** docs (parsed/echoed only; link validation is M5 — add the
   files now so goldens are stable), a ~2 MB doc (repeat a paragraph; write it with a small script, keep it committed), a
   non-UTF-8 `.md` (raw bytes via `git add`ed binary or generated in a build/test script — **recommend generation in
   `tests/common/mod.rs` to keep the diff sane**), `.gitignore` entries (`target/`), an `archive/private/**` + `**/*.draft.md`
   exclusion pair, filenames with spaces and unicode, case-only differently named files (for later).
2. Golden tests with `insta` over `scan --json` (fixture should stay boring and stable).
3. Config example under `tests/fixtures/` exercising every §16.1 path (aliases override, unknown keys present).
4. Polish gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (incl. `clippy::unwrap_used` warn per §10),
   `cargo test --all-targets`, `cargo doc` with `#![warn(missing_docs)]` on public items (SPEC §10), Windows
   `cargo check --target x86_64-pc-windows-msvc` (soft CI job exists — keep it green; install target via rustup if local).
5. `docs/notes/01-m1.md` (§12.3.3: what was built, what confused, what now understood, what would be done differently).
6. Update `README.md` if it claims “no code yet” (it does — M1 changes that).

**Accept:** all gates green; a fresh `janus init && janus scan --json` on the fixture vault prints the expected doc list.

---

## 4. Cross-cutting requirements (checked at the end, and by the follow-up code review)

- **Cross-platform (§8.7).** Paths only via `Path`/`PathBuf`; `rel_path` serialized with `/`; canonicalize via `dunce`;
  `dunce::canonicalize` + component-wise `starts_with` for confinement (no raw string prefix compares); no
  `std::os::unix|windows` without `#[cfg]`; CRLF/BOM preserved; no shelling out; `\n` newlines for JSON/config output only.
- **Security (§8.5).** Confinement enforced at walk AND asserted per admitted file; symlink escapes rejected; `.git/`+`.janus/`
  excluded unconditionally (not config-restorable); no content in logs at `info` (paths only); scan never writes.
- **Performance (§13, guardrails only).** One read per file (reader returns `(bytes, size, mtime)` in one syscall bundle —
  `File::metadata` + `read_to_end`… acceptable to do two syscalls; do not optimize yet); no O(n²) in identity derivation
  (build `HashMap<Key, Vec<idx>>` per tier instead of nested scans); two-pass parse; strings sliced from the single buffer via
  byte spans (`&str` lessons — prefer borrowing, own only `body`/`raw` because the patcher needs stable handles).
- **Determinism.** Same tree ⇒ identical `scan --json` modulo `mtime`/`took_ms`; sorted doc order by `rel_path`; scope
  normalization canonical; `BTreeMap`/sorted iteration where order can leak into JSON.
- **Error model.** Every new `JanusError` variant carries the offending path; exit-code mapping in one place; add
  `PathEscape → 4`, config errors → 1.

## 5. Definition of done for the whole milestone

1. Fixture vault parses into the expected `Document` list — all ugly cases included.
2. Every doc reports `date_source`.
3. `janus scan --json` is deterministic, versioned, and body-free.
4. No index written; no vault mutation; no `unwrap`/`expect` in lib code; clippy `-D warnings` clean on macOS + Linux CI;
   Windows `cargo check` still compiles.
5. `docs/notes/01-m1.md` exists and is honest.
6. Suggested commit sequence: `P1 config`, `P2 walk+confinement`, `P3 frontmatter+resolution`, `P4 scan CLI`,
   `P5 fixtures+goldens+docs` — each with a real message, each green.
