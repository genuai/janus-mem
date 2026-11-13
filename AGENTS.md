# AGENTS.md

Instructions for any AI agent (and human) working in this repository. Pi, Claude
Code, Codex and Cursor read this file automatically. It exists because each
session starts cold: anything not written down here will be re-guessed, usually
differently.

## What this project is

`janus-mem` is a local-first temporal knowledge engine for AI agents: one Rust
binary that indexes a directory of Markdown (journals, specs, ADRs, principles)
and serves authority-weighted, time-aware retrieval. The vault is the source of
truth; everything in `.janus/` is a rebuildable cache.

**`SPEC.md` is the design authority.** It is ~1,500 lines. Do not read it whole —
read the sections the task names. `docs/m1-breakdown.md` is the current phase
plan and carries the decisions already settled (IDs like `I-5`, `V-4`).

## PR size budget — the rule that matters most

**One milestone phase per PR.** The ceiling is counted in two buckets:

| Bucket | Ceiling | What counts |
|---|---|---|
| logic | **600** | inserted + deleted lines in source: `.rs` before `#[cfg(test)]`, plus `.sh`, `.yml`, `.toml` |
| tests | **900** (1.5×) | `tests/`, `benches/`, and everything after a file's `#[cfg(test)]` |
| — | 1500 total | Markdown and `Cargo.lock` are reported but not budgeted |

Tests get a larger allowance on purpose: assertions are not what makes a PR hard
to review, logic is. A parser plus 40 edge-case tests is one reviewable unit; the
same 400 lines of logic alone is not.

This is enforced mechanically:

```bash
scripts/pr-size.sh                              # HEAD against origin/main
BASE=origin/m1-scan-parse scripts/pr-size.sh    # stacked PRs
```

CI runs the same check on every pull request, but the ceiling gates the
**initial submission**: the job fails only while the PR has no reviews. Once a
review exists the same numbers are reported with a warning annotation and the
check passes, because the ceiling protects a reviewer's attention and that
reviewer has already spent it. The `large-pr` label remains an escape hatch for
generated files and mechanical renames, not for "I got carried away" — and if
you use it, put the numbers in the PR description.

How to actually comply:

1. **Plan before coding.** State the phases, and the files each touches, before
   writing any code. If the plan exceeds the budget, say so and propose a split.
2. **One phase, one branch, one PR.** `m1-scan-parse`, then
   `m1-phase3-frontmatter`, then `m1-phase4-scan-json`. Branch from `main` after
   the previous PR merges — not from the previous branch.
3. **When asked to do something large, split it and ask.** Do not silently
   accumulate. A "just one more thing" commit is how a 400-line PR becomes 4,000.
4. **Never rewrite reviewed history to shrink a PR.** If a branch is already
   large, ship it with the phases in separate commits and fix the process next time.

## Gates — all must pass before committing

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo doc --no-deps                 # must produce zero warnings
```

Plus the soft cross-platform gate (Windows must stay *possible*, SPEC §8.7):

```bash
cargo check --target aarch64-pc-windows-msvc --all-targets --features blake3/pure
# never add `--features embeddings` here: `fastembed` -> `ort` fetches a prebuilt
# ONNX Runtime at build time and cannot be cross-compiled from macOS to Windows.
```

(The `blake3/pure` feature is only needed when cross-checking from macOS, which
has no MSVC C toolchain; CI's `windows-latest` job builds natively.)

The `embeddings` feature is therefore never in the default build, `cargo test`,
or the Windows gate. It is checked and linted by its own Linux CI job
(`cargo check` and `cargo clippy --all-targets --features embeddings`), which is
the only place the `fastembed` module is compiled at all.

## Dogfooding install

After a milestone merges, rebuild the local binary that agents run:

```bash
cargo install --path . --locked   # release build into ~/.cargo/bin
janus --version                   # the version the next dogfood run will use
```

Record the commit the binary was built from. A finding that comes from a stale
binary is not a finding: the index is written by one version and queried by
another, and every result is suspect for a reason nobody can name.

Do not skip this because a phase looked small. Tests cover the library, not the
installed binary, and a dogfood run is the only check that covers both.

## Versioning

Four version numbers exist and must never be conflated (SPEC §6.8, §9.5):

| Number | Where | Changed by | Bumping it means |
|---|---|---|---|
| crate / binary version | `Cargo.toml` | us, at release | semver for the `janus` command |
| `format_version` | `.janus/manifest.json` | us, on layout change | every existing index is rebuilt |
| API version | every `--json` and MCP response (`"api": 1`) | us, on a breaking DTO change | a consumer must be updated |
| `schema_version`, `doc_version` | the user's frontmatter | the user, or an explicit write | nothing, unless the user asked |

Policy:

- **Pre-1.0 (`0.y.z`).** A breaking change bumps `y`. Nothing is promised stable
  except the invariants below — but do not use the pre-1.0 excuse to churn
  formats casually; every bump costs a rebuild for every user.
- **`format_version`** is bumped on *any* change to the on-disk layout or its
  meaning. A mismatch triggers a rebuild with a warning; it is never an error,
  and an index is never read across a version boundary (§8.3).
- **Exit codes and error codes are public API** (§9.1, §9.5). Adding a code is
  additive and fine. Changing or removing one is breaking: bump the API version
  and say so in the commit message.
- **Frontmatter versions belong to the user.** janus-mem never invents a
  `doc_version`, never bumps `schema_version`, and never adopts a versioning
  scheme for a document that lacks one (D26). `--bump-version` is an explicit
  opt-in.
- Release artifacts come from `cargo-dist` on a git tag (M8). Until then the
  crate version is a label, not a contract.

## Engineering principles

`SPEC.md` §5.1 is the authority. The subset that actually changes day-to-day
decisions:

- **Single responsibility per module.** `vault` parses, `index` stores, `score`
  scores, `write` writes, adapters adapt. If a function needs the word "and"
  twice in its doc comment, split it.
- **Dependencies arrive as arguments, never from ambient state.** A `Clock`
  rather than `SystemTime::now()`; a `Config` rather than `std::env::var` deep in
  the engine; an `Embedder` rather than a global model. This is precisely what
  makes `FixedClock`, `HashEmbedder` and hermetic `WalkOptions` possible — and
  why tests never read the wall clock or `$HOME`.
- **DRY, with judgment.** Three similar lines beat a premature trait, especially
  in Rust where the trait drags lifetimes into four modules. The signal to
  extract is the third occurrence *in a different module*, not the second.
- **TDD for rules, glue-first for plumbing.** Scoring, decay, status gating, span
  patching, identity resolution: failing test first. CLI formatting and serde
  structs: write them, then test the behaviour.
- **Open for extension, closed for modification.** New intents, embedders and
  index backends arrive as trait impls or enum variants, not as `if` branches
  inside `score()`.
- **No hidden globals.** No `static mut`, no ambient singletons, no `std::env`
  reads below the boundary. Resolve once at the edge, pass it down.

## Code conventions

- **No `unsafe`** (D12). No proc macros, no FFI, no custom async runtime.
- **No `unwrap()`/`expect()` in library code.** Error messages must name the
  path or value that failed, never just "invalid input".
- **Reads fail open, writes fail closed.** Unparseable frontmatter degrades to a
  documented default plus a note; a write with a stale etag is refused.
- **Document every public item** — `#![warn(missing_docs)]` is on.
- **Determinism.** No wall-clock time in tests (inject a `Clock`); sort output;
  prefer `BTreeMap` over `HashMap` anywhere iteration can reach output or a test.
- **Cross-platform.** Paths via `Path`/`PathBuf`, serialized with `/`; canonicalize
  with `dunce`; no `std::os::unix|windows` without `#[cfg]`.
- **Test-first for anything with a rule** (scoring, frontmatter patching,
  chunking). A bug fix lands with a failing test first.

## Public repository: what never goes in

This repository is public. A private path, a harness identifier, an absolute home directory, a real
email address or a credential is a leak. A leak survives the commit that removes it.

**Never write any of these into a file, a commit message, a pull request, an issue, or a reply:**

| Class | Examples |
|---|---|
| A private vault path or folder | a home directory, a numbered vault folder, a system or archive folder |
| A harness path or id | an agent home directory, a thread id, an environment id, a project id, a branch named after one |
| A model or agent name | the model this project runs on, the harness that drives it |
| A real email address | an author address. A GitHub `noreply` address is allowed |
| A credential | a key, a token, a password, a private key, a webhook URL with a secret |
| Private content | a decision, a thought, a journal entry, a client or family fact, a query copied from a private vault |

Write the fact instead of the machinery. Cite a path and a count, never the content.

**The mechanism, not the intention.** `scripts/leak-check.sh` implements the table above. It fails
on a tracked file, a commit subject, or a commit author. CI runs it on every push and on every pull
request, and it scans the pull request's branch name and body as well, because GitHub publishes
those.

```bash
scripts/leak-check.sh           # the tree at HEAD
scripts/leak-check.sh --staged  # what you are about to commit
scripts/leak-check.sh --all     # the tree, every commit subject, every commit author
```

`scripts/leak-allow` lists accepted hits, one `path:rule` per line. It exists so the debt is visible and
countable, and it must end empty. **A new hit is fixed, not excepted.** An exception is a decision
with a date and an owner, recorded in the phase log that removes it.

The author identity is pinned to a GitHub `noreply` address. A commit that changes it is a finding.

## Writing for readers

Documentation that a person or an agent reads to *use* the tool follows the
copywriting rules, not the terse plan voice: README, `docs/*.md` guides, MCP
`instructions`, tool descriptions, error hints.

- Lead with what the reader can do, then the detail.
- Use the reader's words. "Fades" beats "temporal decay" when both are true.
- One idea per sentence. Short sentences, active voice, no filler.
- No AI tells: no contrast reveals ("not X, but Y"), no negation lists, no
  colon reveals, no em dashes in short copy, no "seamless" or "unlock".
- Read it aloud.
- A claim you cannot support does not go in. Name what is missing instead.

Plans, phase logs, decisions and commit message bodies stay terse and
structured: they are engineering records, and a reader scans them.

## Where decisions go

Settled decisions go in the phase log of the relevant `docs/<milestone>-breakdown.md`
(e.g. §6 and §7 of `docs/m1-breakdown.md`), with an ID and a one-line *why*.
Do **not** create `docs/notes/NN-milestone.md` journals — that convention was
retired; nobody read them. Long-lived, project-level decisions belong in
`SPEC.md` §15.

## Commits and branches

- Conventional-ish, lowercase, imperative: `feat:`, `fix:`, `docs:`, `chore:`,
  `refactor:`, `test:`.
- The body explains *why*, names the SPEC section or decision ID, and states what
  is deliberately *not* included.
- Commit email must stay the noreply address; it is pinned in local git config.
- A local `pre-commit` hook runs fmt + clippy. It lives in `.git/hooks` and is
  not cloned, so CI is the real gate.

## Hard "never" list

- Never touch `.git/` or `.janus/` as vault content (§8.6 rule 1).
- Never shell out to git, and never run git commands on the user's behalf.
- Never write to the vault outside an explicit write command (§9.3).
- Never log document content above `debug`, and never at `info` (§8.5).
- Never copy content from a private or proprietary corpus into this repository: not into a
  commit, a pull request, an issue, a test fixture, a doc, a code comment, or an agent's
  reply. Cite a path and a count instead. A leak survives the commit that removes it.
- Never add a dependency without a one-line justification comment in `Cargo.toml`
  (§8.2).
