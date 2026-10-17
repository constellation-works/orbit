#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

fast=false
case "${1:-}" in
  "") ;;
  --fast) fast=true ;;
  *)
    echo "usage: ci-guardrails.sh [--fast]" >&2
    exit 2
    ;;
esac

# Several guardrail scripts shell out to ripgrep; a missing `rg` degrades
# them silently (empty search results read as "clean" or, worse, as false
# violations). Fail fast instead. [ORB-10021]
if ! command -v rg >/dev/null 2>&1; then
  echo "ci-guardrails: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

# The guardrails below import `tomllib` (Python >= 3.11); stop before any gate
# runs rather than die in a traceback partway through.
"$repo_root/scripts/require-python.sh"

# Retain a compact phase breakdown at the end of both passing and failing
# logs. Individual Cargo output can be omitted from bounded delivery excerpts.
# SECONDS is Bash's portable wall clock (one-second resolution), not a sum of
# overlapping compilation units or testcases.
phase_records=()
mkdir -p "$repo_root/.orbit/tmp"
guard_logs="$(mktemp -d "$repo_root/.orbit/tmp/ci-guardrails.XXXXXX")"
report_phases() {
  local status=$? record
  if ((${#phase_records[@]})); then
    for record in "${phase_records[@]}"; do
      echo "ci-guardrails-phase: $record" >&2
    done
  fi
  python3 -c "import shutil, sys; shutil.rmtree(sys.argv[1])" "$guard_logs"
  exit "$status"
}
trap report_phases EXIT

run_phase() {
  local name="$1" started=$SECONDS status=0
  shift
  "$@" || status=$?
  phase_records+=("$name wall_seconds=$((SECONDS - started)) exit=$status")
  return "$status"
}

# Same Cargo job count as scripts/build-budget.py: ORBIT_CARGO_JOBS, else
# CARGO_BUILD_JOBS, else <budget-dir>/cargo-jobs, else 4. The guard pool uses
# that width so it cannot outrun the build budget it shares the machine with.
resolve_guard_pool_width() {
  python3 - <<'PY'
import os
from pathlib import Path
import sys

maximum = 1024
default = 4

def fail(message):
    print(f"ci-guardrails: {message}", file=sys.stderr)
    raise SystemExit(1)

def positive_integer(name, value):
    if not value.isascii() or not value.isdecimal() or value.startswith("0"):
        fail(f"{name} must be a decimal integer from 1 through {maximum}; got {value!r}")
    parsed = int(value)
    if parsed > maximum:
        fail(f"{name} must be a decimal integer from 1 through {maximum}; got {value!r}")
    return parsed

configured = os.environ.get("ORBIT_BUILD_BUDGET_DIR")
directory = Path(configured).expanduser() if configured else Path.home() / ".orbit" / "cache" / "build-budget"
if directory.is_symlink():
    fail(f"lock directory must not be a symbolic link: {directory}")

if "ORBIT_CARGO_JOBS" in os.environ:
    jobs = positive_integer("ORBIT_CARGO_JOBS", os.environ["ORBIT_CARGO_JOBS"])
elif "CARGO_BUILD_JOBS" in os.environ:
    jobs = positive_integer("CARGO_BUILD_JOBS", os.environ["CARGO_BUILD_JOBS"])
else:
    path = directory / "cargo-jobs"
    try:
        value = path.read_text(encoding="utf-8").strip()
    except FileNotFoundError:
        jobs = default
    except (OSError, UnicodeError) as error:
        fail(f"cannot read ORBIT_CARGO_JOBS/CARGO_BUILD_JOBS from {path}: {error}")
    else:
        jobs = positive_integer("ORBIT_CARGO_JOBS/CARGO_BUILD_JOBS", value)

print(jobs)
PY
}

export GIT_OPTIONAL_LOCKS=0

run_self_tests=true
if [[ "$fast" == true ]]; then
  # Match ci-test-affected's delivery-base override and merge-base fallback.
  base=""
  if [[ -n "${CI_TEST_BASE:-}" ]]; then
    base="$(git rev-parse --verify "${CI_TEST_BASE}^{commit}" 2>/dev/null || true)"
  else
    for branch in refs/remotes/origin/agent-main refs/heads/agent-main; do
      if git rev-parse --verify --quiet "$branch" >/dev/null; then
        base="$(git merge-base HEAD "$branch" 2>/dev/null || true)"
        break
      fi
    done
  fi
  # Include committed, staged, unstaged, deleted and non-ignored untracked
  # inputs, including both sides of moves. A failed query runs every test.
  if [[ -n "$base" ]] &&
    git diff --name-only --no-renames -z "$base" -- >"$guard_logs/paths" &&
    git ls-files --others --exclude-standard -z >>"$guard_logs/paths"; then
    run_self_tests=false
    while IFS= read -r -d '' path; do
      case "$path" in
        scripts/* | .github/* | Makefile | .cargo/* | Cargo.toml | Cargo.lock | \
          install.sh | RELEASING.md | plugin/* | npm/* | .agents/* | .claude-plugin/*)
          run_self_tests=true
          break
          ;;
      esac
    done <"$guard_logs/paths"
  else
    echo "ci-guardrails: cannot resolve changed inputs; running all self-tests" >&2
  fi
fi

# Selection must precede Cargo and repository guards: these mandatory fixtures
# import yaml even though the standalone local workflow linter can warn/skip.
if [[ "$run_self_tests" == true ]]; then
  "$repo_root/scripts/require-python.sh" --yaml
fi

guard_pool_width="$(resolve_guard_pool_width)"

# Doctests, rustdoc and cargo-deny share no inputs with the guard pool.
# They run beside it in full CI. A failure still skips later Cargo steps,
# matching the previous sequential short-circuit. Phase lines are files
# because this function runs in a subshell.
doc_chain() {
  local started status
  started=$SECONDS
  status=0
  cargo test --no-fail-fast --workspace --doc || status=$?
  printf 'doctests wall_seconds=%s exit=%s\n' "$((SECONDS - started))" "$status" >"$guard_logs/phase-doctests"
  if [[ "$status" -ne 0 ]]; then
    return "$status"
  fi
  started=$SECONDS
  status=0
  RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace || status=$?
  printf 'documentation wall_seconds=%s exit=%s\n' "$((SECONDS - started))" "$status" >"$guard_logs/phase-documentation"
  if [[ "$status" -ne 0 ]]; then
    return "$status"
  fi
  # Supply-chain gate: dependency advisories + license allow-list (deny.toml).
  # [ORB-00416] [ORB-11983] Soft-presence like nextest above — CI installs a pinned version;
  # local runs without the tool warn instead of hard-failing.
  if command -v cargo-deny >/dev/null 2>&1; then
    started=$SECONDS
    status=0
    "$repo_root/scripts/cargo-deny.sh" check || status=$?
    printf 'supply-chain wall_seconds=%s exit=%s\n' "$((SECONDS - started))" "$status" >"$guard_logs/phase-supply-chain"
    return "$status"
  fi
  echo "cargo-deny not found; skipping supply-chain gate (install: cargo install cargo-deny --locked)" >&2
  return 0
}

absorb_doc_phases() {
  local name file
  for name in doctests documentation supply-chain; do
    file="$guard_logs/phase-$name"
    if [[ -f "$file" ]]; then
      phase_records+=("$(<"$file")")
    fi
  done
}

# --fast runs formatting and script guardrails, never the Rust test suite.
# `make ci-test-affected` supplies the separate pre-handoff test gate.
run_phase fmt cargo fmt --all -- --check
if [[ "$fast" == false ]]; then
  # Enumerating workflow tests compiles their targets; keep it out of ci-fast.
  # --workspace-build lists them from the same `--workspace --lib --bins
  # --tests` artifacts the nextest pass below runs, instead of a per-crate
  # `cargo test -p` build that resolves features differently and recompiles
  # the workspace chain a second time. [DANI-10428]
  run_phase workflow-test-list-build "$repo_root/scripts/check-ci-macos.sh" --workspace-build
  # Production must reject unbounded channels. Existing test-only channels
  # remain exempt in the all-targets pass.
  run_phase clippy-production cargo clippy --workspace --lib --bins -- -D warnings -D clippy::disallowed_methods
  run_phase clippy-all-targets cargo clippy --workspace --all-targets -- -D warnings -A clippy::disallowed_methods
  if cargo nextest --version >/dev/null 2>&1; then
    run_phase workspace-tests cargo nextest run --no-fail-fast \
      --success-output immediate \
      --workspace --lib --bins --tests
  else
    echo "cargo-nextest not found; falling back to cargo test" >&2
    run_phase workspace-tests cargo test --no-fail-fast --workspace --lib --bins --tests
  fi
fi

# Keep all runtime output within the checkout, including on macOS. Separate
# streams prevent interleaved diagnostics while guards execute concurrently.
# bash 3.2 has no wait -n, so each child publishes a completion file and the
# parent polls it. That also works on macOS, where a fifo opened read-write
# is not portable.
guards_started=$SECONDS
echo "ci-guardrails: guard pool width=$guard_pool_width (build-budget cargo jobs)" >&2

pids=()
names=()
slots=()
starts=()
next_slot=0
started_counter=0
pending=0
failed=0
first_failed_start=""

reap_slot() {
  local found="$1" index pid name start_index status slot
  local new_pids=() new_names=() new_slots=() new_starts=()
  pid="${pids[$found]}"
  name="${names[$found]}"
  start_index="${starts[$found]}"
  slot="${slots[$found]}"
  for ((index=0; index<pending; index++)); do
    if [[ "$index" -eq "$found" ]]; then
      continue
    fi
    new_pids+=("${pids[$index]}")
    new_names+=("${names[$index]}")
    new_slots+=("${slots[$index]}")
    new_starts+=("${starts[$index]}")
  done
  if ((${#new_pids[@]})); then
    pids=("${new_pids[@]}")
    names=("${new_names[@]}")
    slots=("${new_slots[@]}")
    starts=("${new_starts[@]}")
  else
    pids=()
    names=()
    slots=()
    starts=()
  fi
  pending=$((pending - 1))
  status=0
  wait "$pid" || status=$?
  cat "$guard_logs/$slot.out"
  cat "$guard_logs/$slot.err" >&2
  if [[ "$status" -ne 0 ]]; then
    echo "ci-guardrails: FAILED $name (exit $status)" >&2
    # The earliest started failure wins, even if a later guard exits first.
    if [[ -z "$first_failed_start" || "$start_index" -lt "$first_failed_start" ]]; then
      first_failed_start=$start_index
      failed=$status
    fi
  fi
}

reap_one() {
  local index
  while true; do
    for ((index=0; index<pending; index++)); do
      if [[ -f "$guard_logs/${slots[$index]}.done" ]]; then
        reap_slot "$index"
        return 0
      fi
    done
    sleep 0.05
  done
}

finish_pool() {
  while [[ "$pending" -gt 0 ]]; do
    reap_one
  done
}

run_guard() {
  local name pid slot
  name="${1##*/}"
  # The QA sweep's --check is also a live CLI/MCP/web inventory check; it
  # must still run for crates-only changes, unlike fixture-only self-tests.
  if [[ "$name" == test-* && "$name" != test-qa-full-sweep.py && "$run_self_tests" == false ]]; then
    echo "ci-guardrails: SKIPPED $name (self-test inputs unchanged)"
    return 0
  fi
  while [[ "$pending" -ge "$pool_width" ]]; do
    reap_one
  done
  slot=$next_slot
  next_slot=$((next_slot + 1))
  (
    # bash 3.2 drops TIMEFORMAT when errexit aborts a failing timed command,
    # so a guard that exits non-zero would publish no duration line. Suspend
    # errexit only around `time` and return the command status unchanged.
    TIMEFORMAT="ci-guardrails: $name %3R s"
    set +e
    time { "$@"; }
    status=$?
    printf '%s\n' "$status" >"$guard_logs/$slot.done"
    exit "$status"
  ) >"$guard_logs/$slot.out" 2>"$guard_logs/$slot.err" &
  pid=$!
  pids+=("$pid")
  names+=("$name")
  slots+=("$slot")
  starts+=("$started_counter")
  started_counter=$((started_counter + 1))
  pending=$((pending + 1))
}

# Repository checks and fake-cargo self-tests share one pool. Nothing here
# builds into the real target directory.
pool_width=$guard_pool_width
doc_pid=""
if [[ "$fast" == false ]]; then
  doc_chain &
  doc_pid=$!
fi

run_guard "$repo_root/scripts/check-orphan-modules.sh"
run_guard "$repo_root/scripts/check-workflow-action-pins.sh"
run_guard "$repo_root/scripts/test-qa-full-sweep.py" --check
run_guard "$repo_root/scripts/check-error-translation.sh"
run_guard "$repo_root/scripts/check-unused-dependencies.py"
run_guard "$repo_root/scripts/check-doc-links.py"
run_guard "$repo_root/scripts/check-cli-imports.sh"
run_guard "$repo_root/scripts/unit-test-inventory.py" --check "$repo_root/scripts/unit-test-baseline.json"
run_guard "$repo_root/scripts/generate-doc-indexes.sh" --check
run_guard "$repo_root/scripts/check-installer-pubkey.sh"
run_guard "$repo_root/scripts/check-dependency-direction.sh"
run_guard "$repo_root/scripts/check-workflow-yaml.py"
run_guard "$repo_root/scripts/check-test-shell-waits.py"
run_guard "$repo_root/scripts/check-codeql-production-paths.py"
run_guard "$repo_root/scripts/check-codeql-extension-schema.py"
run_guard "$repo_root/scripts/check-terminal-state-guard.sh"
run_guard "$repo_root/scripts/check-durability-syncs.py"
run_guard "$repo_root/scripts/check-windows-portability.py"
run_guard "$repo_root/scripts/check-history-note-size.sh"
run_guard "$repo_root/scripts/check-stability.sh"
run_guard "$repo_root/scripts/check-artifact-redaction-guardrail.sh"
run_guard "$repo_root/scripts/check-web-blocking-handlers.py"
run_guard "$repo_root/scripts/check-ci-failure-reporting.sh"
run_guard "$repo_root/scripts/check-public-artifact-ids.py"
run_guard "$repo_root/scripts/check-changelog-style.sh"
run_guard "$repo_root/scripts/check-crate-agent-guides.sh"
run_guard "$repo_root/scripts/check-embedded-asset-portability.py"
run_guard "$repo_root/scripts/check-desktop-ui.sh"
run_guard "$repo_root/scripts/check-dashboard-vendor.py"
run_guard "$repo_root/scripts/sync-plugin-skills.sh" --check
run_guard "$repo_root/scripts/smoke-plugin-install.sh"

# Fixture self-tests: full CI always runs these, fast mode selects by inputs.
# Suites that only shim Cargo join the pool. test-ci-test-affected.py stays
# exclusive below: its NextestOutputTransportTests class runs real Cargo with
# its own target directory, and unittest has no exclusion filter that still
# runs every other class, including classes added later.
run_guard "$repo_root/scripts/test-installer-security.sh"
run_guard "$repo_root/scripts/test-mcp-registry-publish-workflow.sh"
run_guard "$repo_root/scripts/test-check-unused-dependencies.py"
run_guard "$repo_root/scripts/test-release-workflow.py"
run_guard "$repo_root/scripts/test-release-promotion.py"
run_guard "$repo_root/scripts/test-pr-state-workflows.py"
run_guard "$repo_root/scripts/test-website-workflow.py"
run_guard "$repo_root/scripts/test-ci-download-verified.py"
run_guard "$repo_root/scripts/test-ci-fast-guards.py"
run_guard "$repo_root/scripts/test-windows-portability.py"
run_guard "$repo_root/scripts/test-refresh-dashboard-vendor.py"
run_guard "$repo_root/scripts/test-unit-test-inventory.py"
run_guard "$repo_root/scripts/test-codeql-extension-schema.py"
run_guard "$repo_root/scripts/test-codeql-rust-local.py"
run_guard "$repo_root/scripts/test-codeql-production-paths.py"
run_guard "$repo_root/scripts/test-doc-duty-rotation.py"
run_guard "$repo_root/scripts/test-validate-codex-plugin.sh"
run_guard "$repo_root/scripts/test-validate-agent-plugin.sh"
run_guard "$repo_root/scripts/test-make-install.py"
run_guard "$repo_root/scripts/test-cross-revision-check.sh"
finish_pool

doc_status=0
if [[ -n "$doc_pid" ]]; then
  wait "$doc_pid" || doc_status=$?
  absorb_doc_phases
fi

# Build-admission and real-target suites never overlap each other or a Cargo
# build. They start only after fmt, Clippy, nextest, doctests, rustdoc and
# cargo-deny have exited, and the width of one keeps them serial.
if [[ "$doc_status" -eq 0 ]]; then
  pool_width=1
  run_guard "$repo_root/scripts/test-ci-test-affected.py"
  run_guard "$repo_root/scripts/test-build-budget.sh"
  run_guard "$repo_root/scripts/test-compiler-cache.sh"
  run_guard "$repo_root/scripts/test-compiler-cache-namespaces.sh"
  finish_pool
fi

phase_records+=("repository-guards wall_seconds=$((SECONDS - guards_started)) exit=$failed")
if [[ "$doc_status" -ne 0 ]]; then
  exit "$doc_status"
fi
exit "$failed"
