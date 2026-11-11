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
| an advisory corpus lock | **exists** | `pub struct Lock` and `Lock::acquire`, `.janus/lock`, 10 s timeout (`src/index/store.rs:452`, `:462`, `:46`, `:49`) |
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
6. Receipts, and eight new public error codes.
7. `--allow-write` as a real gate, and `writes_enabled` as a real capability flag.
8. The config hash in the manifest, because the writer resolves type from config and the
   index caches it (gate defect, 2026-11-10).
9. Cross-corpus link resolution, because M6b handed it to M7 and a promoted link no reader
   follows is not a feature (M7-37).
10. The rehearsal: every write path on a generated 200-file corpus, plus one recorded run on a
    copy of a real corpus.

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
| Cross-corpus *supersession* (an edge that changes another corpus's windows and status) | M8 | M7-37 resolves a link for reading. Making a link change another corpus's answers is a scoring and validity question, and this milestone changes no ranking. |

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
(`src/vault/frontmatter.rs:31`). No new crate is added. If a span-preserving YAML crate is
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

A patch **key** must match `[A-Za-z0-9][A-Za-z0-9._-]*`. That covers every key the config
reads (`superseded_by`, `shared-with`, `doc_version`). A key outside it is refused with
`KeyShape`, because a key is written as a line: a newline, `: `, `#` or a quote inside a key
would add or split a key, which is an injection, not a patch.

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
    pub original: Vec<u8>,                // the bytes the plan was made from
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

`original` is what makes a rollback possible: after a rename, the old bytes exist nowhere
else, and a two-file transaction must be able to put them back (M7-31).

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
| `PathExists { path }` | `path_exists` | 4 | `create` named a file that exists |
| `KeyShape { path, key }` | `key_shape` | 4 | a patch key is outside the key pattern |

Exit 3 is used for the first time. Both new exit-3 codes mean one thing: re-read, then retry.
A write that fails a shape check is a refusal (exit 4), because a retry without a change of
request cannot succeed. `path_exists` is a refusal for the same reason: a retry with the same
request cannot succeed.

### 2.5 The manifest (`src/index/store.rs`)

`Manifest` gains one field, on the pattern `corpus_sig` already set (`:107`–`:114`):

```rust
/// blake3 of the config sections that feed cached document rows (M7-1).
#[serde(default)]
pub config_hash: Option<String>,
```

The hash covers `[frontmatter]`, `[defaults]`, `[types]` and `[paths]`. Those are the sections
whose value the index caches per document: the resolved type name is stored in the snapshot
(`src/index/build.rs:472`), and a build re-resolves metadata from the current config while it
reuses tokens only (`src/index/build.rs:5`).

The hash covers nothing else, and the reason is checkable:

| Section | Why it is outside the hash |
|---|---|
| `[authority]`, `[decay]`, `[scoring]` | read at query time, per call |
| `[links] allow_disjoint_supersession` | applied per call, when the graph is built from the stored edges (`src/graph.rs:244`) |
| `[chunking]` | already has its own fingerprint in the manifest |
| `[index] exclude` | changes the admitted set, which `corpus_sig` already covers |
| `[mcp]`, `[embedding]` | nothing the index caches depends on them |

A hash over more than this would force a rebuild for a change that already takes effect.

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
config's typed reader as the same string. The generator's alphabet is: a fixed list
(`true`, `null`, `007`, `1.0`, `a: b`, `#x`, `[x]`, `- x`, `&a`, `*a`, `'q'`, `"q"`), an empty
string, a string with a leading or trailing space, a random printable ASCII string, a string
with `\n`, `\r`, `\t`, `\0` and one other control byte, and a non-ASCII UTF-8 string. This is
a property test, and it is the case that decides whether the writer can be trusted.

**Case 1.12a (the parser spike, first).** A value containing a newline and a double quote is
written double-quoted, then read back by `oneyaml` and by the config's typed reader. This runs
before any other case in M7-2. If `oneyaml` cannot read it, M7-2 stops and reports; it does not
add a crate to work around it.

**Case 1.12b (the escape set).** A double-quoted value escapes exactly `\\`, `\"`, `\n`, `\r`,
`\t`, and every other byte below `0x20` or equal to `0x7f` as `\uXXXX`. Whether a value may be
written bare is decided by the **parse-back**, not by a character class: write it bare, parse it
back, and quote it when the result is not the same string. A class alone is not enough — `a: b`
and `- x` both pass a permissive class and both change meaning bare (case 1.12's generator
holds them for exactly this reason). The class `[A-Za-z0-9 ._/:@+-]` is only a fast path, and it
excludes `: `, a trailing `:`, a leading `-`, `?` or `:`, a lone `-`, and `#`. A test asserts
the set, and a second test asserts that each refused bare form is accepted when quoted.

**Case 1.12c (the key).** A patch key outside `[A-Za-z0-9][A-Za-z0-9._-]*` is refused with
`KeyShape`. `a\nsuperseded_by: x`, `a: b`, `a#b` and `"a"` are each refused, and the file is
byte-identical.

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

**Case 2.4 (I6, the fast path).** The plan is made, the file is changed by another writer, then
the commit runs. The commit answers `conflict` and leaves the other writer's bytes intact. The
test needs no thread, no sleep and no second process, because `plan` and `commit` are separate
calls.

**Case 2.4a (I6, the authoritative check).** The file is changed after the commit's pre-lock
check and before the lock is taken, through the test seam of M7-33. The commit answers
`conflict` and writes nothing. Without this case, deleting the under-lock re-read leaves every
other case green, and that re-read is the only thing the lock buys between two writers.

**Case 2.5.** Two commits on the same file, in sequence, with the same etag: the first wins and
the second answers `conflict`. Run with two lock acquisitions, so the lock is proved.

**Case 2.6.** `new_hash` is the value the next read reports as `content_hash`.

**Case 2.7a (workspace mode).** A write into a corpus whose role is `archived`, or whose
`read_only` is true, answers `read_only_corpus` and writes nothing. `read_only` is a workspace
attribute (`src/workspace.rs:52`), so this case runs with a workspace.

**Case 2.7b (single-corpus mode).** There is no role and no `read_only` without a workspace, so
every write is allowed. The test asserts that the refusal of 2.7a cannot fire without a
workspace, so a caller is never told "read-only" by a mode that has no such field.

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

**Case 4.6.** `[mcp] allow_write = true` in a config does not enable writes, and the process
prints one warning naming `--allow-write`. The key is parsed today and used by nothing
(`src/config.rs:717`); one decision with two switches is one switch too many (M7-36).

### 3.5 `write_document` (P5)

**Case 5.1.** `create` on an absent path writes the file and answers a receipt with
`old_hash: null`.

**Case 5.2.** `create` on an existing path answers `path_exists`, exit 4, and writes nothing.
It does not answer `conflict`, because there is no etag to compare: the request itself cannot
succeed.

**Case 5.3.** `overwrite` without `expected_hash` answers `precondition_required`.

**Case 5.4.** `overwrite` with the current hash replaces the whole file, frontmatter and body,
and the receipt's `changed_keys` lists every key written.

**Case 5.5.** A body that is not valid UTF-8 is a `usage` error. The writer never invents bytes.

**Case 5.6.** The written frontmatter is valid on the first read: the document's resolved type,
status and date equal what the request declared.

### 3.6 `supersede_document` (P6)

**Statement.** One call retires the old document and records the new one. Either both files end
in their new state, or both keep their old bytes.

**Case 6.1.** In-place mode, which is the default: the old document gains `status: superseded`
and `superseded_by: <new path>`; the created document gains `supersedes: <old path>`. No other
key changes on either file (SPEC §7.4).

**Case 6.2.** `supersede` writes no `effective_to`, no `doc_version` and no `id` (M7-20, M7-21).

**Case 6.3.** If either patch is refused, neither file is written, and the refusal names the
key and the file. Both patches are planned before either is staged.

**Case 6.4.** A supersession that would close a cycle is refused before any temp file is
staged, and the message names the cycle. The chain is read from the two documents' frontmatter
on disk, not from the index, so the check runs on a corpus that was never built (M7-34).

**Case 6.5.** `--append-only` creates the new document with `supersedes` and leaves the old
one byte-identical (SPEC §7.4). Only the new document is planned, so no temp file is staged
for the old one and a refusal costs nothing.

**Case 6.6.** The receipt carries both receipts and the resolved chain, oldest first.

**Case 6.7.** An existing `new_path` answers `path_exists` and writes nothing. The wire contract
has one `expected_hash`, so a second file cannot be guarded; the call creates its new document
instead of patching one (M7-35).

**Case 6.8 (rollback).** The second rename fails, through the seam of M7-33. Both files are
byte-identical to their state before the call, **and file 1 keeps its original mode**, because
a rename of a staged copy would otherwise leave the user's file at the temp file's mode. The
error names the file that failed and the file that was restored (M7-31).

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

**Case 7.6 (resolution).** After `link`, `get_lineage` on the primary document includes the
archived source as a node, and `janus doctor` stops reporting the link as `other_corpus`. Today
the resolver answers `DanglingReason::OtherCorpus` (`src/link.rs:135`), so the written link is
visible to no reader.

**Case 7.7.** Flipping `allow_cross_corpus_links` on and re-querying resolves the link with no
rebuild and no re-index, because the policy is applied when the graph is built
(`src/graph.rs:244`).

**Case 7.8.** A qualified link whose corpus is not in the workspace is `not_found` and names
the corpora the workspace holds. A qualified link whose corpus is archived and excluded by the
query is not followed, and the chain ends there with a note.

### 3.8 The config is part of the index identity (P1)

**Statement.** A change to a config section that the index caches makes the index stale, and
`status` says so. The mismatch is a **refresh** trigger, the same class as a changed file: the
next read refreshes, and `status` reports the reason until one does (M7-32). It is not a cold
rebuild, so no command exits 1 over a config edit.

**Case 8.1.** After a `[paths]` edit and before any read, `janus status --json` reports
`stale: true` with `stale_reason: "config"`. The field is **absent** when the index is fresh, so
a fresh report keeps its shape and no read golden moves. Today `status` reports `stale: false`
(measured 2026-11-10).

**Case 8.2.** After that edit, a `search` answers the new type and prints one `refresh:` line
naming the config. `status` then reports fresh. The field `config_changed: true` appears on the
build that applied the change, which is that refresh or a `janus index` run before it — never
on both, because the second build has nothing to change.

**Case 8.3.** A manifest written before this field existed is stale exactly once, then fresh.
The build that cures it reuses every token, because a build re-resolves metadata and reuses
tokens only (`src/index/build.rs:5`). No re-chunk, no `format_version` move.

**Case 8.4.** An edit to `[authority]`, `[decay]`, `[scoring]` or `[links]` does not make the
index stale, because each is read at query time. An edit to `[frontmatter]`, `[defaults]`,
`[types]` or `[paths]` does.

**Case 8.5.** `config_changed` is additive: `api` stays 1, and a consumer that ignores it keeps
working.

### 3.9 What must not change

1. Every read golden stays byte-identical, except the MCP `instructions` golden (case 4.4).
2. `format_version` stays 6. `api` stays 1. No exit code moves; eight codes are added, and
exit 3 is used for the first time.
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
| M7-19 | `supersede` plans both patches, stages both temp files, renames old then new, and rolls the first back from the original bytes the plan holds if the second fails | §7.4. The staged file holds the *new* bytes, so it cannot restore the old ones; `Plan.original` is what makes the rollback real (M7-31). |
| M7-20 | `supersede` writes only `status`, `superseded_by` and `supersedes` | D25 and D38: a key the user did not ask for is not written, and the chain fills the window. Closes open question 23 for M7. |
| M7-21 | `supersede` never bumps `doc_version`, and `--bump-version` is not in M7 | Frontmatter versions belong to the user (AGENTS.md, D26). Closes open question 4 for M7; a flag is additive later. |
| M7-22 | `deprecated` keeps its declared field; no tombstone derivation | Open question 21 changes the status ladder, which a writer must not decide. It stays open. |
| M7-23 | `promote --link` patches the primary document only; `--adopt` creates a copy | §9.3 and D42: the archive is never moved or rewritten. Case 7.3 asserts the bytes. |
| M7-24 | The manifest gains `config_hash` over `[frontmatter]`, `[defaults]`, `[types]`, `[paths]` | Enumerate the inputs, then hash them. These four feed the cached type name (`src/index/build.rs:472`). Everything else is read per call, or already fingerprinted, or covered by `corpus_sig` (§2.5). `DocMeta::timeless` is written from `[decay] timeless_types` (`src/index/build.rs:509`) and read by nothing, so `[decay]` stays out: the hash covers the sections that feed a value a reader uses, and that field is not one — drop it in the next format bump. |
| M7-25 | A manifest without `config_hash` is stale exactly once: one refresh, then fresh | An index built before the field existed cannot prove it matches. The cure is the ordinary metadata refresh, never a cold rebuild (M7-32). |
| M7-26 | `format_version` stays 6; the field is additive with a defined absence | It follows `corpus_sig` (`src/index/store.rs:107`): no `deny_unknown_fields`, so an older binary ignores it. |
| M7-27 | `create` refuses an existing path; `overwrite` requires `expected_hash` | Creating over a file is a lost update with no conflict to report. |
| M7-28 | A write to an archived or `read_only` corpus is refused | M6b-4's field exists; a writer that ignored it would make it a decoration. |
| M7-29 | The MCP `instructions` string names the write tools and the etag rule | An agent that reads a false capability line will not call the tool that exists. |
| M7-30 | If M7-7 exceeds its ceiling, `promote` becomes M7b | Cross-corpus promotion is the least-used write and the easiest to defer. The budget protects the review of the writer. |
| M7-31 | `Plan` keeps the original bytes, and the two-file transaction stages them as its rollback copy, with the original mode | After a rename the old bytes exist nowhere else. A rollback that cannot restore byte for byte and mode for mode is not a rollback. Case 6.8 proves both with an injected failure. |
| M7-32 | A `config_hash` mismatch is a **refresh** trigger, the same class as a changed file, never a cold rebuild | The cure already exists: a build re-resolves metadata and reuses tokens (`src/index/build.rs:5`). A cold rebuild would make a config edit exit 1 on `search`, which is hostile for a change that needs no re-chunk. |
| M7-33 | `commit` takes a test-only `Hooks` value with two seams: after the pre-lock check, and before the second rename | The two checks the writer exists for — the under-lock re-read and the rollback — are unreachable in a test without a seam. A no-op production impl keeps the dependency explicit, like `Clock` and `Embedder`. |
| M7-34 | The cycle check reads the chain from frontmatter on disk, bounded to 64 nodes, not from the index | A write never refreshes the index (M7-17), so an index-based check reads a snapshot the writer just invalidated, and cannot run on a corpus that was never built. Paths are the only link form a write can follow without a snapshot. |
| M7-35 | `supersede` creates its new document; an existing `new_path` is `path_exists` | The wire contract has one `expected_hash` (§9.3), so a second file cannot be guarded. Creating is the case the tool exists for, and it needs no second etag. |
| M7-36 | `[mcp] allow_write` is ignored with one warning; the flag alone gates the server surfaces | One decision, one switch. A config file that silently enables writes for every agent is the failure D4 exists to prevent. |
| M7-37 | Cross-corpus link resolution is in M7-7, not deferred | M6b handed it to M7, and without it `promote` writes a link no reader follows (`src/link.rs:135`), so the tool would ship inert. §0.1 names it as in scope; §0.2 keeps cross-corpus supersession out. |
| M7-38 | The four write commands take `--workspace` and resolve a path against the primary unless it carries a `corpus:path` qualifier | `read_only` and a corpus role live only in a workspace (`src/workspace.rs:52`), and `promote` needs two corpora. §9.1's list of workspace-reading commands grows from ten to fourteen. |
| M7-39 | M7-1 ships as two PRs of one phase, 1a (the hash and the manifest field) and 1b (the comparison, `status` and the `index` report) | The phase measured 253 logic and 426 test lines as one PR, against the 250/350 ceiling. 1a measures 108/168 and 1b 150/279, so the seam §5 names splits it inside the ceiling without shaving a comment or dropping a case. Wrong if either part exceeds its ceiling. |
| M7-40 | `status` and the read reports count the config in freshness; `doctor` and `corpora` count the corpus only | One word, two questions. `status` answers "may I trust the stored metadata", so a config edit makes it stale and it names the reason. `search`, `facts`, `read` and `lineage` report `index.stale` only when a read declined to refresh, so the field keeps its meaning and gains the config as a second cause. `doctor` reports graph and link facts read from the generation, and a config edit is not one of them; `corpora` reports one row per corpus from its manifest and one digest. A phase that wants the config in either one adds it there and amends this row. |
| M7-41 | A long-lived process keeps the config it started with, and never publishes a manifest under a config the file on disk disagrees with | `Engine.config` is loaded once. Two processes pinned to different configs would otherwise alternate a refresh and a publish on every read, and the type that answers would depend on who wrote last. So on a config mismatch a read re-reads the file: if the manifest already names the file's config it adopts it and publishes nothing, if the file names this process's config it publishes, and otherwise it publishes nothing and says `refresh: skipped, config changed elsewhere`. Reloading the config in `Engine` is a design change with its own risks and is not in this phase. Reopen it when a user reports that a server needed a restart to see a config edit. |

## 5. Phases

### M7-0 — The safety net (no code, no PR)

**What.** Take a backup of the large corpus, restore it to a temporary directory, and compare by
file count, relative path list and content hash. The backup covers every file the writer can
touch — all Markdown, `janus.toml`, and `.git/` and `.janus/` whole — plus every other file under
50 MiB that is not media or a database. It excludes files at or above 50 MiB and the extensions
`.mp4`, `.wav`, `.flac`, `.m4a`, `.mov`, `.zip`, `.sqlite3` and `.db`, because a 9.4 GiB copy of
material the writer cannot touch proves nothing about the writer and risks the disk. Every
exclusion is counted.

**Record.** In §12: files backed up, files restored, files that differ, the excluded count and
size, and any file the account could not read. The expected answer is "N, N, 0".

**Accept.** The restore reproduces every file. A backup that has never been restored is not a
backup.

**Why no PR.** There is nothing to review. It is a gate, recorded before the first write line.

### M7-1 — The config is part of the index identity

**Files.** `src/config.rs` (the hash of the four sections), `src/index/store.rs` (the field and
its validation), `src/index/cmd.rs` (report `config_changed`), `src/index/refresh.rs` (the
comparison and the refresh trigger), `src/status.rs` (the stale reason), `tests/config_freshness.rs`
(new), `SPEC.md` §8.3, this file.

**Build.** Hash, store, compare, report, and treat a mismatch as a refresh trigger.

**Tests.** Cases 8.1–8.5.

**Accept.** An edit to a covered section makes the index stale, one read cures it, and the new
type answers; an edit to `[authority]` or `[links]` changes nothing about freshness.

**Split seam.** Taken (M7-39). 1a is the hash and the manifest field, with the build that writes
them. 1b is the comparison, `status` and the `index` report field.

**Amendment.** The original file list named six files and the change touches five more.
`src/index/build.rs` gains the `config_hash` parameter of the manifest constructor, and
`src/engine.rs` gains the field in the load-cache key, because both follow the manifest rather
than the phase's subject. `src/doctor.rs` passes `None` to the witness, which is the decision
M7-40 records. `tests/publish.rs` and `tests/staleness.rs` build a manifest directly, so they
name the new field. The five are consequences of the field, not new scope.

### M7-2 — The patcher

**Files.** `src/write/mod.rs`, `src/write/scan.rs`, `src/write/patch.rs`, `src/write/value.rs`
(all new), `src/lib.rs`, `tests/write_patch.rs` (new), `tests/goldens/frontmatter/` (new).

**Build.** Key spans, the splice, the value writer, the refusal list, the key pattern.

**Tests.** Cases 1.1–1.17, including 1.12a, 1.12b and 1.12c, plus property tests for I1–I5 and
case 1.12.

**Accept.** Case 1.12a runs first and settles that `oneyaml` reads an escaped scalar. Every
accepted patch re-reads as valid frontmatter; every refused patch returns the input unchanged;
no goldens outside the new directory move.

**If it goes wrong.** If `oneyaml` cannot read a double-quoted scalar with `\n` and `\"`, stop
and report. Do not add a YAML crate and do not narrow the escape set to fit.

**Split seam.** The scanner first (`scan.rs` and its cases), then the splice and the value
writer. Each half is reviewable alone and neither needs the other's tests.

### M7-3 — The atomic replace and the lock

**Files.** `src/write/atomic.rs`, `src/write/commit.rs` (both new), `src/index/store.rs`
(expose the lock for the writer), `tests/write_atomic.rs` (new).

**Build.** Temp file, `fsync`, `rename`, parent `fsync`, mode preserve; lock, re-read, verify,
and the two `Hooks` seams (M7-33).

**Tests.** Cases 2.4, 2.4a, 2.5, 2.6, 3.1–3.5.

**Accept.** A failed write leaves the file byte-identical and no temp file behind. A file
changed after the pre-lock check and before the lock answers `conflict`, and the test proves
it by removing the under-lock re-read and watching case 2.4a fail.

**Split seam.** The atomic replace first, then the lock and the verification.

### M7-4 — `update_frontmatter` and `janus set`

**Files.** `src/engine.rs`, `src/api/dto.rs`, `src/api/mod.rs` (the tool and the gate),
`src/api/error.rs` (the codes), `src/error.rs` (the variants and exit 3),
`src/mcp/tools.rs`, `src/mcp/protocol.rs` (the instructions string), `src/rpc.rs`,
`src/cli.rs`, `src/main.rs`, `src/status.rs`, `src/config.rs` (the ignored `[mcp]` key and its
warning), `tests/cli_set.rs` (new), `tests/mcp_write.rs` (new), `tests/cli_rpc.rs`,
`tests/cli_mcp.rs`, `tests/cli_status.rs`, `tests/api_envelope.rs`.

**Build.** The tool, the gate, the receipt, the CLI command, `writes_enabled`, the ignored
config key.

**Tests.** Cases 2.1–2.3, 2.7a, 2.7b, 2.8, 4.1–4.6.

**Accept.** All three surfaces answer one contract; `writes_enabled` tells the truth; a write
without the flag is impossible; a config key cannot enable it.

**Split seam.** **Taken, not optional.** This phase ships as two pull requests, because it is
the only phase whose work cannot fit one review at `AGENTS.md`'s ceiling.

- **4a — the engine, MCP and rpc.** `src/engine.rs`, `src/api/*`, `src/error.rs`,
  `src/mcp/*`, `src/rpc.rs`, `src/status.rs`, `src/config.rs`, `tests/mcp_write.rs`,
  `tests/cli_rpc.rs`, `tests/cli_mcp.rs`, `tests/cli_status.rs`, `tests/api_envelope.rs`.
  Cases 2.1–2.3, 2.7a, 2.7b, 2.8, 4.1–4.6.
- **4b — `janus set`.** `src/cli.rs`, `src/main.rs`, `tests/cli_set.rs`. The human table, the
  refusal list, and the workspace resolution of a write path (M7-38).

4a carries the codes, the gate and the receipt. 4b adds one command arm over the contract 4a
proved, so its review reads the surface, not the writer.

### M7-5 — `write_document` and `janus write`

**Files.** `src/write/document.rs` (new), `src/engine.rs`, `src/api/dto.rs`,
`src/mcp/tools.rs`, `src/rpc.rs`, `src/cli.rs`, `src/main.rs`, `tests/cli_write.rs` (new).

**Build.** Create and overwrite, whole file, one receipt.

**Tests.** Cases 5.1–5.6.

**Accept.** A created document's first read agrees with the request.

**Split seam.** `create` first, `overwrite` second.

### M7-6 — `supersede_document` and `janus supersede`

**Files.** `src/write/supersede.rs` (new), `src/write/chain.rs` (new: the on-disk chain read and
the cycle check), `src/engine.rs`, `src/api/dto.rs`, `src/mcp/tools.rs`, `src/rpc.rs`,
`src/cli.rs`, `src/main.rs`, `tests/cli_supersede.rs` (new).

**Build.** Plan two, keep both originals, stage two, rename old then new, roll back from the
original bytes on a failed second rename. The cycle check reads frontmatter on disk and follows
paths, bounded to 64 nodes, so it needs no index (M7-34). `--append-only` creates the new
document and leaves the old one untouched.

**Tests.** Cases 6.1–6.8, including the injected second-rename failure through the hook of
M7-33.

**Accept.** W4 answers: one call retires the old document and records the new one, and a
failure leaves both files as they were, byte for byte.

**Split seam.** In-place first, `--append-only` second.

### M7-7 — `promote_document` and `janus promote`

**Files.** `src/write/promote.rs` (new), `src/workspace.rs` (resolve a corpus by id),
`src/engine.rs`, `src/api/dto.rs`, `src/mcp/tools.rs`, `src/rpc.rs`, `src/cli.rs`,
`src/main.rs`, `tests/cli_promote.rs` (new), `tests/fixtures/workspace-promote/` (new).

**Build.** Link mode, adopt mode, and the resolver: a qualified `corpus:path` becomes a node in
`get_lineage`, and `doctor` stops calling it `other_corpus`.

**Tests.** Cases 7.1–7.8.

**Accept.** The archive's hash is unchanged; the primary document gains one key; and the chain
that key names is readable through `get_lineage`, which is what makes `promote` a tool rather
than a line in a file.

**Split seam.** Part 1 writes the link (`promote.rs`, cases 7.1–7.5). Part 2 resolves it
(`src/link.rs`, `src/graph.rs`, `src/lineage.rs`, cases 7.6–7.8). If part 2 exceeds its ceiling,
`promote` is not done: `adopt` becomes M7b (M7-30) and the resolution stays here, because a
write whose result no reader follows is not a feature.

### M7-8 — The rehearsal and the 200-file dry run

**Files.** `scripts/write-rehearsal.sh` (new), `tests/fixtures/write-vault/` (new: a generated
200-file corpus), `this file` (§12), `README.md` (one section: how to write safely).

**Build.** A fixture generator for 200 files covering every shape the scanner knows: scalar,
flow sequence, block sequence, flow mapping, multi-line scalar, anchor, alias, duplicate key,
comment on the value line, CRLF, BOM, no frontmatter, empty value, and a non-ASCII value. Then
a script that dry-runs every write path over it and diffs the result.

**Tests.** The counts are asserted in the repository: 200 files, 0 unexpected byte changes, and
a named shape for every refusal. The same run on a copy of a real corpus is recorded in §12 as
evidence, not as a condition.

**Accept.** 200 files dry-run on the fixture, 0 unexpected byte changes, every refusal explained.
Then one real write on the copy, read back, and the index rebuilt.

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
3. The 200-file dry run on the generated fixture: **0** unexpected byte changes. Refusals are
   counted and each is explained by a named shape. The same run on a copy of a real corpus is
   recorded in §12 as evidence, not as a condition, because this repository must not describe
   that corpus.
4. `--allow-write` off by default: **4 of 4** write tools refused on **2 of 2** server
   surfaces without the flag, and `writes_enabled` false in **1 of 1** status call. A config
   key cannot enable it: **1 of 1** case.
5. Read goldens: **0** move, except the MCP `instructions` string (**1** intended change).
6. Config freshness: after a `[paths]` edit and before any read, `status` reports stale in
   **1 of 1** case; after one read, the new type answers and `status` is fresh, in **1 of 1**.
7. Cross-corpus resolution: a promoted link is readable through `get_lineage` in **1 of 1**
   case, and `doctor` reports **0** `other_corpus` rows for it.
8. `format_version` **6**, `api` **1**, **0** exit codes moved, **8** codes added and documented,
   of which **2** carry the newly used exit 3.
9. `cargo doc --no-deps`: **0** warnings.
10. Retrieval: **0** claims. A writer cannot move a ranking number, so no row of this plan asks
    it to.

## 9. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate |
|---|---|---|---|---|
| M7-0 safety net | 0 | 0 | 0 | 0 |
| M7-1 config freshness | 250 | ~150 | 350 | ~200 |
| M7-2 patcher | 600 | ~450 | 900 | ~600 |
| M7-3 atomic replace + lock | 300 | ~200 | 450 | ~250 |
| M7-4 `update_frontmatter` + `set`, two PRs (4a engine/MCP/rpc ~380, 4b CLI ~220) | 600 | ~600 | 900 | ~700 |
| M7-5 `write_document` | 250 | ~180 | 400 | ~250 |
| M7-6 `supersede` | 400 | ~300 | 600 | ~400 |
| M7-7 `promote` + resolution | 550 | ~450 | 750 | ~500 |
| M7-8 rehearsal | 200 | ~150 | 250 | ~150 |

M7-1 is two PRs of one phase for the same reason (M7-39): 253 logic and 426 test lines as one
PR, 108/168 and 150/279 as two.

M7-4 was 500 until the review showed that three surfaces, eight codes with doc comments and
five test files cannot fit it. It is now **two PRs of one phase**, 4a and 4b, so the phase
stays inside `AGENTS.md`'s 600 logic ceiling instead of claiming an exception. M7-7 grew by the
resolver (M7-37) and by its cases 7.6–7.8.

Estimates run two to three times low in this repository. Each phase names its split seam, and
the agent splits before the ceiling rather than shaving a comment or dropping a test.

## 10. SPEC corrections

1. **§9.1.** Name the four CLI write commands, and state that `--allow-write` gates the server
   surfaces only (M7-15). State that exit 3 is now used, by two codes. Add the four write
   commands to the list of commands that read a workspace, which grows from ten to fourteen
   (M7-38).
2. **§9.3.** State that a missing `expected_hash` answers `precondition_required` with a
   one-line preview in `hint`, and that the full preview is a `--dry-run` call (M7-13). State
   that `new_path` must be absent, and that an existing one answers `path_exists` (M7-35).
3. **§9.4.** Add the receipt shape and state that a receipt is not an envelope (M7-14).
4. **§7.3.** State the closed list of replaceable shapes, the duplicate-key refusal, the key
   pattern, the escape set, and the append position (M7-3, M7-7, M7-8, M7-5).
5. **§7.3 step 5.** State that a `null` patch writes `null` and that deletion is not in v1
   (M7-6).
6. **§7.4.** State that in-place writes `status` and `superseded_by` on the old document and
   `supersedes` on the new one; that `--append-only` creates the new document and leaves the
   old one untouched; and that no `effective_to` and no `doc_version` is written (M7-20, M7-21,
   M7-35). **Step 1 of §7.4 also changes**: the new document must not exist, and an existing
   one is `path_exists` — step 1's "validate both docs exist" is true of the old document
   only. This closes open questions 4 and 23 for this milestone.
7. **§8.3.** Add `config_hash`, the four sections it covers, the reason for each section that
   is outside it, and the refresh-trigger rule for a manifest without it (M7-24, M7-25, M7-32).
8. **§6.11.** State that a qualified cross-corpus link resolves when
   `allow_cross_corpus_links` is true, and that `get_lineage` follows it (M7-37).
9. **§16.1.** Remove `[mcp] allow_write` from the shipped config and state that it is ignored
   with a warning (M7-36).
10. **§11 M7.** Replace the done-when with the numbered rows of §8, including the rehearsal
    phase and the fixture rule.

## 11. Handoff

- **M8** inherits the writer: `watch` must not race it, and the mmap load path must read a
  generation a write did not build. M8 also inherits cross-corpus *supersession*, which §0.2
  keeps out of M7 because it changes another corpus's windows and status.
- **The shipped config changes once.** `[mcp] allow_write` leaves `janus.toml` and the config
guide in M7-4, because the flag alone gates a server (M7-36). An existing config keeps the key
and gets one warning, so no user is broken by the removal.
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

One entry per phase, appended when the phase merges. The M7-0 record goes first.

### M7-0 — The safety net

**State.** Complete for every file this account can read. Run before M7-1.

**Numbers.** 15,752 files backed up, 15,752 restored, **0** differ by content, mode, type or
symlink target. The relative path lists are identical. 376 files (9.4 GiB) excluded by the rule
above: media, databases, and two files at or above 50 MiB. 2,409 Markdown files, 3 config files,
3,239 `.git/` files and 4 `.janus/` files are all inside the set.

**The comparison can fail.** One byte appended to a restored file reported exactly 1 difference;
restoring that file reported 0. One mode change reported exactly 1; reverting it reported 0.

**Open, for the vault owner.** 6 files (4.4 MiB) are owned by another account at mode 0600 and
are outside the backup and outside the comparison. A full restore would not bring them back.
They must be made readable and the rehearsal re-run before the first phase that writes to that
corpus (M7-3).

**What the rehearsal cannot prove.** The backup and the restore were on one volume, so this
proves the copy logic and not the survival of a disk failure. Extended attributes and ACLs were
not compared. A live database and two appending logs made 4 files differ against the live tree;
the snapshot comparison correctly ignores them, and a quiet snapshot needs those writers
stopped.
