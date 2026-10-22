#!/usr/bin/env bash
set -uo pipefail

fail=0

bad() { echo "::error::$1"; fail=1; }

if grep -nE 'generate_release_notes:[[:space:]]*true' .github/workflows/*; then bad "generate_release_notes true in a workflow"; fi
if grep -nE "(^|[[:space:]\"'(])@[A-Za-z0-9_-]+" .github/workflows/*; then bad "@mention in a workflow"; fi

exit $fail
