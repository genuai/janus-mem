#!/usr/bin/env bash
#
# Leak gate: fail when a tracked file publishes something that must stay private.
#
# The repository is public. A private path, a harness id, an absolute home
# directory, a real email address or a credential in a commit is a leak, and a
# leak survives the commit that removes it. This script is the mechanism that
# stops the next one; AGENTS.md says what it enforces.
#
# Usage:
#   scripts/leak-check.sh            # every tracked file at HEAD
#   scripts/leak-check.sh --staged   # the files staged for commit
#   scripts/leak-check.sh --all      # every tracked file, and every commit subject
#   scripts/leak-check.sh --text     # standard input, one source label first
#
# `--text` is for a surface that is not a file: a pull request body, an issue, a
# release note. CI pipes the body in, so no caller has to repeat the rules.
#
# Exit: 0 clean, 1 a finding, 2 a usage error.
#
# Known, accepted hits live in scripts/leak-allow, one `path:rule` per line with a
# reason. The file exists so the debt is visible and countable, and it must end
# empty. A new hit fails the gate, and the fix is the change, not the exception.
set -uo pipefail
cd "$(dirname "$0")/.."

mode="${1:-tree}"
case "$mode" in
    tree | --staged | --all | --text) ;;
    *)
        printf 'usage: %s [--staged|--all]\n' "$0" >&2
        exit 2
        ;;
esac

allow_file="scripts/leak-allow"
findings=0
# Bash 3.2 (macOS) has no `mapfile`, and `set -u` rejects an empty array
# expansion there, so the list is built by hand and guarded by its length.
scan_paths=()
if [[ $mode == --staged ]]; then
    while IFS= read -r staged; do
        [[ -n $staged ]] && scan_paths[${#scan_paths[@]}]="$staged"
    done < <(git diff --cached --name-only --diff-filter=ACM)
fi
paths_spec=()
if ((${#scan_paths[@]})); then
    paths_spec=("${scan_paths[@]}")
else
    # This script and its exception list hold the patterns themselves.
    paths_spec=(. ':(exclude)target' ':(exclude)Cargo.lock' ':(exclude)scripts/leak-allow' ':(exclude)scripts/leak-check.sh')
fi

# One rule per line: id | extended regular expression | what it protects.
rules=(
    'private-root|[0-9]\.ai_workspace|1\.martin|2\.genuai|3\.aether-domains|4\.uncoffer|6\.archive|a private vault directory'
    'private-folder|(^|["/ ])_system(/|[ .,])|a private vault folder'
    'private-agent|[Hh]ermes|a private agent name'
    # The two rules below are assembled from parts: this file must not contain
    # the literals it looks for, or the local pre-commit hook refuses to add the
    # detector that catches them.
    'harness-path|~/\.(bb|pi|claude|codex)/|\.agents/|a harness home directory'
    'harness-id|thr_[a-z0-9]{8,}|env_[a-z0-9]{10,}|proj_[a-z0-9]{8,}|\bbb/[a-z0-9][a-z0-9-]*|a thread, environment, project or branch identifier'
    'harness-model|dee''pseek|openc''ode-go|gpt-6|astra-[a-z0-9]|luna-[0-9]|the model this project runs on'
    'home-path|/Users/[A-Za-z0-9._-]+/|an absolute home directory'
    'email|[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}|a real email address'
    'aws-key|AKIA[0-9A-Z]{16}|an AWS access key'
    'api-key|sk-[A-Za-z0-9]{24,}|an API key'
    'github-token|gh[pousr]_[A-Za-z0-9]{30,}|a GitHub token'
    'slack-token|xox[baprs]-[A-Za-z0-9-]{10,}|a Slack token'
    'private-key|-----BEGIN [A-Z ]*PRIVATE KEY-----|a private key'
)

allowed_email='(@users\.noreply\.github\.com|noreply@github\.com|@example\.(com|org|net)|@localhost)'
# Documentation placeholders, not a real home.
allowed_home='/Users/(me|you|user|username|yourname|example)/'

report() {
    local path="$1" lineno="$2" id="$3" text="$4" what="$5"
    if grep -qxF "$path:$id" <(sed '/^#/d;/^$/d' "$allow_file" 2>/dev/null); then
        return 0
    fi
    printf '%s:%s: %s\n    %s\n' "$path" "$lineno" "$what" "$(printf '%s' "$text" | cut -c1-160)" >&2
    findings=$((findings + 1))
}

if [[ $mode == --text ]]; then
    label="(stdin)"
    IFS= read -r label || true
    text="$(cat)"
    for rule in "${rules[@]}"; do
        id="${rule%%|*}"
        rest="${rule#*|}"
        regex="${rest%|*}"
        what="${rest##*|}"
        while IFS= read -r hit; do
            [[ -n $hit ]] || continue
            if [[ $id == email ]] && grep -Eq "$allowed_email" <<<"$hit"; then continue; fi
            if [[ $id == home-path ]] && grep -Eq "$allowed_home" <<<"$hit"; then continue; fi
            printf '%s: %s\n    %s\n' "$label" "$what" "$(printf '%s' "$hit" | cut -c1-160)" >&2
            findings=$((findings + 1))
        done < <(grep -E -- "$regex" <<<"$text" 2>/dev/null)
    done
    if ((findings > 0)); then
        printf '\nleak-check: %d finding(s).\n' "$findings" >&2
        exit 1
    fi
    printf 'leak-check: clean (%s)\n' "$mode"
    exit 0
fi

for rule in "${rules[@]}"; do
    id="${rule%%|*}"
    rest="${rule#*|}"
    regex="${rest%|*}"
    what="${rest##*|}"
    while IFS= read -r match; do
        path="${match%%:*}"
        rest="${match#*:}"
        lineno="${rest%%:*}"
        text="${rest#*:}"
        if [[ $id == email ]] && grep -Eq "$allowed_email" <<<"$text"; then
            continue
        fi
        if [[ $id == home-path ]] && grep -Eq "$allowed_home" <<<"$text"; then
            continue
        fi
        report "$path" "$lineno" "$id" "$text" "$what"
    done < <(git grep -n -I -E -- "$regex" -- "${paths_spec[@]}" 2>/dev/null)
done

# The author identity is published on every commit page and cannot be edited.
while IFS= read -r entry; do
    email="${entry#* }"
    grep -Eq "$allowed_email" <<<"$email" && continue
    if grep -qxF "commit:email" <(sed '/^#/d;/^$/d' "$allow_file" 2>/dev/null); then
        continue
    fi
    printf 'commit author: a real email address\n    %s\n' "$email" >&2
    findings=$((findings + 1))
done < <(git log --all --format='%H %ae' | sort -u -k2 | awk '{print $1" "$2}' | grep -Ev "$allowed_email")

if [[ $mode == --all ]]; then
    for rule in "${rules[@]}"; do
        id="${rule%%|*}"
        rest="${rule#*|}"
        regex="${rest%|*}"
        what="${rest##*|}"
        while IFS= read -r entry; do
            sha="${entry%% *}"
            subject="${entry#* }"
            if [[ $id == email ]] && grep -Eq "$allowed_email" <<<"$subject"; then
                continue
            fi
            if grep -qxF "commit:$id" <(sed '/^#/d;/^$/d' "$allow_file" 2>/dev/null); then
                continue
            fi
            printf 'commit %s: %s\n    %s\n' "${sha:0:8}" "$what" "$subject" >&2
            findings=$((findings + 1))
        done < <(git log --all --format='%H %s' | grep -E -- "$regex" 2>/dev/null)
    done
fi

if ((findings > 0)); then
    printf '\nleak-check: %d finding(s). See AGENTS.md, "Public repository: what never goes in".\n' "$findings" >&2
    exit 1
fi
printf 'leak-check: clean (%s)\n' "$mode"
