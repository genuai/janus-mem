# M7b — The writer's usability

> Branch: `m7b-plan` (this document), then one branch per phase from `main`.
> Spec: §7.3, §9.1, §9.3, §9.4, §6.3, §11 (M7 done), §15.
> Evidence: the M7 write rehearsal of 2026-11-12 on a complete copy of a 2,407-document
> corpus, and the gate of the same date on two corpora.
>
> **Done when:** a promotion completes on a document with no frontmatter block (today it cannot,
> on 79% of that corpus), a write whose value is already right changes **0** bytes (today 39 of
> 200 do), and `write --overwrite` reports only the keys whose bytes changed.

**Words used here.** **Block**: the `---` … `---` frontmatter region. **Promotion**: setting a
document's `type`, `status` and `date` so it climbs the type ladder. **Idempotent**: a second
write with the same value changes nothing. **Parse-back**: writing a value and reading it through
the config's typed reader to prove it survived.

## 0. Scope

**Decision: M7b fixes what the writer cannot do on a real vault. It adds no tool, no ranking
change and no index change.**

M7 shipped four write tools that are correct and safe. The rehearsal then showed that one of them
cannot be used on four documents in five, and that two receipts say more than they did.

| M7b item | state today | evidence |
|---|---|---|
| `create_frontmatter` on a tool | absent; the flag exists on `Patch` and every engine path passes `false` | `src/write/patch.rs:31`, `src/engine.rs:377`, `:451`, `:650` |
| a write on a document with no block | refused | `src/write/patch.rs:83` |
| documents with no block, measured | 1,906 of 2,407 (79%) | rehearsal of 2026-11-12 |
| a write whose value is already right | changes bytes when the quoting differs | `src/write/patch.rs:109` compares bytes, not values |
| same-value writes that changed bytes | 39 of 200 (24 gained quotes, 15 lost them) | rehearsal of 2026-11-12 |
| `write_document` `changed_keys` | every key in the request | `src/write/document.rs:93` |
| value validation on a write | none; `date=not-a-date` and `status=nonsense-value` are accepted | rehearsal of 2026-11-12 |
| the status ladder a write may store | four declared values plus the `unknown` fallback | `src/document.rs:255` |
| what the read path accepts as a date | `YYYY-MM-DD` and a truncated ISO timestamp | `src/vault/frontmatter.rs:332` |
| `promote_document`'s write | one key, `supersedes`, in `link` mode | `src/engine.rs:369`, `src/write/promote.rs:75` |

### 0.1 In scope

1. `create_frontmatter` as an explicit parameter on `update_frontmatter` and on `janus set`, and
   as the default for `promote_document --link-from`, whose target needs a block to hold
   `supersedes`.
2. A block created after any BOM, before the body, with the file's newline, and the body
   untouched.
3. Value equality: a patch write compares the **parsed** value, so a value that is already right
   changes no byte.
4. `changed_keys` in every receipt means "the bytes differ", and `appended_keys` means "the file
   did not have this key".
5. Validation of the four keys the config interprets and a write can get wrong: `status`,
   `date`, `effective_from`, `effective_to`.
6. The rehearsal's 200-document fixture gains documents with no block and a real-write pass, so
   the create path and the idempotency count are measured, not only asserted.

### 0.2 Not in scope

| Item | Owner | Why |
|---|---|---|
| Validating `type` | later | types are an open registry (D34, §6.3): an unknown type keeps its name and gets the neutral weight plus a `doctor` note. Closing that on write is a product decision, not a bug fix. |
| Searchable frontmatter values | R2 | it changes what a chunk holds, so it changes the index format and every score |
| The ordering gap, semantic ranking | R2 | its trigger is unchanged: three wording failures in ten; measured two |
| `--bump-version`, key deletion, the `deprecated` merge | later | each is additive and none blocks a workflow |
| Cross-corpus supersession | M8 | a link that changes another corpus's windows and status |
| `watch`, mmap, quantisation, release | M8 | unchanged |

### 0.3 Why four phases

| Phase | Unit | Why it is one reviewable change |
|---|---|---|
| M7b-1 | `create_frontmatter` | one parameter, one insertion, one receipt field; it unblocks promotion |
| M7b-2 | value equality | one comparison, and every no-op patch write in the product changes behaviour |
| M7b-3 | receipt accuracy | one field's meaning, in two writers |
| M7b-4 | validation | one predicate per key, and the code it raises |

## 1. Dependencies

None.

## 2. Data model

### 2.1 The parameter

`update_frontmatter` and `janus set` gain `create_frontmatter` (a boolean, default `false`).
`promote_document --link-from` sets it to `true`, because its target must hold `supersedes` and a
promotion that cannot create the block it needs is the defect this milestone exists for.

`Patch.create_frontmatter` already exists (`src/write/patch.rs:31`) and already inserts the block
correctly. The work is to reach it, not to rebuild it.

**A promotion is a `set`, not a `promote`.** The type ladder is climbed by writing `type`,
`status` and `date` through `update_frontmatter` / `janus set`. `promote_document` is a different
write: it declares cross-corpus lineage and writes one key, `supersedes`. Both need
`create_frontmatter`; only the first climbs the ladder.

### 2.2 The equality rule

```text
old      = the bytes the key's value span holds
rendered = render_value(the requested value)
equal    = read_back(rendered) == read_back(old)
changed  = !equal && rendered != old_bytes
```

`changed` decides the edit. Two tests, because either alone is wrong:

- `read_back` compares what the config's typed reader would report, so `status: Active` and a
  request for `active` are equal (the reader canonicalises status), and a free-text key
  `owner: Active` and a request for `active` are not.
- `rendered != old_bytes` is the belt: a value that renders to the bytes already there changes
  nothing, whatever the reader says. Without it a write reports a change it did not make.

When `changed` is false, no edit is planned, `changed_keys` omits the key, and the file keeps its
bytes, its quoting and its trailing comment.

**Two values are one value.** An alias and its canonical name (`type: adr` and `type: decision`)
read back the same, and so do a timestamp and its date (`2026-10-29T10:00:00Z` and
`2026-10-29`). A write that asks for the canonical form of a value already stored therefore
changes nothing and reports `changed_keys: []`. That is the rule, not a gap: a caller that wants
the bytes normalised asks for a different value, or edits the file.

**An empty span reads as null.** A key with no value bytes (`status:`) has an empty span, and
`read_back("")` is `null`. A request for a string is therefore a change; a request for `null` is a
no-op.

**Scope.** This rule is every writer's, `write_document` included: it renders the block from the
request, and a key it re-renders differently does differ in bytes, so it appears in
`changed_keys`. A key it renders identically does not.

`plan` and `plan_document` gain a `&Config`, because resolving an alias and reading a value back
needs the type registry and the field aliases. They stay pure: no file is read. Every call site
moves with them, and §5 names the files.

### 2.3 The receipt

| Field | Means |
|---|---|
| `changed_keys` | keys whose bytes differ after the write, sorted |
| `appended_keys` | keys the file did not have, sorted; every key of a created block is here |
| `old_hash` | the hash of the bytes before, or `null` for a create |
| `new_hash` | the hash of the bytes written |
| `created_block` | **new**, `true` when this write inserted the block |

`created_block` is additive; `api` stays 1.

### 2.4 Validation

A write validates the keys the config interprets **and** a caller can get wrong. Every other key
is stored as the caller wrote it, because the type registry and the alias lists are open.

| Key | Accepted | Refused |
|---|---|---|
| `status` | `active`, `draft`, `superseded`, `deprecated`, `unknown`, `null` | anything else |
| `date`, `effective_from`, `effective_to` | exactly what `vault::frontmatter::parse_date` accepts (`src/vault/frontmatter.rs:332`), plus `null` | anything else |

The accepted date set is **derived from the parser**, not written down here: a write must not be
stricter than a read of the same file, and it must not store a date the reader would drop. `null`
is a value (`effective_to: null` is canonical, SPEC §6.3), not an absence.

`type` is not validated. An unknown type keeps its name and gets the neutral weight plus a
`doctor` note (§6.3, D34).

A refused value is `invalid_value`, exit 4, whose message names the key, the value and the
accepted set. It is a refusal, not a usage error: the request is well formed and the value is
not storable, and a retry with the same request cannot succeed.

## 3. Contracts

### 3.1 `create_frontmatter` (P1)

**Case 1.1.** `set` with `create_frontmatter` on a document with no block inserts
`---\n<keys>\n---\n` before the body, leaves the body byte-identical, and the receipt reports
`created_block: true` and every key in `appended_keys`.

**Case 1.2.** The same write without the parameter still answers `no_frontmatter`, and the file
is byte-identical.

**Case 1.3.** A document with a UTF-8 BOM keeps the BOM at byte 0, before the created block.

**Case 1.4.** A CRLF document gets a CRLF block, and the body keeps every CRLF.

**Case 1.5.** A body whose first line is `---` is a body: the created block goes above it, and
the stray fence stays the body's first line. The read-back names the requested keys.

**Case 1.6.** A promotion — one `set` writing `type=decision`, `status=active` and
`date=<today>` — completes on a document with no block in one call, and the document then
resolves `decision` and `active` on the next read.

**Case 1.7.** A create on a document that already has a block is a no-op for the block: the
existing keys are patched, and `created_block` is `false`.

**Case 1.8.** `promote_document --link-from` on a target with no block creates one holding
`supersedes`, and the source corpus is byte-identical.

### 3.2 Value equality (P2)

**Case 2.1.** A value written quoted (`status: "active"`) patched to `active` changes **0**
bytes, and `changed_keys` is empty.

**Case 2.2.** The same bare value (`status: active`) written again changes **0** bytes and reports
`changed_keys: []`. A request is a value, not a rendering: `"active"` with the quote characters in
it is a different string, and it writes.

**Case 2.3.** A value with a trailing comment (`status: active  # current`) patched to `active`
keeps the comment and changes **0** bytes.

**Case 2.4.** `status: Active` patched to `active` changes **0** bytes, because the reader
canonicalises both to `active`.

**Case 2.5.** `owner: Active` patched to `active` **does** change, because a free-text value is
not canonicalised.

**Case 2.6.** A value that differs writes, and the span is replaced exactly as M7-2 defined.

**Case 2.7.** A real write (not a dry run) on a temporary copy, run twice with the same value:
the second run changes **0** bytes, and its receipt's `old_hash` equals its `new_hash`.

**Case 2.8.** The rehearsal reports **0** same-value writes that change a byte, down from 39.

**Case 2.9.** `type: adr` patched to `decision` changes **0** bytes, because the two names resolve
to one type.

**Case 2.10.** A key with an empty value (`status:`) patched to a string changes; patched to
`null` changes **0** bytes.

### 3.3 Receipt accuracy (P3)

**Case 3.1.** `write_document --overwrite` reports in `changed_keys` only the keys whose bytes
differ from the old file, and in `appended_keys` only the keys the old file lacked.

**Case 3.2.** A key the old block had, rewritten with the **same bytes**, appears in neither
list.

**Case 3.3.** A created block reports every key in `appended_keys` and `created_block: true`.

**Case 3.4.** `changed_keys` and `appended_keys` are sorted and deduplicated.

### 3.4 Validation (P4)

**Case 4.1.** `date=not-a-date` answers `invalid_value`, exit 4, names the key and the value, and
writes nothing.

**Case 4.2.** `status=nonsense` answers `invalid_value`, exit 4, and writes nothing.

**Case 4.3.** Every form `parse_date` accepts is accepted by the write, proved by a table test
that reads the parser's own cases; and `date=2026-11`, which the parser does not accept, is
refused. The two sides cannot drift.

**Case 4.4.** `effective_to=null` is accepted and stores `null`.

**Case 4.5.** `type=nonsense` is **accepted**, the document keeps that name, and `doctor` reports
it. The registry stays open.

**Case 4.6.** A key the config does not interpret (`owner=café`) is stored unchanged.

**Case 4.7.** Validation applies to every write path: `update_frontmatter`, `write_document`,
`supersede` (both documents) and `promote` (the adopted copy and the created block).

### 3.5 What must not change

1. Every read golden stays byte-identical.
2. `format_version` stays 6. `api` stays 1. No exit code moves; one code (`invalid_value`, exit
   4) and one receipt field are added.
3. No new dependency.
4. A write still never refreshes the index (M7-17).
5. A refused write still writes nothing and leaves the file byte-identical.

## 4. Decisions

| ID | Decision | Why |
|---|---|---|
| M7b-1 | `create_frontmatter` is an explicit parameter, default `false` | A write that invents structure must be asked for. A promotion asks for it, because a promotion without it is the defect this milestone exists for. |
| M7b-2 | The created block is `---\n<key: value>\n---\n`, after any BOM, before the body, in the file's newline | It is the shape `write_document` already writes, so two paths cannot disagree. |
| M7b-3 | A patch write compares the **parsed** value of the rendered request with the parsed value of the span, and skips the edit when they are equal | A value that is already right must not change the file. Quoting is a rendering choice, not a value. |
| M7b-4 | The byte test is a belt beside the parsed test: a value that renders to the bytes already there never changes | The parsed test alone reports a change it did not make when the reader canonicalises a value (`status: Active`). |
| M7b-5 | A no-op write reports `changed_keys: []` and equal hashes, and writes nothing | A receipt describes bytes (M7-14), so a no-op must be visible as one. |
| M7b-6 | `created_block` is a new receipt field, additive, `api` stays 1 | A caller that cannot tell "patched" from "created the block" cannot report the change to a user. |
| M7b-7 | Validation covers `status` and the three date keys, and nothing else | Those four are the keys the config interprets and a caller can get wrong. Every other key is the user's. |
| M7b-8 | `type` is not validated; an unknown type keeps its name and `doctor` reports it | Types are an open registry (D34, §6.3). Closing it on write is a product decision, and this milestone is a bug fix. |
| M7b-9 | The accepted date set is derived from `parse_date`; `null` is accepted on `status` and on the three date keys | A write must not be stricter than a read of the same file. `status: null` reads as the default today, so refusing it on write would be the asymmetry this row forbids. |
| M7b-10 | A refused value is `invalid_value`, exit 4 | The request is well formed and the value is not storable. A bad key name is already exit 4; a bad value cannot be exit 2. |
| M7b-11 | `changed_keys` means "bytes differ" in every writer, `write_document` included | A whole-file write re-renders the block, so a key it re-quotes does differ in bytes and does appear. The receipt reports what changed, not what was written. |
| M7b-12 | `plan` and `plan_document` gain a `&Config` | Resolving an alias and reading a value back needs the registry and the aliases. They stay pure: no file is read. |
| M7b-13 | `promote_document --link-from` sets `create_frontmatter` | The promotion instruction in the MCP `instructions` string must be executable on a note that has no block. |
| M7b-14 | The rehearsal fixture gains documents with no block, and a real-write pass | A measurement that cannot reach the create path measures nothing about it. |

## 5. Phases

### Phase 1 — `create_frontmatter`

**Files.** `src/write/patch.rs` (the parameter's doc and the receipt field), `src/write/document.rs`
(the receipt), `src/api/dto.rs`, `src/engine.rs` (three call sites), `src/mcp/tools.rs` (the
schema), `src/mcp/protocol.rs` (the `instructions` string, which names the new parameter),
`src/cli.rs`, `src/main.rs`, `scripts/write-rehearsal.sh` (the promote expectation flips from
refusal to success), `tests/write_rpc.rs`, `tests/cli_set.rs`, `tests/write_patch.rs`,
`tests/write_rehearsal.rs`.

**Build.** The parameter, the insertion, `created_block`, the promotion default, and the script's
expectation.

**Tests.** Cases 1.1 to 1.8.

**Accept.** A promotion completes on a document with no block, the body is byte-identical, and the
rehearsal passes with the create path exercised.

**Split seam.** The engine and rpc first; the CLI flag and the script second.

### Phase 2 — Value equality

**Files.** `src/write/patch.rs` (the comparison), `src/write/document.rs` (the definition site of
`plan_document`, whose signature gains the config), `src/write/value.rs` (the parse-back, reused),
`src/write/supersede.rs`, `src/write/promote.rs`, `src/engine.rs` (passing the config),
`src/write/mod.rs`, `tests/write_atomic.rs`, `tests/write_patch.rs`, `tests/cli_set.rs`,
`tests/cli_supersede.rs`, `tests/write_rehearsal.rs` (the real-write pass).

**Build.** The two tests of §2.2, the `&Config` threading, and the real-write idempotency pass.

**Tests.** Cases 2.1 to 2.8.

**Accept.** A second identical write changes 0 bytes, on a temporary copy, and the rehearsal
reports 0 same-value changes.

**Split seam.** The comparison first; the `&Config` threading second, as its own commit in the
same PR.

### Phase 3 — Receipt accuracy

**Files.** `src/write/document.rs` (the comparison against the old block), `src/api/dto.rs`,
`tests/cli_write.rs`.

**Build.** `changed_keys` and `appended_keys` computed from the old file.

**Tests.** Cases 3.1 to 3.4.

**Accept.** A key rewritten with the same bytes appears in neither list.

**Split seam.** None.

### Phase 4 — Validation

**Files.** `src/write/validate.rs` (new), `src/write/patch.rs`, `src/write/document.rs`,
`src/engine.rs`, `src/api/error.rs` (the code and its row), `src/error.rs` (the variant and its
exit code), `src/mcp/tools.rs`, `tests/write_validate.rs` (new), `docs/corpus-config.md`.

**Build.** One predicate per key, one message, one code, and six call sites.

**Tests.** Cases 4.1 to 4.7.

**Accept.** A nonsense date and a nonsense status are refused on every write path, and every form
the parser accepts is accepted by the write.

**Split seam.** The predicate and its table test first; the six call sites second.

## 6. Cross-cutting requirements

- The gates of `AGENTS.md`, unchanged.
- Every test is deterministic: a fixed clock, a `tempfile` corpus, no sleep, no network.
- A no-op write is proved by a byte comparison of the whole file.
- After each phase merges, rebuild the local install and re-run the ten questions.

## 7. The evaluation packet

The repository's ten questions are `tests/eval/repo-ten.jsonl`. M7b changes no ranking, so it
claims **no** gain on them. The baseline after M7 is top-1 **4/10**, top-3 **9/10**, p50 6 ms.

The private corpus's ten questions stay out of this repository. Its counts are recorded outside
it: A 4, B 2, C 4 with metadata, A 4, B 3, C 3 with metadata neutralised.

The product claims M7b makes are measured in the repository, on generated fixtures: a promotion
completes on a no-block document, and 0 same-value writes change a byte.

## 8. Definition of done

1. A promotion completes on a document with no block: **1 of 1** by case 1.6, and the rehearsal's
   no-block documents all admit a patch afterwards: **N of N**, where N is the count the fixture
   builds.
2. A second identical write changes **0** bytes, proved by a real write on a temporary copy (case
   2.7), and the rehearsal reports **0** same-value changes, down from 39 of 200.
3. `write --overwrite` reports only keys whose bytes differ: **0** extra keys in case 3.2.
4. A nonsense `date` and a nonsense `status` are refused on every write path: **2 of 2**, and
   **6 of 6** paths for case 4.7.
5. Read goldens: **0** move.
6. `format_version` **6**, `api` **1**, **0** exit codes moved, **1** code added
   (`invalid_value`, exit 4), **1** receipt field added.
7. `cargo doc --no-deps`: **0** warnings.
8. Retrieval: **0** claims.

## 9. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate |
|---|---|---|---|---|
| M7b-1 `create_frontmatter` | 250 | ~220 | 450 | ~350 |
| M7b-2 value equality | 250 | ~200 | 400 | ~300 |
| M7b-3 receipt accuracy | 150 | ~100 | 250 | ~150 |
| M7b-4 validation | 250 | ~200 | 400 | ~300 |

M7's estimates ran two to four times low because each phase's real unit was "a tool and its
surfaces". These four are smaller, but Phase 2 threads a `&Config` through every call site, which
is mechanical churn that the logic ceiling counts. The estimate is raised for it. Each phase names
its split seam and splits before its ceiling.

## 10. SPEC corrections

1. **§9.3.** `update_frontmatter` gains `create_frontmatter?`, and `promote_document` sets it in
   `link` mode.
2. **§9.1.** `janus set` gains `--create-frontmatter`.
3. **§6.3.** Replace "Files with no frontmatter … are never mutated to add one unless the user
   explicitly calls `write_document`" with: a block is added only when the request asks for it,
   through `update_frontmatter`, `janus set`, `promote_document --link-from`, or
   `write_document`. §7.3 step 6 already says "when `create_frontmatter: true`" and needs no
   change.
4. **§7.3 step 5.** A key changes when its parsed value differs **and** its bytes would differ.
   This supersedes M7-44's byte-only rule.
5. **§6.3.** Name the four keys a write validates, that the accepted date forms are the parser's,
   and that `type` is not validated because the registry is open.
6. **§9.4.** `changed_keys` means "bytes differ", `appended_keys` means "the file did not have
   it", and `created_block` is new.
7. **§9.1.** The error-code table gains `invalid_value`, exit 4.
8. **§11.** Insert M7b between M7 and M8, and state that M8 inherits `watch`, mmap and
   cross-corpus supersession.

## 11. Handoff

- **M8** inherits the writer: `watch` must not race it, and `doctor` reports stray temp files.
- **R2** inherits the searchable-frontmatter question and the ordering gap. Its trigger is
  unchanged, and the count after M7 was two in ten.
- **The decay off-switch** was fixed in its own PR before this milestone started.
- **Validating `type`** is deliberately not done here. It is a product decision about an open
  registry, and it is recorded in §0.2 so the next plan does not have to rediscover it.

## 12. Implementation logs

Empty. One entry per phase, appended when the phase merges.
