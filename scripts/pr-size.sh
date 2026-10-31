#!/usr/bin/env bash
#
# Measures how large a branch's change is, and fails when it exceeds the budget
# in AGENTS.md.
#
# Why a script and not a convention: every agent session starts cold, and an
# instruction in a file is a hope while a non-zero exit is a fact. This is the
# fact. Run it before opening a PR; CI runs it as well, so forgetting is not a
# way through.
#
# Usage:
#   scripts/pr-size.sh [--base REF] [--limit N] [--warn N] [--quiet]
#
# Environment:
#   BASE   ref to diff against (default: origin/main)
#   LIMIT  hard ceiling in changed lines (default: 600)
#   WARN   advisory threshold (default: 400)
#
# Exit codes:
#   0  within budget (possibly with a warning)
#   1  over the hard ceiling
#   2  usage or environment error
#
# Lockfiles are excluded: they are generated, and counting them would make the
# budget meaningless rather than strict. Add more paths with EXTRA_EXCLUDES
# (a git pathspec fragment, e.g. "':(exclude)tests/fixtures/**'").

set -euo pipefail

BASE="${BASE:-origin/main}"
LIMIT="${LIMIT:-600}"
WARN="${WARN:-400}"
EXTRA_EXCLUDES="${EXTRA_EXCLUDES:-}"
QUIET=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) BASE="$2"; shift 2 ;;
    --limit) LIMIT="$2"; shift 2 ;;
    --warn) WARN="$2"; shift 2 ;;
    --quiet) QUIET=1; shift ;;
    -h|--help) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if ! git rev-parse --verify --quiet "${BASE}^{commit}" >/dev/null; then
  echo "error: base ref '$BASE' not found (fetch it, or pass --base)" >&2
  exit 2
fi

# `...` (three dots) is the merge base, so a feature branch is measured by what
# it adds, not by everything upstream did since it forked.
#
# The conditional is not style: bash 3.2 (still macOS's /bin/bash) errors on
# `"${empty_array[@]}"` under `set -u`, so the extra pathspecs are only expanded
# when there are any.
if [[ -n "$EXTRA_EXCLUDES" ]]; then
  read -r -a excludes <<< "$EXTRA_EXCLUDES"
  numstat=$(git diff --numstat "${BASE}...HEAD" -- . ':(exclude)Cargo.lock' "${excludes[@]}")
else
  numstat=$(git diff --numstat "${BASE}...HEAD" -- . ':(exclude)Cargo.lock')
fi

if [[ -z "$numstat" ]]; then
  echo "PR size: no changes against $BASE"
  exit 0
fi

# Sum insertions and deletions, treat binary entries ("-") as 0 lines but still
# count the file.
summary=$(printf '%s\n' "$numstat" | awk -F'\t' '
  { files++; ins += ($1 == "-" ? 0 : $1); del += ($2 == "-" ? 0 : $2) }
  END { printf "%d %d %d", files, ins, del }
')
read -r files insertions deletions <<< "$summary"
total=$((insertions + deletions))

# Biggest contributors, so the fix is obvious rather than a hunt.
biggest=$(printf '%s\n' "$numstat" | awk -F'\t' '
  { lines = ($1 == "-" ? 0 : $1) + ($2 == "-" ? 0 : $2); printf "%6d  %s\n", lines, $3 }
' | sort -rn | head -5)

if [[ $QUIET -eq 0 ]]; then
  echo "PR size against $BASE"
  echo "  files       $files"
  echo "  insertions  $insertions"
  echo "  deletions   $deletions"
  echo "  changed     $total  (budget: warn $WARN, limit $LIMIT)"
  echo
  echo "  largest files:"
  printf '%s\n' "$biggest" | sed 's/^/  /'
  echo
fi

if (( total > LIMIT )); then
  cat >&2 <<EOF
✗ over budget: $total changed lines (limit $LIMIT)

This change is too large to review well. Split it before opening the PR — see
AGENTS.md. Usual causes:

  * more than one milestone phase in a single branch
  * a refactor mixed with a feature
  * a behaviour change mixed with a file move

If the size is genuinely unavoidable (a generated file, a mechanical rename),
label the PR \`large-pr\` and say why in the description. CI requires that label
to override this check.
EOF
  exit 1
fi

if (( total > WARN )); then
  echo "⚠ $total changed lines (warn threshold $WARN) — consider splitting."
fi

exit 0
