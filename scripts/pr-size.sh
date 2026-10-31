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
# Budget (AGENTS.md):
#   logic   <= 600 lines
#   tests   <= 900 lines (1.5x the logic ceiling)
#
# Test lines are counted separately because assertions are not what makes a PR
# hard to review - the logic is. A file counts as tests if it lives in tests/ or
# benches/, or if the line sits after the file's first `#[cfg(test)]`.
#
# Usage:
#   scripts/pr-size.sh [--base REF] [--logic N] [--tests N] [--quiet]
#
# Environment:
#   BASE   ref to diff against (default: origin/main)
#   LOGIC  logic-line ceiling (default: 600)
#   TESTS  test-line ceiling (default: 900)
#
# Exit codes:
#   0  within budget
#   1  over the hard ceiling
#   2  usage or environment error
#
# Markdown and lockfiles are excluded from the budget and reported separately:
# prose and generated text are not logic a reviewer has to hold in their head,
# and both would drown the number that matters. Everything else - Rust, shell,
# YAML, TOML - is budgeted.

set -euo pipefail

BASE="${BASE:-origin/main}"
LOGIC="${LOGIC:-600}"
TESTS="${TESTS:-900}"
QUIET=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) BASE="$2"; shift 2 ;;
    --logic) LOGIC="$2"; shift 2 ;;
    --tests) TESTS="$2"; shift 2 ;;
    --quiet) QUIET=1; shift ;;
    -h|--help) sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if ! git rev-parse --verify --quiet "${BASE}^{commit}" >/dev/null; then
  echo "error: base ref '$BASE' not found (fetch it, or pass --base)" >&2
  exit 2
fi

# `...` (three dots) is the merge base, so a feature branch is measured by what
# it adds, not by everything upstream did since it forked.
RANGE="${BASE}...HEAD"

if [[ -z "$(git diff --name-only "$RANGE")" ]]; then
  echo "PR size: no changes against $BASE"
  exit 0
fi

# Per-file boundary: the first `#[cfg(test)]` line in a source file. Everything
# from there to EOF is the test module, which is the convention every file in
# this repo follows.
boundaries=$(mktemp)
trap 'rm -f "$boundaries"' EXIT

while IFS= read -r file; do
  [[ -f "$file" ]] || continue
  line=$(grep -n -m1 '^#\[cfg(test)\]' "$file" 2>/dev/null | cut -d: -f1 || true)
  printf '%s\t%s\n' "$file" "${line:-0}" >> "$boundaries"
done < <(git diff --name-only --diff-filter=ACM "$RANGE" -- '*.rs')

# Walk the diff hunk by hunk so each added line can be attributed to the right
# side of the boundary. `-U0` means hunk bodies contain only + and - lines, so
# new-side line numbers advance on insertions alone.
summary=$(git diff -U0 "$RANGE" -- . ':(exclude)Cargo.lock' ':(exclude)*.md' \
  | awk -F'\t' -v boundaries="$boundaries" '
    function classify(path, newline, kind,   limit, bucket) {
      if (path ~ /\.md$/ || path ~ /Cargo\.lock$/) return "docs"
      if (path ~ /^(tests|benches)\//) return "tests"
      if (path !~ /\.rs$/) return "logic"
      limit = (path in bound) ? bound[path] : 0
      if (kind == "ins" && limit > 0 && newline >= limit) return "tests"
      return "logic"
    }
    BEGIN {
      while ((getline line < boundaries) > 0) {
        split(line, parts, "\t")
        bound[parts[1]] = parts[2] + 0
      }
    }
    /^diff --git/ { file = ""; next }
    /^\+\+\+ / {
      f = substr($0, 7)
      sub(/^b\//, "", f)
      file = (f == "/dev/null") ? "" : f
      next
    }
    /^--- / { next }
    /^@@ / {
      # `@@ -a,b +c,d @@` -> added lines start at new-side line c
      match($0, /\+[0-9]+/)
      newline = substr($0, RSTART + 1, RLENGTH - 1) + 0
      next
    }
    /^\+/ {
      if (file != "") {
        bucket = classify(file, newline, "ins")
        count[bucket]++
        perfile[bucket "\t" file]++
        newline++
      }
      next
    }
    /^-/ {
      if (file != "") {
        bucket = classify(file, 0, "del")
        count[bucket]++
      }
      next
    }
    END {
      printf "%d %d %d\n", count["logic"], count["tests"], count["other"]
      for (key in perfile) print "FILE\t" key "\t" perfile[key]
    }
  ')

read -r logic tests other <<< "$(printf '%s\n' "$summary" | head -1)"
total=$((logic + tests))

if [[ $QUIET -eq 0 ]]; then
  echo "PR size against $BASE"
  printf '  %-10s %6d / %d\n' "logic" "$logic" "$LOGIC"
  printf '  %-10s %6d / %d\n' "tests" "$tests" "$TESTS"
  if (( other > 0 )); then
    printf '  %-10s %6d  (not budgeted: Markdown and lockfiles)\n' "docs" "$other"
  fi
  printf '  %-10s %6d / %d\n' "total" "$total" "$((LOGIC + TESTS))"
  echo
  echo "  largest logic contributors:"
  printf '%s\n' "$summary" \
    | awk -F'\t' '$1 == "FILE" && $2 == "logic" { printf "%6d  %s\n", $4, $3 }' \
    | sort -rn | head -5 | sed 's/^/  /'
  echo
fi

over=0
if (( logic > LOGIC )); then
  echo "✗ logic over budget: $logic lines (limit $LOGIC)" >&2
  over=1
fi
if (( tests > TESTS )); then
  echo "✗ tests over budget: $tests lines (limit $TESTS)" >&2
  over=1
fi

if (( over )); then
  cat >&2 <<'EOF'

This change is too large to review well. Split it before opening the PR — see
AGENTS.md. Usual causes:

  * more than one milestone phase in a single branch
  * a refactor mixed with a feature
  * a behaviour change mixed with a file move

If the size is genuinely unavoidable (a generated file, a mechanical rename),
label the PR `large-pr` and say why in the description. CI requires that label
to override this check.
EOF
  exit 1
fi

(( logic > LOGIC * 2 / 3 )) && echo "⚠ logic at $logic lines — approaching the $LOGIC ceiling."
exit 0
