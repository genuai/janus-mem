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

**Case 2.4b (the seam that is not called).** A commit with a hook that panics in
`before_second_rename` succeeds, because a one-file commit renames once (M7-33). Without this
case, "the second seam stays uncalled in M7-3" is asserted nowhere.

**Case 2.4c (a link).** An in-corpus symlink is admitted as a document under its own path, so
the bytes live at its target. A commit through the link replaces the target and leaves the link
a link; replacing the link itself would leave the target stale and the corpus with two
documents. A link that resolves outside the corpus answers `path_escape` and writes nothing.

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

**The temp file and a dead process.** A process killed between the create and the rename leaves
one `.tmp` file in the document's directory. Nothing sweeps that directory: a writer that deletes
files it did not create is worse than one file that `doctor` can report, and M8 reports it
(M7-47). A running process removes its temp file on every error path.

**Case 3.1.** A successful write leaves no temp file in the directory, before or after the
return.

**Case 3.2.** A refused or failed write leaves no temp file. Both paths are covered: a stale
etag, which stages nothing, and a rename that fails after the temp file exists.

**Case 3.3.** The file mode is preserved (0o640 stays 0o640). On Unix the inode changes, which
proves the replace was a rename and not a truncate.

**Case 3.3b.** The file's owner and group are preserved. A rename installs a new inode owned by
the writer, so `chgrp` to a second group the process belongs to, commit, and assert the group is
unchanged. Skipped when the process has one group, because then nothing can change.

**Case 3.4.** A write into a directory without write permission fails with `write`, and the
file is byte-identical. Skipped on Windows.

**Case 3.5.** The parent directory is synced after the rename. The call site is unobservable,
so the test asserts the one function that performs it: it syncs a directory and names the one it
cannot sync. The effect is not claimed.

**Case 3.6.** A new document (`Mode::New`) takes `0o666` masked by the process umask, because
the mode is set at creation. The test creates a probe file with a plain create and compares the
two modes, so it reads no umask. A umask of `0o077` makes both modes `0o600`, and then the case
cannot tell a mask from a hardcoded mode; no other umask hides it.

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
| M7-39 | M7-1 ships as two PRs of one phase, 1a (the hash and the manifest field) and 1b (the comparison, `status` and the `index` report) | The phase measured 253 logic and 426 test lines as one PR, against the 250/350 ceiling. After the review rounds 1a measures 124/181 and 1b 233/437. The review added five cases — the both-causes line, the pinned-config adopt, the pinned-config decline, a trailing-slash hash case, and a `[defaults] status` case — two of them the P1 decline, so 1b is 87 over the phase's 350 test ceiling. The repository's own ceiling still holds: the phase is 597 test lines cumulative against `main`, inside 900. No comment was shaved and no case dropped to fit; the phase ceiling gates an initial submission, and a reviewed round reports it. Wrong if a part is over its ceiling after review closes. |
| M7-40 | `status` and the read reports count the config in freshness; `doctor` and `corpora` count the corpus only | One word, two questions. `status` answers "may I trust the stored metadata", so a config edit makes it stale and it names the reason. `search`, `facts`, `read` and `lineage` report `index.stale` only when a read declined to refresh, so the field keeps its meaning and gains the config as a second cause. `doctor` reports graph and link facts read from the generation, and a config edit is not one of them; `corpora` reports one row per corpus from its manifest and one digest. A phase that wants the config in either one adds it there and amends this row. |
| M7-41 | A long-lived process keeps the config it started with, and never publishes a manifest under a config the file on disk disagrees with | `Engine.config` is loaded once. Two processes pinned to different configs would otherwise alternate a refresh and a publish on every read, and the type that answers would depend on who wrote last. So on a config mismatch a read re-reads the file: if the manifest already names the file's config it adopts it and publishes nothing, if the file names this process's config it publishes, and otherwise it publishes nothing and says `refresh: skipped, config changed elsewhere`. Reloading the config in `Engine` is a design change with its own risks and is not in this phase. Reopen it when a user reports that a server needed a restart to see a config edit. When the manifest already names the file's config, a pinned process adopts it: it answers with the new cached metadata while its per-call values — authority, decay, scoring — stay the ones it started with, and `status` keeps reporting stale for that process. That is the one case where a read is silent and the index is current, and the two answers differ on purpose. |
| M7-42 | A null value's span is empty and sits directly after the key's colon; the rendered value carries its own separator space | §2.1 says the span starts at the value's first byte, and a null has no byte. The point after the colon keeps the spacing before a trailing comment where the file put it, so `status:  # none` becomes `status: superseded  # none` and not `status: superseded# none`. Wrong if a later phase needs to put a value anywhere but directly after the colon. |
| M7-43 | A refusal costs one key, except a line at column 0 that is not a key | A quoted key, or a name with a space, is a real key that no patch can name: it is reported `Unsupported(Nested)` and left alone, and the other keys still splice. A column-0 line with no key at all means the block is not a mapping, so no key in it can be spliced safely and the whole block is refused. Wrong if a vault reports "cannot patch this file" for a file whose only oddity is one unreachable key. |
| M7-44 | `changed_keys` names a key only when its bytes differ after the patch; an absent key always differs | A receipt describes bytes (M7-14), so a patch that writes the value the file already had reports no change and the same `old_hash` and `new_hash`. A caller can then tell "nothing to do" from "wrote something". Wrong if a caller needs a receipt to list a key it wrote with an unchanged value. |
| M7-45 | `CommitOptions` carries the corpus root and the `Config`, and `commit` resolves the lock directory with `config::resolve_index_dir` | One resolution, one lock file. A free-form directory field lets one caller pass `root/.janus` and another pass a moved `index_dir`; both verify the same etag, both rename, and the first write is lost with no error. The function is the one the builder calls, so the writer cannot drift from it. Wrong if the corpus root must be readable and still the lock is not found. |
| M7-46 | M7-3 lands as one PR at 508 logic and 570 test lines, against the phase ceiling of 300 and 450 and `AGENTS.md`'s 600 and 900 | Round 1 measured 328/366. Round 2 added the owner copy, the mode policy, the resolved lock directory, the resolved link, the unsynced answer and four cases, and each one closes a way to lose or misreport a file. 195 of the 478 added lines in the two new files are comments the public-item gate and the safety argument need, so reaching 300 means deleting the reason the checks exist. The split seam is not taken: `commit` cannot be reviewed without the `replace` it calls, and a second read of one interface costs the attention the ceiling protects. Wrong if a reviewer reports that this diff is too large to hold in one reading. |
| M7-47 | A temp file left by a killed process is reported by `doctor` in M8, never swept by the writer | A crash between the create and the rename leaves one `.tmp` file in a document's directory. A sweeper that removes files it did not create can remove a user's file that happens to start with `.tmp`, on a corpus the writer does not own. A report costs one line in `doctor` and loses nothing. Wrong if the files become common enough to hide the corpus, which the report would show. |
| M7-48 | One write switch: `--allow-write` is read once in `main`, becomes `api::Writes`, and travels in the `Fleet`; the engine's gate is the only check, and `write_refusal` reads the same value | Two switches with one meaning drift. If the adapters checked the flag and the engine did not, the CLI would need a third path, and a surface added later would answer a write it forgot to gate. If the engine checks it, a caller that skips an adapter is still refused, and a test can prove the refusal without a server. The three names whose phases have not landed keep a second refusal of their own: `write_disabled` without the flag, `refused` with it, because their phase owns them. Wrong if any surface answers a write while `Writes.allowed` is false, or if a new surface must pass a second value by hand. |
| M7-50 | M7-4 lands as three PRs, split by surface: 4a-1 (codes, switch, engine, `janus rpc`), 4a-2 (MCP, `status`, the ignored config key) and 4b (`janus set`) | The phase measured 1068 logic and 1472 test lines as two PRs, and 4a alone was 936 against the 600 ceiling: the one diff a reviewer will not read. A split by surface is testable at each step, which a split by layer is not, because each later PR adds a surface whose cases it can run. The shared mechanism — `src/api/*`, `src/error.rs`, `src/engine.rs`, `src/status.rs` — is carried whole by 4a-1, so no later PR rewrites it. 4a-1 measures 784 logic and 890 test lines against `main` — 6 of the logic lines are the size script fix below — so the tests fit (890 of 900) and the logic does not: about 540 of the lines are code and 190 are comments the public-item gate and the safety argument need. `scripts/pr-size.sh` sliced the new side of a deleted file from the wrong offset, so the 135 deleted lines of `tests/mcp_gate.rs` were reported as logic; a deletion now costs nothing. Cutting to 600 means deleting the reason the eight codes and the etag checks exist, so the number is reported instead. The next seam, if it is still too large, is a fourth PR holding only the eight codes, their table and the receipt type. 4a-2 and 4b report their own numbers in their bodies. The fourth split was considered and rejected: a codes-only PR leaves a code table with no path that raises it, and a table-only review is worse than a larger one whose paths are proven. Wrong if a reviewer reports that 4a-1 is too large to hold in one reading. |
| M7-52 | A write needs a `.md` or `.markdown` target; a Markdown file that `[index] exclude` keeps out of search stays writable | An etag is not a secret: a caller can hash any file, so `expected_hash` cannot be what keeps a write on a document. The extension is the corpus's own admission rule (`vault::is_markdown`), and a write uses it, so a copied document under `.txt` is refused with `usage`. The exclude rule shapes the index, not the corpus's documents: a draft kept out of search is still a file the user may edit, and refusing it would make an index setting decide a write. Wrong if a user needs to write a document whose extension the walk does not admit. |

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

**Amendment.** The seam is taken, because the phase measures 945 logic lines as one change
against a ceiling of 600. Part 1 is branch `m7-2-scan` and holds `src/write/scan.rs`, the
public surface it needs, and the shape half of the cases. Part 2 is branch `m7-2-patcher`,
opened from part 1, and holds `patch.rs`, `value.rs` and every case that asserts a splice or a
refusal.

Part 1 measures 595 logic and 516 test lines against `main`; part 2 measures 376 logic and
855 test lines against part 1. The two parts sum to 971 logic and 1371 test lines, and the
phase as one change measures 945 and 1349, because part 2 rewrites part of part 1's
`src/write/mod.rs`.

Three things in the plan move with the split, and none of them silently:

1. `tests/goldens/frontmatter/` is not created. A golden is a literal in
   `tests/write_patch.rs`, so one changed byte shows as a changed line of a test instead of in
   a file the reviewer has to diff.
2. The refusal half of cases 1.5, 1.6, 1.8 and 1.12c arrives in part 2. A refusal is `plan`'s
   answer, and `plan` is part 2. Part 1 asserts the shape each case is about — the closed
   `ValueShape` list and the key pattern — and part 2 asserts that `plan` refuses.
3. The scanner returns one value, `write::Scan`: the fence span, the YAML extent, the newline
   and one `KeySpan` per key. §2.1 lists the key span only. Appending a key before the closing
   fence needs the block's own extent, so the type is additive and changes nothing in §2.1.

### M7-3 — The atomic replace and the lock

**Files.** `src/write/atomic.rs`, `src/write/commit.rs` (both new), `src/write/mod.rs` (the
exports), `tests/write_atomic.rs` (new). `src/index/store.rs` is untouched: `Lock::acquire`,
`LOCK_FILE` and `LOCK_TIMEOUT` were already public, so the writer reuses them and copies nothing
(M7-9).

**Build.** Temp file, `fsync`, `rename`, parent `fsync`, mode preserve; lock, re-read, verify,
and the two `Hooks` seams (M7-33).

**Tests.** Cases 2.4, 2.4a, 2.4b, 2.4c, 2.5, 2.6, 3.1–3.6.

**Accept.** A failed write leaves the file byte-identical and no temp file behind. A file
changed after the pre-lock check and before the lock answers `conflict`, and the test proves
it by removing the under-lock re-read and watching case 2.4a fail.

**Split seam.** The atomic replace first, then the lock and the verification. Not taken: the
phase measured over its ceiling, and M7-46 reports both numbers.

**Decisions.** M7-45 (the lock directory is resolved, not passed), M7-46 (the size) and M7-47
(a temp file left by a killed process is reported, not swept).

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

- **4a — the write tool on a server surface.** This became three PRs (M7-50), split by
  surface because a surface is testable alone.
  - **4a-1 — the codes, the switch, the engine path, and `janus rpc`.** `src/error.rs`,
    `src/api/*`, `src/engine.rs`, `src/status.rs`, `src/rpc.rs`, `src/main.rs` (the fleet
    plumbing and the rpc arm), `src/mcp/*` (the `Writes` argument and the two descriptor arms
    the new tool variant forces), `tests/write_rpc.rs`, `tests/cli_rpc.rs`, `tests/cli_mcp.rs`,
    `tests/workspace_read.rs`, `SPEC.md` §9.1, §9.3, §9.4. Cases 2.1–2.3, 2.7a, 2.7b, 2.8,
    4.2, 4.5.
  - **4a-2 — MCP, `status`, and the instructions string.** `src/mcp/*` (the list, the
    instructions string), `src/main.rs` (the MCP arm), `src/config.rs`, `src/engine.rs` (the
    config warnings), `tests/mcp_write.rs`, `tests/cli_mcp.rs`, `tests/workspace_read.rs`,
    `tests/cli_status.rs`, `SPEC.md` §9.2, §16.1. Cases 4.1, 4.3, 4.4, 4.6.

  Two files move with 4a-1, and neither silently. `tests/mcp_gate.rs` is deleted: both of its
  cases assert the M6 gate that this phase replaces, and the file says so itself.
  `tests/api_envelope.rs` is untouched: case 4.5 asserts the receipt's key set in
  `tests/write_rpc.rs`, beside the call that builds it. `tests/cli_mcp.rs` case 8.2 and
  `tests/workspace_read.rs` case 3.5 pin the MCP surface, which 4a-1 does not serve yet, so
  they answer `write_disabled` in 4a-1 and their final shape in 4a-2; each PR states that in
  its body.
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
| M7-4 `update_frontmatter` + `set`, three PRs (4a-1 codes/engine/rpc, 4a-2 MCP/status, 4b CLI) | 600 | ~600 | 900 | ~700 |
| M7-5 `write_document` | 250 | ~180 | 400 | ~250 |
| M7-6 `supersede` | 400 | ~300 | 600 | ~400 |
| M7-7 `promote` + resolution | 550 | ~450 | 750 | ~500 |
| M7-8 rehearsal | 200 | ~150 | 250 | ~150 |

M7-1 is two PRs of one phase for the same reason (M7-39): 253 logic and 426 test lines as one
PR, 124/181 and 233/437 after the review rounds, and 352/597 cumulative against `main`. The
phase ceiling is reported on 1b, not passed (M7-39).

M7-4 was 500 until the review showed that three surfaces, eight codes with doc comments and
five test files cannot fit it. It is now **three PRs of one phase**, split by surface: 4a-1
(codes, switch, engine, `janus rpc`), 4a-2 (MCP, `status`, the ignored config key) and 4b
(`janus set`). 4a-2 measures 61 logic and 4b 132, both inside the ceiling; 4a-1 measures 745,
which is over it, and M7-50 reports why rather than claiming the phase fits. M7-7 grew by the
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
6. **§7.3 step 7.** State that a new document's mode is `0o666` masked by the umask and that
   an existing document keeps its mode and owner; that a link is resolved before the write and
   a resolved path outside the corpus is refused; and that a failed directory sync after a
   successful rename is reported as a write that happened, never as a refusal (M7-45, M7-47).
7. **§7.4.** State that in-place writes `status` and `superseded_by` on the old document and
   `supersedes` on the new one; that `--append-only` creates the new document and leaves the
   old one untouched; and that no `effective_to` and no `doc_version` is written (M7-20, M7-21,
   M7-35). **Step 1 of §7.4 also changes**: the new document must not exist, and an existing
   one is `path_exists` — step 1's "validate both docs exist" is true of the old document
   only. This closes open questions 4 and 23 for this milestone.
8. **§8.3.** Add `config_hash`, the four sections it covers, the reason for each section that
   is outside it, and the refresh-trigger rule for a manifest without it (M7-24, M7-25, M7-32).
9. **§6.11.** State that a qualified cross-corpus link resolves when
   `allow_cross_corpus_links` is true, and that `get_lineage` follows it (M7-37).
10. **§16.1.** Remove `[mcp] allow_write` from the shipped config and state that it is ignored
   with a warning (M7-36).
11. **§11 M7.** Replace the done-when with the numbered rows of §8, including the rehearsal
    phase and the fixture rule.

## 11. Handoff

- **M8** inherits the writer: `watch` must not race it, and the mmap load path must read a
  generation a write did not build. M8 also inherits cross-corpus *supersession*, which §0.2
  keeps out of M7 because it changes another corpus's windows and status. And `doctor` reports
  stray temp files in a document directory in M8, because a killed writer leaves them (M7-47).
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
