#!/usr/bin/env bash
set -uo pipefail

range="$1"
fail=0

bad() { echo "::error::$1"; fail=1; }

if [ -z "${FORBIDDEN_NAMES:-}" ]; then
  if [ "${FROM_FORK:-false}" = true ]; then
    echo "::warning::Name check skipped: secrets are not passed to pull requests from forks."
  else
    bad "the FORBIDDEN_NAMES secret is not set"
  fi
else
  changed=$(git diff "$range" -U0 --no-color | grep -E '^\+' | grep -vE '^\+\+\+' || true)
  grep -Eiq "$FORBIDDEN_NAMES" <<<"$changed" && bad "forbidden name in changed lines"
  git log "$range" --format=%B | grep -Eiq "$FORBIDDEN_NAMES" && bad "forbidden name in a commit message"
  printf '%s\n%s\n' "${PR_TITLE:-}" "${PR_BODY:-}" | grep -Eiq "$FORBIDDEN_NAMES" && bad "forbidden name in the pull request title or body"
fi

if grep -nE 'generate_release_notes:[[:space:]]*true' .github/workflows/*; then bad "generate_release_notes true in a workflow"; fi
if grep -nE "(^|[[:space:]\"'(])@[A-Za-z0-9_-]+" .github/workflows/*; then bad "@mention in a workflow"; fi

exit $fail
