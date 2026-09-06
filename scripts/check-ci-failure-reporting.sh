#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
workflow="$repo_root/.github/workflows/ci.yml"

if ! command -v rg >/dev/null 2>&1; then
  echo "check-ci-failure-reporting: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

required_patterns=(
  'name: Report CI failure for host sweep'
  'if: failure()'
  'host-owned ci_failure_sweep_pipeline'
  'durable filing happens only on the Orbit host'
  'RUN_ID: ${{ format('\''{0}'\'', github.run_id) }}'
  'RUN_ATTEMPT: ${{ format('\''{0}'\'', github.run_attempt) }}'
  'JOB: ${{ format('\''{0}'\'', github.job) }}'
  'EVENT_SHA: ${{ format('\''{0}'\'', github.sha) }}'
  'PR_HEAD_SHA: ${{ format('\''{0}'\'', github.event.pull_request.head.sha) }}'
  'git rev-parse HEAD'
  'exit 0'
)

for pattern in "${required_patterns[@]}"; do
  if ! rg -F -q -- "$pattern" "$workflow"; then
    echo "CI failure reporting is missing required provenance or fail-open behavior: $pattern" >&2
    exit 1
  fi
done

if rg -n -- 'Create orbit task on CI failure|orbit\.task\.add|github-actions' "$workflow"; then
  echo "CI runners must not create Orbit tasks or claim an unsupported filer identity" >&2
  exit 1
fi

echo "CI failure reporting guard passed"
