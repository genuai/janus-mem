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
| the status ladder a write may store | five declared values plus `unknown` | `src/document.rs:255` |

### 0.1 In scope

1. `create_frontmatter` as an explicit parameter on `update_frontmatter` and on
   `janus set`, and as the default for a promotion.
2. A block created after any BOM, before the body, with the file's newline, and the body
   untouched.
3. Value equality: a write compares the **parsed** value, so a value that is already right
   changes no byte.
4. `changed_keys` in every receipt means "the bytes differ", and `appended_keys` means "the file
   did not have this key".
5. Validation of the five keys the config interprets: `type`, `status`, `date`,
   `effective_from`, `effective_to`.
6. The rehearsal's 200-document fixture gains documents with no block, so the create path is
   measured and not only asserted.

### 0.2 Not in scope

| Item | Owner | Why |
|---|---|---|
| Searchable frontmatter values | R2 | it changes what a chunk holds, so it changes the index format and every score |
| The ordering gap, semantic ranking | R2 | its trigger is unchanged: three wording failures in ten; measured two |
| `--bump-version`, key deletion, the `deprecated` merge | later | each is additive and none blocks a workflow |
| Cross-corpus supersession | M8 | a link that changes another corpus's windows and status |
| `watch`, mmap, quantisation, release | M8 | unchanged |

### 0.3 Why four phases

| Phase | Unit | Why it is one reviewable change |
|---|---|---|
| M7b-1 | `create_frontmatter` | one parameter, one insertion, one receipt field; it unblocks promotion |
| M7b-2 | value equality | one comparison, and every no-op write in the product changes behaviour |
| M7b-3 | receipt accuracy | one field's meaning, in two writers |
| M7b-4 | validation | one predicate per key, and the codes it raises |

## 1. Dependencies

None.

## 2. Data model

### 2.1 The parameter

`update_frontmatter` and `janus set` gain `create_frontmatter` (a boolean, default `false`).
`promote_document` sets it to `true` in `link` mode, because a promotion that cannot create the
block it needs is the defect this milestone exists for.

`Patch.create_frontmatter` already exists (`src/write/patch.rs:31`) and already inserts the block
correctly. The work is to reach it, not to rebuild it.

### 2.2 The equality rule

```text
old  = the bytes the key's value span holds
new  = render_value(the requested value)
equal = read_back(old) == the requested string
```

`equal` decides whether the key changes. When it is true, no edit is planned, `changed_keys`
omits the key, and the file keeps the bytes it had, including its quoting and its trailing
comment. When it is false, the value's span is replaced with `new`, exactly as today.

The parse-back uses the same reader the config uses, so a value that would read back as a number
or a boolean is compared as the string the reader would report.

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

A write validates only the keys the config interprets. Any other key is stored as the caller
wrote it, because the type registry and the alias lists are open.

| Key | Accepted | Refused |
|---|---|---|
| `type` | a canonical type or a configured alias | anything else |
| `status` | `active`, `draft`, `superseded`, `deprecated`, `unknown` | anything else |
| `date`, `effective_from`, `effective_to` | `YYYY-MM-DD`, `YYYY-MM`, `YYYY` | anything else |

A refused value is a `usage` failure, exit 2, whose message names the key, the value and the
accepted set. It is a caller mistake: a retry with the same request cannot succeed.

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

**Case 1.6.** A promotion on a no-block document completes with one call, and the document then
resolves the type the request named.

**Case 1.7.** A create on a document that already has a block is a no-op for the block: the
existing keys are patched, and `created_block` is `false`.

### 3.2 Value equality (P2)

**Case 2.1.** A value written quoted (`status: "active"`) patched to `active` changes **0**
bytes, and `changed_keys` is empty.

**Case 2.2.** A value written bare (`status: active`) patched to `"active"` changes **0** bytes.

**Case 2.3.** A value with a trailing comment (`status: active  # current`) patched to `active`
keeps the comment and changes **0** bytes.

**Case 2.4.** A value that differs writes, and the span is replaced exactly as M7-2 defined.

**Case 2.5.** The 200-document rehearsal reports **0** same-value writes that change a byte,
down from 39.

**Case 2.6.** A document's hash is unchanged after a no-op write, and its receipt's `old_hash`
equals its `new_hash`.

### 3.3 Receipt accuracy (P3)

**Case 3.1.** `write_document --overwrite` reports only the keys whose bytes differ from the old
file, and `appended_keys` only the keys the old file lacked.

**Case 3.2.** A key the old block had, rewritten with the same value, appears in neither list.

**Case 3.3.** A created block reports every key in `appended_keys` and `created_block: true`.

**Case 3.4.** `changed_keys` and `appended_keys` are sorted and deduplicated.

### 3.4 Validation (P4)

**Case 4.1.** `date=not-a-date` answers `usage`, exit 2, names the key and the value, and writes
nothing.

**Case 4.2.** `status=nonsense` answers `usage` and writes nothing.

**Case 4.3.** `type=nonsense` answers `usage`; `type=adr` (a configured alias) is accepted and the
document resolves to `decision`.

**Case 4.4.** `date=2026-11` is accepted and reads back as the first day of the month, which is
what the read path already does.

**Case 4.5.** A key the config does not interpret (`owner=café`) is stored unchanged.

**Case 4.6.** Validation applies to every write path: `update_frontmatter`, `write_document`,
`supersede` (both documents) and `promote` (the adopted copy).

### 3.5 What must not change

1. Every read golden stays byte-identical.
2. `format_version` stays 6. `api` stays 1. No exit code moves; one receipt field and no code is
   added.
3. No new dependency.
4. A write still never refreshes the index (M7-17).
5. A refused write still writes nothing and leaves the file byte-identical.

## 4. Decisions

| ID | Decision | Why |
|---|---|---|
| M7b-1 | `create_frontmatter` is an explicit parameter, default `false` | A write that invents structure must be asked for. A promotion asks for it, because a promotion without it is the defect this milestone exists for. |
| M7b-2 | The created block is `---\n<key: value>\n---\n`, after any BOM, before the body, in the file's newline | It is the same shape `write_document` already writes, so two paths cannot disagree. |
| M7b-3 | A write compares the **parsed** value, not the bytes | A value that is already right must not change the file. Quoting is a rendering choice, not a value. |
| M7b-4 | A no-op write reports `changed_keys: []` and equal hashes, and writes nothing | A receipt describes bytes (M7-14), so a no-op must be visible as one. |
| M7b-5 | `created_block` is a new receipt field, additive, `api` stays 1 | A caller that cannot tell "patched" from "created the block" cannot report the change to a user. |
| M7b-6 | Validation covers exactly the five keys the config interprets | Any other key is the user's; refusing it would close an open registry (D34). |
| M7b-7 | A refused value is `usage`, exit 2 | The caller can fix the invocation, and a retry with the same request cannot succeed. |
| M7b-8 | `type` is validated against the registry, so an alias resolves and a typo is refused | A typo silently becomes `unknown`, which loses the document's authority without saying so. |
| M7b-9 | Dates accept the three forms the read path already parses | A write must not be stricter than a read of the same file. |
| M7b-10 | `changed_keys` means "bytes differ" in every writer | Two writers that disagree about one field's meaning is the drift this milestone removes. |
| M7b-11 | `promote_document` sets `create_frontmatter` in `link` mode | The promotion instruction in the MCP `instructions` string must be executable on a note that has no block. |
| M7b-12 | The rehearsal fixture gains documents with no block | A measurement that cannot reach the create path measures nothing about it. |

## 5. Phases

### Phase 1 — `create_frontmatter`

**Files.** `src/write/patch.rs` (the parameter's doc and the insertion's receipt field),
`src/write/document.rs` (the receipt), `src/api/dto.rs`, `src/engine.rs` (three call sites),
`src/mcp/tools.rs` (the schema), `src/cli.rs`, `src/main.rs`, `tests/write_rpc.rs`,
`tests/cli_set.rs`, `tests/write_patch.rs`.

**Build.** The parameter, the insertion, `created_block`, and the promotion default.

**Tests.** Cases 1.1 to 1.7.

**Accept.** A promotion completes on a document with no block, and the body is byte-identical.

**Split seam.** The engine and rpc first; the CLI flag second.

### Phase 2 — Value equality

**Files.** `src/write/patch.rs` (the comparison), `src/write/value.rs` (the parse-back, reused),
`tests/write_patch.rs`, `tests/cli_set.rs`, `scripts/write-rehearsal.sh` (case 2.5's count).

**Build.** Compare the parsed value; plan no edit when it is equal.

**Tests.** Cases 2.1 to 2.6.

**Accept.** The rehearsal reports 0 same-value writes that change a byte.

**Split seam.** None. It is one comparison.

### Phase 3 — Receipt accuracy

**Files.** `src/write/document.rs` (the comparison against the old block),
`src/api/dto.rs`, `tests/cli_write.rs`.

**Build.** `changed_keys` and `appended_keys` computed from the old file, not from the request.

**Tests.** Cases 3.1 to 3.4.

**Accept.** A rewritten-identical key appears in neither list.

**Split seam.** None.

### Phase 4 — Validation

**Files.** `src/write/validate.rs` (new), `src/write/patch.rs`, `src/write/document.rs`,
`src/engine.rs`, `src/api/error.rs` (the message, not a new code), `src/mcp/tools.rs`,
`tests/write_validate.rs` (new), `docs/corpus-config.md` (the accepted values).

**Build.** One predicate per interpreted key, and one message that names the key and the set.

**Tests.** Cases 4.1 to 4.6.

**Accept.** A nonsense date and a nonsense status are refused on every write path.

**Split seam.** The predicate first; the four call sites second.

## 6. Cross-cutting requirements

- The gates of `AGENTS.md`, unchanged.
- Every test is deterministic: a fixed clock, a `tempfile` corpus, no sleep, no network.
- A no-op write is proved by a byte comparison of the whole file, not a parsed one.
- After each phase merges, rebuild the local install and re-run the ten questions.

## 7. The evaluation packet

The repository's ten questions are `tests/eval/repo-ten.jsonl`. M7b changes no ranking, so it
claims **no** gain on them. The baseline after M7 is top-1 **4/10**, top-3 **9/10**, p50 6 ms.

The private corpus's ten questions stay out of this repository. Its counts are recorded outside
it: A 4, B 2, C 4 with metadata, A 4, B 3, C 3 with metadata neutralised.

The product claims M7b makes are measured, not graded: a promotion completes on a no-block
document, and 0 same-value writes change a byte.

## 8. Definition of done

1. A promotion completes on a document with no block: **1 of 1** by case 1.6, and the patchable
   share of the private corpus rises from 501 of 2,407 documents (21%) to **2,407 of 2,407**.
2. Same-value writes that change a byte: **0 of 200**, down from 39.
3. `write --overwrite` reports only keys whose bytes differ: **0** extra keys in the probe.
4. A nonsense `date` and a nonsense `status` are refused on every write path: **2 of 2**, and
   **6 of 6** paths for case 4.6.
5. Read goldens: **0** move.
6. `format_version` **6**, `api` **1**, **0** exit codes moved, **0** codes added, **1** receipt
   field added.
7. `cargo doc --no-deps`: **0** warnings.
8. Retrieval: **0** claims.

## 9. PR plan and size budget

| phase | logic ceiling | estimate | test ceiling | estimate |
|---|---|---|---|---|
| M7b-1 `create_frontmatter` | 250 | ~200 | 400 | ~300 |
| M7b-2 value equality | 200 | ~120 | 350 | ~200 |
| M7b-3 receipt accuracy | 150 | ~100 | 250 | ~150 |
| M7b-4 validation | 250 | ~180 | 400 | ~250 |

M7's estimates ran two to four times low because each phase's real unit was "a tool and its
surface". These four are smaller and share the surfaces that already exist, so the estimates are
written as upper bounds. Each phase names its split seam and splits before its ceiling.

## 10. SPEC corrections

1. **§9.3.** `update_frontmatter` gains `create_frontmatter?`, and `promote_document` sets it in
   `link` mode.
2. **§9.1.** `janus set` gains `--create-frontmatter`.
3. **§7.3 step 6.** A write may create the block on request, not only when an internal flag is
   set.
4. **§7.3 step 5.** A key changes when its **parsed value** differs. This supersedes M7-44's
   byte rule.
5. **§6.3.** Name the five keys a write validates and the forms a date accepts.
6. **§9.4.** `changed_keys` means "bytes differ", `appended_keys` means "the file did not have
   it", and `created_block` is new.
7. **§11.** Insert M7b between M7 and M8, and state that M8 inherits `watch`, mmap and
   cross-corpus supersession.

## 11. Handoff

- **M8** inherits the writer: `watch` must not race it, and `doctor` reports stray temp files.
- **R2** inherits the searchable-frontmatter question and the ordering gap. Its trigger is
  unchanged, and the count after M7 was two in ten.
- **The gate's remaining findings** are M7b-4 (validation) and nothing else. The decay
  off-switch is fixed in its own PR before this milestone starts.

## 12. Implementation logs

Empty. One entry per phase, appended when the phase merges.
