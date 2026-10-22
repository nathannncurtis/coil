#!/usr/bin/env bash
set -uo pipefail

range="$1"
employer='REDACTED'
fail=0

bad() { echo "::error::$1"; fail=1; }

changed=$(git diff "$range" -U0 --no-color -- . ':!.github/scripts/rules.sh' | grep -E '^\+' | grep -vE '^\+\+\+' || true)
grep -Eiq "$employer" <<<"$changed" && bad "employer name in changed lines"
git log "$range" --format=%B | grep -Eiq "$employer" && bad "employer name in a commit message"
printf '%s\n%s\n' "${PR_TITLE:-}" "${PR_BODY:-}" | grep -Eiq "$employer" && bad "employer name in the PR title or body"

if grep -nE 'generate_release_notes:[[:space:]]*true' .github/workflows/*; then bad "generate_release_notes true in a workflow"; fi
if grep -nE "(^|[[:space:]\"'(])@[A-Za-z0-9_-]+" .github/workflows/*; then bad "@mention in a workflow"; fi

exit $fail
