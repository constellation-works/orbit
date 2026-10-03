#!/usr/bin/env bash
set -euo pipefail

# Task history notes must not inline bulk. [ORB-10343]
#
# Measurement over 845 task bundles found that every history entry above 2 KB
# was a `workflow_run_failed` note carrying a run's whole `error_message` — 9
# entries holding 16.6% of all history bytes in the workspace, one of them
# 85 KB. Every reader of that task pays for the blob: Daniel scanning history,
# memory distillation, triage, and every agent whose task context is built from
# the same note (`v2_host/task_context.rs`).
#
# The fix is a single cap in `orbit-engine`'s `context::outcome`, which keeps the
# head of the message and points at `orbit run show <run_id> --json` for the
# rest. That is lossless because `job_run_steps.error_message` already persists
# the full text — the note was a duplicate.
#
# This guard exists because the failure mode is silent: a new failure writer that
# formats its own note, or a second copy of the threshold, reintroduces the blob
# and nothing else notices until history is fat again. The runtime bound itself
# is pinned by crates/orbit-engine/tests/history_note.rs; this script
# pins the structural invariants that no unit test can see.
#
# Re-measure any time with: scripts/measure-history-signal.py

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! command -v rg >/dev/null 2>&1; then
  echo "check-history-note-size: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

owner="crates/orbit-engine/src/context/outcome.rs"
failed=0

if [[ ! -f "$owner" ]]; then
  echo "check-history-note-size: expected the note owner at $owner" >&2
  exit 1
fi

# 1. One threshold, one place. A second declaration is a threshold that drifts.
threshold_sites="$(rg --no-heading --line-number --glob '*.rs' \
  'const MAX_NOTE_ERROR_BYTES' crates || true)"
threshold_count="$(printf '%s' "$threshold_sites" | rg -c . || true)"
if [[ "$threshold_count" != "1" ]]; then
  echo "the history-note size threshold must be declared exactly once; found ${threshold_count:-0}:"
  printf '%s\n' "$threshold_sites"
  failed=1
elif ! printf '%s' "$threshold_sites" | rg -q "^$owner:"; then
  echo "MAX_NOTE_ERROR_BYTES must live in $owner:"
  printf '%s\n' "$threshold_sites"
  failed=1
fi

# 2. One producer. A writer that assembles its own `workflow_run_failed` note
#    bypasses the cap entirely, so only the owning module may emit that event.
while IFS= read -r hit; do
  [[ -z "$hit" ]] && continue
  file="${hit%%:*}"
  [[ "$file" == "$owner" ]] && continue
  # Naming the pattern in prose is not emitting it. A doc comment has to be able
  # to say which event this rule governs and why a call site stopped building it.
  code="${hit#*:*:}"
  if [[ "${code#"${code%%[![:space:]]*}"}" == //* ]]; then
    continue
  fi
  echo "workflow_run_failed notes must be built by workflow_failure_note in $owner: $hit"
  failed=1
done < <(rg --no-heading --line-number --glob '*.rs' --glob '!**/tests/**' \
  'status_event:\s*Some\(\s*WORKFLOW_RUN_FAILED_EVENT' crates || true)

# 3. The elision must keep saying where the full text is. A pointer-free
#    elision would turn a bounded note into a lossy one from the reader's side.
if ! rg -q 'orbit run show \{run_id\} --json' "$owner"; then
  echo "the elided note must carry its retrieval command (\`orbit run show <run_id> --json\`) in $owner"
  failed=1
fi
if ! rg -q 'run\.steps\[\]\.error_message' "$owner"; then
  echo "the elided note must name the field that holds the full text, in $owner"
  failed=1
fi

if [[ "$failed" -ne 0 ]]; then
  echo "see docs/design/task-artifacts/specs/task-bundle-v2.md §Events for the rule" >&2
  exit 1
fi

echo "history note size guard passed"
