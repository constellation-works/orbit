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
failed=0
guard_pool_pid=""
guard_phase_started=0
# Keep all runtime output within the checkout, including on macOS.
mkdir -p "$repo_root/.orbit/tmp"
guard_logs="$(mktemp -d "$repo_root/.orbit/tmp/ci-guardrails.XXXXXX")"

# Joins the background guard pool: prints its output in launch order and folds
# its status into the first failure. The exit trap joins too, so a failing cargo
# phase cannot leave guard children running after the script exits.
join_guard_pool() {
  local status=0
  if [[ -z "$guard_pool_pid" ]]; then return 0; fi
  wait "$guard_pool_pid" || status=$?
  guard_pool_pid=""
  cat "$guard_logs/pool.out"
  cat "$guard_logs/pool.err" >&2
  phase_records+=("guard-pool wall_seconds=$((SECONDS - guard_phase_started)) exit=$status")
  if [[ "$failed" -eq 0 ]]; then failed="$status"; fi
}

report_phases() {
  local status=$? record
  join_guard_pool
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

# --fast runs formatting and script guardrails, never the Rust test suite.
# `make ci-test-affected` supplies the separate pre-handoff test gate.
run_phase fmt cargo fmt --all -- --check
if [[ "$fast" == false ]]; then
  # Enumerating workflow tests compiles their targets; keep it out of ci-fast.
  # --workspace-build lists them from the same `--workspace --lib --bins
  # --tests` artifacts the nextest pass below runs, instead of a per-crate
  # `cargo test -p` build that resolves features differently and recompiles
  # the workspace chain a second time. [DANI-10428]
  # The list-build lints lib, bins, tests and examples with the former
  # all-targets flags (see check-ci-macos.sh). That replaces the separate
  # clippy-all-targets phase, which re-checked the same crates after this
  # compile. Production clippy stays its own pass: it is the only one that
  # denies clippy::disallowed_methods, so test-only channels stay exempt on
  # the folded build. make ci-lint still runs a separate all-targets clippy
  # because that gate does not build test binaries.
  run_phase workflow-test-list-build "$repo_root/scripts/check-ci-macos.sh" --workspace-build
  run_phase clippy-production cargo clippy --workspace --lib --bins -- -D warnings -D clippy::disallowed_methods
  if cargo nextest --version >/dev/null 2>&1; then
    # Profile ci raises test-threads above the 4-vCPU runner default.
    # The sampler prints one cpu-util line and keeps nextest's exit status.
    run_phase workspace-tests "$repo_root/scripts/cpu-utilisation.sh" --label nextest -- \
      cargo nextest run --profile ci --no-fail-fast \
      --success-output immediate \
      --workspace --lib --bins --tests
  else
    echo "cargo-nextest not found; falling back to cargo test" >&2
    run_phase workspace-tests cargo test --no-fail-fast --workspace --lib --bins --tests
  fi
fi

# Repository guards and self-tests share one sliding-window pool. Its width is
# the build budget's cargo job count, the same number Cargo receives, so guard
# fan-out stays inside the host-wide budget. The pool runs in a background
# subshell, so the cargo doctests, documentation and supply-chain gate below
# overlap it; nothing in the pool depends on them. Each guard writes its own
# output files, and the pool prints them in launch order as it reaps each guard.
pool_width="$("$repo_root/scripts/build-budget.py" --cargo-jobs)"
pids=()
names=()
slots=()
launched=0
reaped=0
next_slot=0

# Prints the oldest launched guard's output and folds its status into the first failure.
reap_oldest() {
  local status=0
  wait "${pids[$reaped]}" || status=$?
  cat "$guard_logs/${slots[$reaped]}.out"
  cat "$guard_logs/${slots[$reaped]}.err" >&2
  if [[ "$status" -ne 0 ]]; then
    echo "ci-guardrails: FAILED ${names[$reaped]} (exit $status)" >&2
    # Preserve the first failure's exit status for existing callers.
    if [[ "$failed" -eq 0 ]]; then failed="$status"; fi
  fi
  reaped=$((reaped + 1))
}

drain_pool() {
  while ((reaped < launched)); do
    reap_oldest
  done
}

# Starts one guard in its own output slot. bash's time keyword supplies
# fractional wall time on Linux and bash 3.2.
launch_guard() {
  local name="${1##*/}" slot="$next_slot"
  next_slot=$((next_slot + 1))
  (
    # bash 3.2 drops TIMEFORMAT when errexit aborts a failing timed command,
    # so a guard that exits non-zero would publish no duration line. Suspend
    # errexit only around `time` and return the command status unchanged.
    TIMEFORMAT="ci-guardrails: $name %3R s"
    set +e
    time { "$@"; }
    status=$?
    exit "$status"
  ) >"$guard_logs/$slot.out" 2>"$guard_logs/$slot.err" &
  pids+=("$!")
  names+=("$name")
  slots+=("$slot")
  launched=$((launched + 1))
}

# Self-tests run only when their inputs changed. The QA sweep's --check is also
# a live CLI/MCP/web inventory check; it must still run for crates-only changes,
# unlike fixture-only self-tests.
skipped() {
  local name="${1##*/}"
  if [[ "$name" == test-* && "$name" != test-qa-full-sweep.py && "$run_self_tests" == false ]]; then
    echo "ci-guardrails: SKIPPED $name (self-test inputs unchanged)"
    return 0
  fi
  return 1
}

# Pool member: waits for the oldest running guard whenever the pool is full.
run_guard() {
  if skipped "$1"; then return 0; fi
  while ((launched - reaped >= pool_width)); do
    reap_oldest
  done
  launch_guard "$@"
}

# Exclusive self-test: runs alone after the pool has joined, and is drained
# before the next starts. Use it for suites that assert elapsed time or timeouts,
# measure build-budget admission, or drive a real Cargo target.
run_exclusive_guard() {
  if skipped "$1"; then return 0; fi
  launch_guard "$@"
  drain_pool
}

guard_phase_started=$SECONDS
(
  # The report trap belongs to the parent shell, not to the pool.
  trap - EXIT
  # Start the longer repository scans first; the pool keeps pool_width running.
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

  # Fixture self-tests that assert no elapsed time or timeout and touch no real
  # Cargo target. Full CI always runs these; fast mode selects them by inputs.
  # The exclusive suites are listed after the pool joins.
  run_guard "$repo_root/scripts/test-installer-security.sh"
  run_guard "$repo_root/scripts/test-mcp-registry-publish-workflow.sh"
  run_guard "$repo_root/scripts/test-check-unused-dependencies.py"
  run_guard "$repo_root/scripts/test-release-workflow.py"
  run_guard "$repo_root/scripts/test-release-promotion.py"
  run_guard "$repo_root/scripts/test-pr-state-workflows.py"
  run_guard "$repo_root/scripts/test-website-workflow.py"
  run_guard "$repo_root/scripts/test-windows-portability.py"
  run_guard "$repo_root/scripts/test-refresh-dashboard-vendor.py"
  run_guard "$repo_root/scripts/test-unit-test-inventory.py"
  run_guard "$repo_root/scripts/test-codeql-extension-schema.py"
  run_guard "$repo_root/scripts/test-codeql-production-paths.py"
  run_guard "$repo_root/scripts/test-doc-duty-rotation.py"
  run_guard "$repo_root/scripts/test-validate-codex-plugin.sh"
  run_guard "$repo_root/scripts/test-validate-agent-plugin.sh"
  run_guard "$repo_root/scripts/test-make-install.py"
  run_guard "$repo_root/scripts/test-cross-revision-check.sh"
  run_guard "$repo_root/scripts/test-cpu-utilisation.py"
  drain_pool
  exit "$failed"
) >"$guard_logs/pool.out" 2>"$guard_logs/pool.err" &
guard_pool_pid=$!

if [[ "$fast" == false ]]; then
  run_phase doctests cargo test --no-fail-fast --workspace --doc
  RUSTDOCFLAGS="-D warnings" run_phase documentation cargo doc --no-deps --workspace
  # Supply-chain gate: dependency advisories + license allow-list (deny.toml).
  # [ORB-00416] [ORB-11983] Soft-presence like nextest above — CI installs a pinned version;
  # local runs without the tool warn instead of hard-failing.
  if command -v cargo-deny >/dev/null 2>&1; then
    run_phase supply-chain "$repo_root/scripts/cargo-deny.sh" check
  else
    echo "cargo-deny not found; skipping supply-chain gate (install: cargo install cargo-deny --locked)" >&2
  fi
fi

join_guard_pool

# Exclusive self-tests run one at a time with nothing else on the host. Each one
# asserts elapsed time or timeouts, measures build-budget admission, or drives a
# real Cargo target; the rendezvous and deadline checks in ci-fast-guards count too.
run_exclusive_guard "$repo_root/scripts/test-build-budget.sh"
run_exclusive_guard "$repo_root/scripts/test-ci-download-verified.py"
run_exclusive_guard "$repo_root/scripts/test-ci-fast-guards.py"
run_exclusive_guard "$repo_root/scripts/test-ci-test-affected.py"
run_exclusive_guard "$repo_root/scripts/test-codeql-rust-local.py"
run_exclusive_guard "$repo_root/scripts/test-compiler-cache.sh"
run_exclusive_guard "$repo_root/scripts/test-compiler-cache-namespaces.sh"

phase_records+=("repository-guards wall_seconds=$((SECONDS - guard_phase_started)) exit=$failed")
exit "$failed"
