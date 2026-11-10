# M7 — Safe writes

> Branch: `m7-plan` (this document), then one branch per phase from `main`.
> Spec: §7.3, §7.4, §9.1, §9.3, §9.4, §8.3, §11 (M7), §15 (D18, D25, D38, D42).
> Evidence: the code at `6b068ef` (v0.2.1), and the gate of 2026-11-10 on two corpora.
>
> **Done when:** one call retires the Postgres ADR and records the SQLite decision (W4);
> invariants I1–I7 each have a named test; a 200-file dry run on a copy of the large corpus
> reports zero unexpected byte changes; and a write is impossible without `--allow-write` on
> either server surface.

**Words used here.** **Patch**: one request to change frontmatter keys. **Splice**: replace
one byte range and copy everything else. **Etag**: the `content_hash` a read tool returned.
**Receipt**: the answer to a write. **Rehearsal**: running every write path on a copy of a
real corpus. **Refuse**: answer `refused` and write nothing.

## 0. Scope

**Decision: M7 adds the writer. It adds no retrieval change, and it moves no ranking number.**

A defect in a reader costs an answer. A defect in a writer costs a file. So the milestone is
built from the smallest parts that can be reviewed alone, and the safety net is a named gate
before the first line of write code.

| M7 item | state today | evidence |
|---|---|---|
| a write module | absent | `src/` has no `write` module; 16 CLI commands, none of them `set`, `write`, `supersede`, `promote` (`src/cli.rs`) |
| frontmatter isolation, byte-exact | **exists** | `FrontmatterRead` and `FrontmatterSpan` (`src/vault/frontmatter.rs:47`, `src/document.rs:95`) |
| a byte span per frontmatter key | absent | `grep -rn KeySpan src/` returns nothing |
| a splice | absent | `grep -rn splice src/` returns comments only (`src/document.rs:92`) |
| atomic replace of a document | absent | the only temp+rename is the index publish (`src/index/store.rs:723`, `:751`) |
| an advisory corpus lock | **exists** | `Lock::acquire`, `.janus/lock`, 10 s timeout (`src/index/store.rs:452`, `:46`, `:49`) |
| etag on every read row | **exists** | `content_hash` from the bytes read (`src/read.rs:42`, `:83`) |
| `expected_hash` enforcement | absent | no `conflict` and no `precondition_required` code (`src/api/error.rs`); exit 3 is unreachable (`src/error.rs:230`) |
| the four write tools | named, then refused | `WRITE_TOOLS`, `write_refusal` (`src/api/mod.rs:115`, `:131`) |
| `--allow-write` as a gate | parsed, inert | `let _ = allow_write;` (`src/api/mod.rs:134`) |
| `writes_enabled` | hardcoded `false` | `src/status.rs:170` |
| `[mcp] allow_write` | parsed, unused | `src/config.rs:717` |
| the MCP `instructions` sentence | says "This release reads. It does not write." | `src/mcp/protocol.rs:30` |
| config in the index freshness signal | absent | `corpus_sig` covers file content only (`src/index/refresh.rs:149`); measured 2026-11-10 |

### 0.1 In scope

1. A span-preserving frontmatter patcher: key spans, splice, refusal, value round-trip.
2. Atomic replace: temp file, `fsync`, `rename`, parent `fsync`, mode preserved.
3. The corpus lock around the read-verify-write sequence.
4. Optimistic concurrency on `expected_hash`, on every surface.
5. `update_frontmatter`, `write_document`, `supersede_document`, `promote_document` over
   MCP and `janus rpc`, and `janus set`, `janus write`, `janus supersede`, `janus promote`.
6. Receipts, and two new public error codes.
7. `--allow-write` as a real gate, and `writes_enabled` as a real capability flag.
8. The config hash in the manifest, because the writer resolves type from config and the
   index caches it (gate defect, 2026-11-10).
9. The rehearsal: every write path on a copy of a real corpus, plus the 200-file dry run.

### 0.2 Not in scope

| Item | Owner | Why |
|---|---|---|
| Any ranking or retrieval change | R2 | the gate measured recall 10/10 on this repository and 9/10 keyword recall on the large corpus; the gap is ordering, and a writer cannot move an order |
| `--bump-version` | later, additive | frontmatter versions belong to the user (AGENTS.md, D26) |
| Deleting a frontmatter key | later, additive | no workflow asks for it, and the receipt needs a `removed_keys` field first |
| Merging `deprecated` with tombstone derivation | later | open question 21 changes status semantics; a writer must not decide it |
| `--git-history`, `watch`, mmap | M8 | unchanged |
| A vault-wide multi-file transaction | never in v1 | §5.1: one writer, one file, one lock |
| Editing a `.md` file the request did not name | never | §5.1 rule 4 |

### 0.3 Why eight phases

| Phase | Unit | Why it is one reviewable change |
|---|---|---|
| M7-0 | the safety net | no code: a backup restored once, and the default proved |
| M7-1 | config is part of the index identity | one hash, one comparison, one report field; unblocks the writer |
| M7-2 | the patcher, pure | no IO, no lock, no surface; goldens and a property test |
| M7-3 | atomic replace and the lock | the only phase that touches bytes on disk |
| M7-4 | `update_frontmatter` and `set` | the first write tool, the gate, the receipt, the error codes |
| M7-5 | `write_document` and `write` | whole-file create and overwrite |
| M7-6 | `supersede` | two files, one transaction, one rollback |
| M7-7 | `promote` | cross-corpus, and it never touches the archive |
| M7-8 | the rehearsal | measured, on a copy, with a repeatable command |

## 1. Dependencies

None. The patcher uses `oneyaml`, already a dependency for frontmatter reads
(`src/vault/frontmatter.rs:32`). No new crate is added. If a span-preserving YAML crate is
adopted instead, it needs a one-line justification in `Cargo.toml` (§8.2) and a decision row.

## 2. Data model

### 2.1 The key span (`src/write/scan.rs`, new)

```rust
pub struct KeySpan {
    pub key: String,          // unquoted, as the config reads it
    pub line: usize,          // 1-based, for the refusal message
    pub value: ValueSpan,     // where the value's bytes are
    pub shape: ValueShape,    // Scalar | FlowSequence | Unsupported(&'static str)
}

pub struct ValueSpan {
    pub start: usize,         // first byte of the value
    pub end: usize,           // one past the last byte of the value
    pub comment: Option<usize>, // where a trailing comment starts, if any
}
```

`ValueShape` is a closed list. Anything outside it is `Unsupported` with a reason string:
`BlockSequence`, `BlockMapping`, `FlowMapping`, `MultilineScalar`, `Anchor`, `Alias`,
`DuplicateKey`, `NoValue`, `Nested`.

### 2.2 The patch request and the plan

```rust
pub struct Patch {
    pub path: PathBuf,                    // resolved, inside the corpus
    pub keys: Vec<(String, Option<String>)>, // sorted before use; None writes `null`
    pub expected_hash: Option<String>,
    pub create_frontmatter: bool,
    pub dry_run: bool,
}

pub struct Plan {
    pub bytes_before: usize,
    pub bytes_after: usize,
    pub old_hash: String,
    pub new_hash: String,
    pub text: String,                     // the whole new file
    pub changed_keys: Vec<String>,        // sorted
    pub appended_keys: Vec<String>,       // sorted; a subset of changed_keys
}
```

`Plan` is pure. It takes the current bytes and the patch, and returns the new bytes or a
refusal. No IO happens after the read.

### 2.3 The receipt (`src/api/dto.rs`)

```json
{ "api": 1, "ok": true,
  "path": "decisions/2026-10-29-postgres.md",
  "changed_keys": ["status", "superseded_by"],
  "appended_keys": ["superseded_by"],
  "old_hash": "…", "new_hash": "…",
  "dry_run": false,
  "bytes_before": 412, "bytes_after": 470 }
```

`supersede` and `promote` add `receipts`: the receipts of every file they touched, in the
order they were written.

### 2.4 Error variants and codes

| Variant | Code | Exit | When |
|---|---|---|---|
| `Conflict { path, expected, current }` | `conflict` | 3 | the etag does not match the bytes on disk |
| `PreconditionRequired { path, preview }` | `precondition_required` | 3 | a write on an existing file carried no etag |
| `UnsupportedPatch { path, key, reason }` | `unsupported_patch` | 4 | the key's shape is outside the closed list |
| `NoFrontmatter { path }` | `no_frontmatter` | 4 | no block, and `create_frontmatter` is false |
| `ReadOnlyCorpus { corpus }` | `read_only_corpus` | 4 | the corpus is archived or `read_only` |
| `WriteDisabled { surface }` | `write_disabled` | 4 | the surface was started without `--allow-write` |

Exit 3 is used for the first time. Both new exit-3 codes mean one thing: re-read, then retry.
A write that fails a shape check is a refusal (exit 4), because a retry without a change of
request cannot succeed.

### 2.5 The manifest (`src/index/store.rs`)

`Manifest` gains one field, on the pattern `corpus_sig` already set (`:107`–`:114`):

```rust
/// blake3 of the config sections that feed cached document rows (M7-1).
#[serde(default)]
pub config_hash: Option<String>,
```

The hash covers `[frontmatter]`, `[defaults]`, `[types]`, `[paths]` and `[links]`. Those are
the sections whose value the index caches per document. `[authority]` and `[decay]` are read
at query time, so a change to either needs no rebuild (measured 2026-11-10). `[chunking]`
already has a fingerprint. `[index] exclude` changes the admitted set, which `corpus_sig`
covers.

## 3. Contracts

### 3.1 The patcher (P2)

**Statement.** A patch replaces the smallest byte range that holds the value. Every other
byte of the file is copied unchanged. A shape the scanner does not understand is refused, and
the file is not touched.

**Case 1.1 (I1).** `status: active` → `superseded`. The output equals the input with
`active` replaced by `superseded`, and nothing else. A golden test holds the whole file.

**Case 1.2 (I2).** The same patch on a CRLF file keeps every CRLF, and the appended line uses
CRLF. A test asserts the byte length difference is exactly the value length difference.

**Case 1.3 (I2).** A file with a UTF-8 BOM keeps the BOM at byte 0, before the opening fence.

**Case 1.4.** `status: active  # current` → `status: superseded  # current`. The comment and
the two spaces survive, because the value span ends at the comment.

**Case 1.5 (I3).** `tags:` with a block sequence is refused with `unsupported_patch`,
`key: "tags"`, and the input is returned unchanged.

**Case 1.6.** A multi-line scalar (`summary: |`), a flow mapping (`owner: {a: b}`), an anchor
(`x: &a 1`) and an alias (`y: *a`) are each refused with their own reason.

**Case 1.7.** A duplicate top-level key is refused, because a splice would leave two
definitions and the file's meaning would depend on the parser.

**Case 1.8.** `tags: [db, rust]` is a flow sequence of scalars, so it is replaceable.

**Case 1.9.** An absent key is appended after the last key, before the closing fence, in the
file's newline style. `appended_keys` names it.

**Case 1.10.** A file with no frontmatter answers `no_frontmatter` unless
`create_frontmatter` is true; then one block is inserted after the BOM and before the body.

**Case 1.11.** A key whose value is empty (`status:`) is a null value and is replaceable.

**Case 1.12 (round trip).** For every generated value, the written file reads back through the
config's typed reader as the same string. Values that would change meaning bare
(`true`, `null`, `007`, `1.0`, `a: b`, `#x`, leading or trailing space, an empty string) are
written double-quoted with `\` and `"` escaped. This is a property test.

**Case 1.13 (I4).** Every accepted patch re-reads as a valid frontmatter block plus a body.

**Case 1.14 (I5).** Unknown keys, key order, anchors on *other* keys, comments on other lines
and multi-line strings on other keys are byte-identical after a patch of one scalar.

**Case 1.15.** A patch of two keys touches two spans and nothing between them.

**Case 1.16.** The same key twice in one request, and an empty request, are `usage` errors.

**Case 1.17.** A patch whose value bytes are outside the frontmatter span cannot occur: the
scanner only reports spans inside the block. A test asserts the invariant on the scanner's
output rather than trusting the caller.

### 3.2 Concurrency and the etag (P3)

**Statement.** A write happens only against bytes the caller has seen. The check is made
twice: before the lock (fast path) and under the lock (authoritative).

**Case 2.1 (I7).** A write on an existing file with no `expected_hash` answers
`precondition_required`, exit 3, and the file is byte-identical. `hint` names the keys that
would change, in one line.

**Case 2.2.** A wrong `expected_hash` answers `conflict` with both hashes, exit 3, and the file
is byte-identical.

**Case 2.3.** A correct `expected_hash` answers a receipt whose `old_hash` is the presented
hash and whose `new_hash` is the hash of the bytes written.

**Case 2.4 (I6).** The plan is made, the file is changed by another writer, then the commit
runs. The commit answers `conflict` and leaves the other writer's bytes intact. The test needs
no thread, no sleep and no second process, because `plan` and `commit` are separate calls.

**Case 2.5.** Two commits on the same file, in sequence, with the same etag: the first wins and
the second answers `conflict`. Run with two lock acquisitions, so the lock is proved.

**Case 2.6.** `new_hash` is the value the next read reports as `content_hash`.

**Case 2.7.** A write on an archived corpus, or on a corpus with `read_only = true`, answers
`read_only_corpus` and writes nothing.

**Case 2.8.** A write to a path outside the corpus, into `.git/`, or into `.janus/` answers
`path_escape` and writes nothing.

### 3.3 The atomic replace (P3)

**Statement.** The file is replaced by a rename of a complete temp file in the same directory.
A crash leaves either the old file or the new file, never a mixture.

**Case 3.1.** A successful write leaves no temp file in the directory, before or after the
return.

**Case 3.2.** A refused or failed write leaves no temp file.

**Case 3.3.** The file mode is preserved (0o640 stays 0o640). On Unix the inode changes, which
proves the replace was a rename and not a truncate.

**Case 3.4.** A write into a directory without write permission fails with `write`, and the
file is byte-identical. Skipped on Windows.

**Case 3.5.** The parent directory is synced after the rename. The test asserts the call
happens through one function; the effect is not observable and is not claimed.

### 3.4 The gate on three surfaces (P4)

**Statement.** A server started without `--allow-write` refuses all four write tools. A CLI
write command needs no flag, because typing the command is the consent.

**Case 4.1.** `janus mcp` without the flag answers `write_disabled` for each of the four names;
with the flag, each reaches its own contract. 4 of 4 names, 1 of 1 surface.

**Case 4.2.** `janus rpc` answers identically, so the two surfaces cannot disagree.

**Case 4.3.** `janus_status` reports `writes_enabled: false` without the flag, `true` with it,
and `false` in both cases on a `read_only` corpus.

**Case 4.4.** The MCP `instructions` string states that writes exist, names the four tools, and
says that a write needs the etag a read returned. The golden that holds the string moves once,
and the diff is one paragraph.

**Case 4.5.** A receipt is not an envelope: it carries `api` and `ok`, and no `results` and no
`notes`. A test asserts the absent keys, so a later field cannot leak in.

### 3.5 `write_document` (P5)

**Case 5.1.** `create` on an absent path writes the file and answers a receipt with
`old_hash: null`.

**Case 5.2.** `create` on an existing path answers `conflict` and writes nothing.

**Case 5.3.** `overwrite` without `expected_hash` answers `precondition_required`.

**Case 5.4.** `overwrite` with the current hash replaces the whole file, frontmatter and body,
and the receipt's `changed_keys` lists every key written.

**Case 5.5.** A body that is not valid UTF-8 is a `usage` error. The writer never invents bytes.

**Case 5.6.** The written frontmatter is valid on the first read: the document's resolved type,
status and date equal what the request declared.

### 3.6 `supersede_document` (P6)

**Statement.** Two patches, one transaction. Either both files change or neither does.

**Case 6.1.** The old document gains `status: superseded` and
`superseded_by: <new path>`; the new document gains `supersedes: <old path>`. No other key
changes.

**Case 6.2.** `supersede` writes no `effective_to`, no `doc_version` and no `id` (M7-20, M7-21).

**Case 6.3.** If either patch is refused, neither file is written, and the refusal names the
key and the file.

**Case 6.4.** A supersession that would close a cycle is refused before any temp file is
staged, and the message names the cycle.

**Case 6.5.** `--append-only` creates the new file first, then patches the old. If the second
step fails, the created file is removed and the old file is byte-identical.

**Case 6.6.** The receipt carries both receipts and the resolved chain, oldest first.

**Case 6.7.** An existing `new_path` needs its own `expected_hash`; an absent one is created.

### 3.7 `promote_document` (P7)

**Statement.** Promotion declares lineage. It never moves and never rewrites an archived file.

**Case 7.1.** `link` mode patches the primary document's `supersedes` with the qualified
`corpus:path` and touches nothing else.

**Case 7.2.** `adopt` mode creates a copy in the primary corpus and leaves the source in place.

**Case 7.3.** The archive's bytes and mtime are unchanged after either mode. A test compares
the source hash before and after.

**Case 7.4.** Without `allow_cross_corpus_links`, the written link is inert and `janus doctor`
reports it, which is the rule M6b left in place.

**Case 7.5.** A promote whose target is in an archived corpus is refused.

### 3.8 The config is part of the index identity (P1)

**Statement.** A change to a config section that the index caches makes the index stale, and
`status` says so.

**Case 8.1.** After a `[paths]` edit, `janus status --json` reports `stale: true` and names the
config as the reason.

**Case 8.2.** `janus index` after that edit reports `config_changed: true` and the new type
appears in `search`. Today `status` says fresh and `search` serves the old type (measured
2026-11-10).

**Case 8.3.** A manifest written before this field existed is stale exactly once, then fresh.

**Case 8.4.** An edit to `[authority]` does not make the index stale, because authority is read
at query time. An edit to `[frontmatter]`, `[defaults]`, `[types]`, `[paths]` or `[links]`
does.

**Case 8.5.** `config_changed` is additive: `api` stays 1, and a consumer that ignores it keeps
working.

### 3.9 What must not change

1. Every read golden stays byte-identical, except the MCP `instructions` golden (case 4.4).
2. `format_version` stays 6. `api` stays 1. No exit code moves; two are added to the table.
3. No new dependency.
4. A read never takes the writer lock.
5. A write never refreshes the index.
6. The four write tool names, their parameters and their codes stay as §9.3 states.

## 4. Decisions

| ID | Decision | Why |
|---|---|---|
| M7-1 | The patcher is a hand-written line scanner, not a YAML crate | §7.3's candidate (b): no new dependency, no `unsafe`, and the refusal path is the safety feature. `oneyaml` is already present to validate a value. |
| M7-2 | The scanner reads top-level keys at column 0 only | A nested key is not a key the config reads, and a splice inside a block mapping is the reformatting §7.3 forbids. |
| M7-3 | A value is replaceable when it is a scalar on the key's line, or a flow sequence of scalars; the list is closed | A shape nobody thought of is refused, not guessed. |
| M7-4 | A trailing comment survives, because the value span ends before it | §7.3's worked example keeps `# current`. |
| M7-5 | A value is written bare only when it reads back as the same string; otherwise it is double-quoted | A write that changes a value's meaning is data loss. Case 1.12 is the proof. |
| M7-6 | A patch value of `null` writes `null`; deleting a key is not in M7 | §7.3 lists `null` as a value. A deletion needs a receipt field and a workflow, and neither exists. |
| M7-7 | A duplicate top-level key is refused | Two definitions make the file's meaning depend on the parser. |
| M7-8 | An absent key is appended after the last key, before the closing fence, in the file's newline style | Deterministic position, one changed line in `git diff` (§8.6 rule 5). |
| M7-9 | The writer takes the corpus lock, the same `.janus/lock` the builder takes | One lock, one meaning: this corpus is being mutated. Two locks need an order and a deadlock story. |
| M7-10 | The writer re-reads and re-hashes under the lock; the pre-lock check is a fast path only | I6. The fast path saves a lock in the common conflict case. |
| M7-11 | `plan` is pure and `commit` does IO | The conflict test mutates the file between the two calls, so it needs no thread, no sleep and no second process. |
| M7-12 | `conflict` and `precondition_required` both exit 3 | §9.1 defines exit 3 as "conflict/stale hash". A missing etag is the same class: re-read, then retry. |
| M7-13 | `precondition_required` carries a one-line preview in `hint`; the full preview is a `--dry-run` call | §9.4 says a failure carries no `results`, so a preview list cannot be a result. |
| M7-14 | A receipt is not an envelope: `api`, `ok`, and its own fields, no `results`, no `notes` | §9.4's envelope belongs to the five read tools. A receipt describes bytes, not rows. |
| M7-15 | CLI write commands need no `--allow-write`; the flag gates the server surfaces | D4 is about an agent with a filesystem. Typing `janus set` is consent. §9.1 is corrected. |
| M7-16 | `writes_enabled` is true only when the surface has writes enabled and the corpus is not read-only | A capability flag that says true on a read-only corpus invites a call that must fail. |
| M7-17 | A write never refreshes the index | The receipt describes bytes. A write that also builds is two failures in one call, and the next read already refreshes. |
| M7-18 | The receipt's `new_hash` is the hash of the bytes written | A receipt an agent cannot present as the next `expected_hash` is useless. |
| M7-19 | `supersede` plans both patches, stages both temp files, renames old then new, and rolls the first back if the second fails | §7.4. The rollback uses the staged copy, and the error names the inconsistent state. |
| M7-20 | `supersede` writes only `status`, `superseded_by` and `supersedes` | D25 and D38: a key the user did not ask for is not written, and the chain fills the window. Closes open question 23 for M7. |
| M7-21 | `supersede` never bumps `doc_version`, and `--bump-version` is not in M7 | Frontmatter versions belong to the user (AGENTS.md, D26). Closes open question 4 for M7; a flag is additive later. |
| M7-22 | `deprecated` keeps its declared field; no tombstone derivation | Open question 21 changes the status ladder, which a writer must not decide. It stays open. |
| M7-23 | `promote --link` patches the primary document only; `--adopt` creates a copy | §9.3 and D42: the archive is never moved or rewritten. Case 7.3 asserts the bytes. |
| M7-24 | The manifest gains `config_hash` over `[frontmatter]`, `[defaults]`, `[types]`, `[paths]`, `[links]` | Enumerate the inputs, then hash them. `[authority]` and `[decay]` are read at query time, `[chunking]` has a fingerprint, `[index] exclude` is covered by `corpus_sig`. |
| M7-25 | A manifest without `config_hash` is stale exactly once | An index built before the field existed cannot prove it matches. One rebuild, then fresh. |
| M7-26 | `format_version` stays 6; the field is additive with a defined absence | It follows `corpus_sig` (`src/index/store.rs:107`): no `deny_unknown_fields`, so an older binary ignores it. |
| M7-27 | `create` refuses an existing path; `overwrite` requires `expected_hash` | Creating over a file is a lost update with no conflict to report. |
| M7-28 | A write to an archived or `read_only` corpus is refused | M6b-4's field exists; a writer that ignored it would make it a decoration. |
| M7-29 | The MCP `instructions` string names the write tools and the etag rule | An agent that reads a false capability line will not call the tool that exists. |
| M7-30 | If M7-7 exceeds its ceiling, `promote` becomes M7b | Cross-corpus promotion is the least-used write and the easiest to defer. The budget protects the review of the writer. |

## 5. Phases

### M7-0 — The safety net (no code, no PR)

**What.** Take a backup of the large corpus, restore it to a temporary directory, and compare
by file count and content hash. Confirm that `[mcp] allow_write` is absent or false in both
real configs, and that `McpConfig::allow_write` defaults to false.

**Record.** Three numbers in §12: files backed up, files restored, hashes that differ. The
expected answer is "N, N, 0". No path is written into this document.

**Accept.** The restore reproduces every file. A backup that has never been restored is not a
backup.

**Why no PR.** There is nothing to review. It is a gate, recorded before the first write line.

### M7-1 — The config is part of the index identity

**Files.** `src/config.rs` (the hash of the five sections), `src/index/store.rs` (the field and
its validation), `src/index/cmd.rs` (report `config_changed`), `src/index/refresh.rs` (the
comparison), `src/status.rs` (the stale reason), `tests/config_freshness.rs` (new), `SPEC.md`
§8.3, this file.

**Build.** Hash, store, compare, report.

**Tests.** Cases 8.1–8.5.

**Accept.** An edit to a covered section makes the index stale and one `index` fixes it; an
edit to `[authority]` does not.

**Split seam.** The hash and the manifest field first; the report field and `status` second.

### M7-2 — The patcher

**Files.** `src/write/mod.rs`, `src/write/scan.rs`, `src/write/patch.rs`, `src/write/value.rs`
(all new), `src/lib.rs`, `tests/write_patch.rs` (new), `tests/goldens/frontmatter/` (new).

**Build.** Key spans, the splice, the value writer, the refusal list.

**Tests.** Cases 1.1–1.17, plus property tests for I1–I5 and case 1.12.

**Accept.** Every accepted patch re-reads as valid frontmatter; every refused patch returns
the input unchanged; no goldens outside the new directory move.

**Split seam.** The scanner first (`scan.rs` and its cases), then the splice and the value
writer. Each half is reviewable alone and neither needs the other's tests.

### M7-3 — The atomic replace and the lock

**Files.** `src/write/atomic.rs`, `src/write/commit.rs` (both new), `src/index/store.rs`
(expose the lock for the writer), `tests/write_atomic.rs` (new).

**Build.** Temp file, `fsync`, `rename`, parent `fsync`, mode preserve; lock, re-read, verify,
rollback.

**Tests.** Cases 2.4–2.6, 3.1–3.5.

**Accept.** A failed write leaves the file byte-identical and no temp file behind. A changed
file between plan and commit answers `conflict`.

**Split seam.** The atomic replace first, then the lock and the verification.

### M7-4 — `update_frontmatter` and `janus set`

**Files.** `src/engine.rs`, `src/api/dto.rs`, `src/api/mod.rs` (the tool and the gate),
`src/api/error.rs` (six codes), `src/error.rs` (six variants and exit 3),
`src/mcp/tools.rs`, `src/mcp/protocol.rs` (the instructions string), `src/rpc.rs`,
`src/cli.rs`, `src/main.rs`, `src/status.rs`, `tests/cli_set.rs` (new),
`tests/mcp_write.rs` (new), `tests/cli_rpc.rs`, `tests/cli_mcp.rs`, `tests/cli_status.rs`,
`tests/api_envelope.rs`.

**Build.** The tool, the gate, the receipt, the CLI command, `writes_enabled`.

**Tests.** Cases 2.1–2.3, 2.7–2.8, 4.1–4.5.

**Accept.** All three surfaces answer one contract; `writes_enabled` tells the truth; a write
without the flag is impossible.

**Split seam.** The engine, MCP and rpc first; the CLI command and its human table second.

### M7-5 — `write_document` and `janus write`

**Files.** `src/write/document.rs` (new), `src/engine.rs`, `src/api/dto.rs`,
`src/mcp/tools.rs`, `src/rpc.rs`, `src/cli.rs`, `src/main.rs`, `tests/cli_write.rs` (new).

**Build.** Create and overwrite, whole file, one receipt.

**Tests.** Cases 5.1–5.6.

**Accept.** A created document's first read agrees with the request.

**Split seam.** `create` first, `overwrite` second.

### M7-6 — `supersede_document` and `janus supersede`

**Files.** `src/write/supersede.rs` (new), `src/graph.rs` (the cycle check, reused),
`src/engine.rs`, `src/api/dto.rs`, `src/mcp/tools.rs`, `src/rpc.rs`, `src/cli.rs`,
`src/main.rs`, `tests/cli_supersede.rs` (new).

**Build.** Plan two, stage two, rename old then new, roll back.

**Tests.** Cases 6.1–6.7.

**Accept.** W4 answers: one call retires the old document and records the new one, and a
failure leaves both files as they were.

**Split seam.** In-place first, `--append-only` second.

### M7-7 — `promote_document` and `janus promote`

**Files.** `src/write/promote.rs` (new), `src/workspace.rs` (resolve a corpus by id),
`src/engine.rs`, `src/api/dto.rs`, `src/mcp/tools.rs`, `src/rpc.rs`, `src/cli.rs`,
`src/main.rs`, `tests/cli_promote.rs` (new), `tests/fixtures/workspace-promote/` (new).

**Build.** Link mode and adopt mode, with the archive read-only.

**Tests.** Cases 7.1–7.5.

**Accept.** The archive's hash is unchanged; the primary document gains one key.

**Split seam.** `link` first, `adopt` second. If the phase exceeds its ceiling, `adopt` becomes
M7b (M7-30).

### M7-8 — The rehearsal and the 200-file dry run

**Files.** `scripts/write-rehearsal.sh` (new), this file (§12), `README.md` (one section: how
to write safely).

**Build.** A script that runs every write path with `--dry-run` over a copy of a real corpus,
and diffs the result against the input.

**Tests.** Measured, not asserted in the repository: the counts go in §12.

**Accept.** 200 files dry-run, 0 unexpected byte changes, and every refusal explained by a
named shape. Then one real write on the copy, read back, and the index rebuilt.

**Split seam.** None. It is one measurement.

## 6. Cross-cutting requirements

- The gates of `AGENTS.md`, unchanged. The cross-platform check includes the writer, because
  a rename and a directory `fsync` are the two places Windows differs.
- Every test is deterministic: a `FixedClock`, a `tempfile` directory, no sleep, no port, no
  network.
- No test reads `$HOME` or the wall clock.
- Every new public item is documented; `#![warn(missing_docs)]` stays on.
- Comments in new code follow the writing rule: short sentences, plain words.
- No phase writes to a real corpus. M7-8 runs on a copy.
- After each phase merges, rebuild the local install and re-run the ten questions.

## 7. The evaluation packet

The repository's ten questions are `tests/eval/repo-ten.jsonl`, committed in R1. M7 changes no
retrieval, so it claims **no** gain on them. The baseline is the gate of 2026-11-10: top-1
4/10, top-3 10/10, p50 6 ms on this repository.

The large corpus's ten questions stay out of this repository, by rule. Its counts are recorded
outside the repository and are supplementary evidence, never a condition a phase must satisfy.

The one measured product claim M7 makes is W4: **0 of 1** paths today, **1 of 1** after M7-6.

## 8. Definition of done

1. W4 has a path: **1 of 1**, proved by a fixture test and one manual run on a copy.
2. Invariants I1–I7 each have a named test: **7 of 7**.
3. The 200-file dry run on a copy of the large corpus: **0** unexpected byte changes. Refusals
   are counted and each is explained by a named shape.
4. `--allow-write` off by default: **4 of 4** write tools refused on **2 of 2** server
   surfaces without the flag, and `writes_enabled` false in **1 of 1** status call.
5. Read goldens: **0** move, except the MCP `instructions` string (**1** intended change).
6. Config freshness: after a `[paths]` edit, `status` reports stale in **1 of 1** case, and
   `index` reports `config_changed: true` in **1 of 1**.
7. `format_version` **6**, `api` **1**, **0** exit codes moved, **2** added and documented.
8. `cargo doc --no-deps`: **0** warnings.
9. Retrieval: **0** claims. A writer cannot move a ranking number, so no row of this plan asks
   it to.

## 9. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate |
|---|---|---|---|---|
| M7-0 safety net | 0 | 0 | 0 | 0 |
| M7-1 config freshness | 250 | ~150 | 350 | ~200 |
| M7-2 patcher | 600 | ~450 | 900 | ~600 |
| M7-3 atomic replace + lock | 300 | ~200 | 450 | ~250 |
| M7-4 `update_frontmatter` + `set` | 500 | ~350 | 750 | ~450 |
| M7-5 `write_document` | 250 | ~180 | 400 | ~250 |
| M7-6 `supersede` | 400 | ~300 | 600 | ~400 |
| M7-7 `promote` | 350 | ~250 | 500 | ~300 |
| M7-8 rehearsal | 100 | ~60 | 150 | ~50 |

Estimates run two to three times low in this repository. Each phase names its split seam, and
the agent splits before the ceiling rather than shaving a comment or dropping a test.

## 10. SPEC corrections

1. **§9.1.** Name the four CLI write commands, and state that `--allow-write` gates the server
   surfaces only (M7-15). State that exit 3 is now used, by two codes.
2. **§9.3.** State that a missing `expected_hash` answers `precondition_required` with a
   one-line preview in `hint`, and that the full preview is a `--dry-run` call (M7-13).
3. **§9.4.** Add the receipt shape and state that a receipt is not an envelope (M7-14).
4. **§7.3.** State the closed list of replaceable shapes, the duplicate-key refusal, and the
   append position (M7-3, M7-7, M7-8).
5. **§7.3 step 5.** State that a `null` patch writes `null` and that deletion is not in v1
   (M7-6).
6. **§7.4.** State that `supersede` writes no `effective_to` and no `doc_version`, which closes
   open questions 4 and 23 for this milestone (M7-20, M7-21).
7. **§8.3.** Add `config_hash`, the five sections it covers, and the one-rebuild rule for a
   manifest without it (M7-24, M7-25).
8. **§11 M7.** Replace the done-when with the numbered rows of §8, including the rehearsal
   phase and the dry-run rule.

## 11. Handoff

- **M8** inherits the writer: `watch` must not race it, and the mmap load path must read a
  generation a write did not build.
- **R2** inherits the ordering gap the gate measured. Its trigger is unchanged: three wording
  failures in ten. The gate measured one in twenty, so R2 does not start yet. Recheck after
  M7 with the same packet.
- **The gate's other findings** stay out of M7 and are named here so they are not lost: a warm
  re-index costs the same as a cold one (walk and hash dominate), chunk counts are uneven
  (mean 52.9 on this repository, 13.25 on the large corpus, one document with zero chunks),
  and an index file that mentions everything outranks the document that answers. The last one
  is a corpus-config question, measured and reverted on 2026-11-10.
- **Open question 21** (`deprecated` and tombstone derivation) is still open. It belongs to a
  milestone that changes the status ladder.

## 12. Implementation logs

Empty. One entry per phase, appended when the phase merges. The M7-0 record goes first.
