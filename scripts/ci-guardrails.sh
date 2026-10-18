#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

# Modes: the default full run (`make ci`); --fast (`make ci-fast`); the offline
# file/metadata-only --static (`make ci-guards`); and the two
# halves of the split hosted workflow (.github/workflows/ci.yml). --checks runs
# everything except the workspace test build and nextest. --test-archive PATH
# runs only the workspace test build, archives its test binaries for the
# parallel nextest shards, and proves the shards cover every test.
fast=false
static=false
checks=false
archive=""
usage() {
  echo "usage: ci-guardrails.sh [--fast | --static | --checks | --test-archive PATH]" >&2
  exit 2
}
case "${1:-}" in
  "") ;;
  --fast) fast=true ;;
  --static) fast=true; static=true ;;
  --checks) checks=true ;;
  --test-archive)
    [[ -n "${2:-}" ]] || usage
    archive="$2"
    shift
    ;;
  *) usage ;;
esac
[[ $# -le 1 ]] || usage

# Several guardrail scripts shell out to ripgrep; a missing `rg` degrades
# them silently (empty search results read as "clean" or, worse, as false
# violations). Fail fast instead. [ORB-10021]
if [[ -z "$archive" ]] && ! command -v rg >/dev/null 2>&1; then
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
guard_pool_seconds=0
exclusive_lane_pid=""
exclusive_started=0
guards_joined=0
check_chain_pid=""
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
  guard_pool_seconds=$((SECONDS - guard_phase_started))
  phase_records+=("guard-pool wall_seconds=$guard_pool_seconds exit=$status")
  if [[ "$failed" -eq 0 ]]; then failed="$status"; fi
}

# Joins the exclusive self-test lane: prints its output and folds its status
# into the first failure. It joins after the pool, so a pool failure stays the
# job's exit status.
join_exclusive_lane() {
  local status=0
  if [[ -z "$exclusive_lane_pid" ]]; then return 0; fi
  wait "$exclusive_lane_pid" || status=$?
  exclusive_lane_pid=""
  cat "$guard_logs/exclusive.out"
  cat "$guard_logs/exclusive.err" >&2
  phase_records+=("exclusive-self-tests wall_seconds=$((SECONDS - exclusive_started)) exit=$status")
  if [[ "$failed" -eq 0 ]]; then failed="$status"; fi
}

# Joins the background production clippy, doctest and documentation chain:
# prints its output, adopts its phase records and folds its status into the
# first failure.
join_check_chain() {
  local status=0 record
  if [[ -z "$check_chain_pid" ]]; then return 0; fi
  wait "$check_chain_pid" || status=$?
  check_chain_pid=""
  cat "$guard_logs/checks.out"
  cat "$guard_logs/checks.err" >&2
  if [[ -f "$guard_logs/checks.phases" ]]; then
    while IFS= read -r record; do
      phase_records+=("$record")
    done <"$guard_logs/checks.phases"
  fi
  if [[ "$failed" -eq 0 ]]; then failed="$status"; fi
}

report_phases() {
  local status=$? record
  join_guard_pool
  join_exclusive_lane
  join_check_chain
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

# The hosted test-build job: the folded workspace test build, then an archive
# of its test binaries and the `orbit` binary the CLI fixtures execute, then
# the partition guard. The shard jobs run nextest from that archive, one count
# partition each, and the Check job (--checks) runs everything else. Archiving
# and listing run under the folded build's lint environment, so neither
# recompiles; the listings read the archive itself, so the guard proves what
# the shards receive. NEXTEST_SHARDS (ci.yml) is the shard count.
if [[ -n "$archive" ]]; then
  lint="$repo_root/scripts/with-test-build-lint.sh"
  run_phase workflow-test-list-build "$repo_root/scripts/check-ci-macos.sh" --workspace-build
  run_phase workspace-test-archive "$lint" cargo nextest archive --locked \
    --workspace --lib --bins --tests --archive-file "$archive"
  # nextest extracts the archive for each listing; keep it in the checkout.
  TMPDIR="$guard_logs" run_phase workspace-test-partitions \
    "$repo_root/scripts/check-nextest-partitions.py" --shards "${NEXTEST_SHARDS:?set NEXTEST_SHARDS}" -- \
    cargo nextest list --archive-file "$archive" --workspace-remap "$repo_root"
  exit 0
fi

# Static guards may read Cargo metadata but must never fetch registries.
if [[ "$static" == true ]]; then
  export CARGO_NET_OFFLINE=true
fi

run_self_tests=true
if [[ "$static" == true ]]; then
  run_self_tests=false
elif [[ "$fast" == true ]]; then
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
if [[ "$run_self_tests" == true || "$static" == true ]]; then
  "$repo_root/scripts/require-python.sh" --yaml
fi

# --fast runs formatting and script guardrails, never the Rust test suite.
# `make ci-test-affected` supplies the separate pre-handoff test gate.
if [[ "$static" == false ]]; then
  run_phase fmt cargo fmt --all -- --check
fi

# Repository guards and self-tests share one sliding-window pool. Its width is
# the build budget's cargo job count, the same number Cargo receives, so guard
# fan-out stays inside the host-wide budget. The pool runs in a background
# subshell, so in full mode the workflow test build below overlaps it;
# nothing in the pool depends on that build, and no pool member
# takes the Cargo build lock. Each guard writes its own output files, and the
# pool prints them in launch order as it reaps each guard.
pool_width="$("$repo_root/scripts/build-budget.py" --cargo-jobs)"
pids=()
names=()
slots=()
# The exclusive lane is a second subshell with copies of these counters, so it
# names its output slots apart from the pool's.
slot_prefix=""
launched=0
reaped=0
next_slot=0

# Waits for one guard, prints its output and folds its status into the first
# failure. Arguments: pid, output slot, name.
reap_guard() {
  local status=0
  wait "$1" || status=$?
  cat "$guard_logs/$2.out"
  cat "$guard_logs/$2.err" >&2
  if [[ "$status" -ne 0 ]]; then
    echo "ci-guardrails: FAILED $3 (exit $status)" >&2
    # Preserve the first failure's exit status for existing callers.
    if [[ "$failed" -eq 0 ]]; then failed="$status"; fi
  fi
}

# Reaps the oldest launched guard.
reap_oldest() {
  reap_guard "${pids[$reaped]}" "${slots[$reaped]}" "${names[$reaped]}"
  reaped=$((reaped + 1))
}

drain_pool() {
  while ((reaped < launched)); do
    reap_oldest
  done
}

# Forks one guard into output slot $1 and leaves its pid in started_pid.
# bash's time keyword supplies fractional wall time on Linux and bash 3.2.
start_guard() {
  local slot="$1" name="${2##*/}"
  shift
  (
    # bash 3.2 drops TIMEFORMAT when errexit aborts a failing timed command,
    # so a guard that exits non-zero would publish no duration line. Suspend
    # errexit only around `time` and return the command status unchanged.
    TIMEFORMAT="ci-guardrails: $name %3R s"
    set +e
    if [[ "$slot_prefix" == exclusive- ]]; then
      time { "$@"; }
    else
      time { run_lowered "$@"; }
    fi
    status=$?
    exit "$status"
  ) >"$guard_logs/$slot.out" 2>"$guard_logs/$slot.err" &
  started_pid=$!
}

# Starts one pool member in the next output slot.
launch_guard() {
  local slot="$slot_prefix$next_slot"
  next_slot=$((next_slot + 1))
  start_guard "$slot" "$@"
  pids+=("$started_pid")
  names+=("${1##*/}")
  slots+=("$slot")
  launched=$((launched + 1))
}

# A self-test that spends nearly all of its wall time in fixed waits starts
# beside the pool, outside its window, and is reaped after the pool drains.
# Inside the window it would hold a slot for its whole duration, and because
# the window reaps in launch order, the guards behind it would wait on it.
waiting_pid=""
waiting_name=""
start_waiting_guard() {
  if skipped "$1"; then return 0; fi
  waiting_name="${1##*/}"
  start_guard waiting "$@"
  waiting_pid="$started_pid"
}

reap_waiting_guard() {
  if [[ -z "$waiting_pid" ]]; then return 0; fi
  reap_guard "$waiting_pid" waiting "$waiting_name"
  waiting_pid=""
}

# Self-tests run only when their inputs changed. The QA sweep's --check is also
# a live CLI/MCP/web inventory check; it must still run for crates-only changes,
# unlike fixture-only self-tests.
skipped() {
  local name="${1##*/}"
  if [[ "$static" == true ]]; then
    case "$name" in
      check-workflow-action-pins.sh | check-desktop-ui.sh | smoke-plugin-install.sh)
        # Network checks, Node behavior tests and scratch installations stay
        # in --fast/full mode, outside the file/metadata-only lane.
        return 0
        ;;
    esac
  fi
  if [[ "$name" == test-* && "$name" != test-qa-full-sweep.py && "$run_self_tests" == false ]]; then
    if [[ "$static" == false ]]; then
      echo "ci-guardrails: SKIPPED $name (self-test inputs unchanged)"
    fi
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

# Exclusive self-test: runs in the exclusive lane at normal priority, and is
# drained before the next starts. Use it for suites that assert elapsed time or
# timeouts, measure build-budget admission, or drive a real Cargo target.
run_exclusive_guard() {
  if skipped "$1"; then return 0; fi
  launch_guard "$@"
  drain_pool
}

# Production Clippy's lint environment: what `cargo clippy --workspace --lib
# --bins -- -D warnings -D clippy::disallowed_methods` sets for its `cargo
# check`, spelled out the way with-test-build-lint.sh spells out the folded
# build's. Documentation runs under it too. Cargo fingerprints it, so
# documentation reuses Clippy's check of every workspace library instead of
# checking the workspace again with plain rustc before rustdoc can start.
with_production_lint() {
  RUSTC_WORKSPACE_WRAPPER=clippy-driver \
    CLIPPY_ARGS='-D__CLIPPY_HACKERY__warnings__CLIPPY_HACKERY__-D__CLIPPY_HACKERY__clippy::disallowed_methods__CLIPPY_HACKERY__' \
    "$@"
}

# Production clippy, doctests, then documentation. Each takes the Cargo build
# lock, so they run in sequence; a failure still reports the later phases, and
# the first failure is the chain's status. Production clippy is the only pass
# that denies clippy::disallowed_methods, so test-only channels stay exempt on
# the folded build; it checks lib and bin targets and writes no test binary.
# Doctests use the folded build's lint environment and so reuse its library
# artifacts.
run_check_chain() {
  local status=0 next=0
  run_phase clippy-production with_production_lint run_lowered cargo check --workspace --lib --bins || status=$?
  run_phase doctests run_lowered "$lint" cargo test --no-fail-fast --workspace --doc || next=$?
  if [[ "$status" -eq 0 ]]; then status="$next"; fi
  next=0
  RUSTDOCFLAGS="-D warnings" run_phase documentation with_production_lint run_lowered cargo doc --no-deps --workspace || next=$?
  if [[ "$status" -eq 0 ]]; then status="$next"; fi
  return "$status"
}

# Lower priority at command launch, never by changing a parent process:
# macOS workers can lower their own priority but cannot renice their parent
# inside Seatbelt. Probe idle scheduling without executing the real command,
# so a command failure is never retried under the fallback scheduler. nice -n
# is an increment on both platforms and preserves an already-niced worker.
lowered_command=(nice -n 19)
if chrt --idle 0 true >/dev/null 2>&1; then
  lowered_command=(chrt --idle 0)
fi

# Runs an external command and its descendants at lowered priority. Keep nice
# errors visible if the environment also denies changing one's own priority.
run_lowered() {
  "${lowered_command[@]}" "$@"
}

# Runs the clippy/doctest/documentation chain in the background while nextest
# runs. At idle priority it takes only the CPU nextest leaves unused, so the
# workspace suites' elapsed-time and timeout assertions keep the host to
# themselves. None of its commands writes the test binaries nextest executes.
start_check_chain() {
  (
    # The report trap belongs to the parent shell, not to the chain.
    trap - EXIT
    phase_records=()
    status=0
    run_check_chain || status=$?
    printf '%s\n' "${phase_records[@]}" >"$guard_logs/checks.phases"
    exit "$status"
  ) >"$guard_logs/checks.out" 2>"$guard_logs/checks.err" &
  check_chain_pid=$!
}

# Exclusive self-tests run one at a time, at normal priority, in a lane that
# starts after formatting. Each one asserts elapsed time or timeouts, measures
# build-budget admission, or drives a real Cargo target; the rendezvous and
# deadline checks in ci-fast-guards count too. Everything beside them, the guard
# pool and in full mode the workflow test build, starts at lowered priority to
# reduce CPU contention with the suites. The lane joins after the pool and
# before the workspace tests start. Each suite is mostly one process, so the
# lane leaves most of the host to that lowered work.
guard_phase_started=$SECONDS
if [[ "$static" == false ]]; then
  exclusive_started=$SECONDS
  (
    # The report trap belongs to the parent shell, not to the lane.
    trap - EXIT
    slot_prefix=exclusive-
    run_exclusive_guard "$repo_root/scripts/test-build-budget.sh"
    run_exclusive_guard "$repo_root/scripts/test-ci-fast-guards.py"
    run_exclusive_guard "$repo_root/scripts/test-ci-test-affected.py"
    run_exclusive_guard "$repo_root/scripts/test-codeql-rust-local.py"
    run_exclusive_guard "$repo_root/scripts/test-compiler-cache.sh"
    run_exclusive_guard "$repo_root/scripts/test-compiler-cache-namespaces.sh"
    exit "$failed"
  ) >"$guard_logs/exclusive.out" 2>"$guard_logs/exclusive.err" &
  exclusive_lane_pid=$!
fi

(
  # The report trap belongs to the parent shell, not to the pool.
  trap - EXIT
  # Beside the exclusive lane, each pool command starts at lowered priority.
  # About 6 s of CPU time across about 63 s of wall time, nearly all of it
  # fixed slow-child and hang delays. The helper's deadlines run on an
  # injected clock, and a fixture step fails only after 120 s without output,
  # which no correct run nears on a saturated host, so it runs beside the
  # build instead of extending the exclusive suites.
  start_waiting_guard "$repo_root/scripts/test-ci-download-verified.py"
  # Start the longer repository scans first; the pool keeps pool_width running.
  run_guard "$repo_root/scripts/check-orphan-modules.sh"
  run_guard "$repo_root/scripts/check-workflow-action-pins.sh"
  if [[ "$static" == true ]]; then
    run_guard "$repo_root/scripts/test-qa-full-sweep.py" --check --static
  else
    run_guard "$repo_root/scripts/test-qa-full-sweep.py" --check
  fi
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
  # The exclusive suites run in their own lane, above.
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
  run_guard "$repo_root/scripts/test-check-nextest-partitions.py"
  run_guard "$repo_root/scripts/test-ci-await-jobs.py"
  run_guard "$repo_root/scripts/test-cpu-utilisation.py"
  drain_pool
  reap_waiting_guard
  exit "$failed"
) >"$guard_logs/pool.out" 2>"$guard_logs/pool.err" &
guard_pool_pid=$!

# Every Cargo command that compiles workspace members after the folded build
# runs under its lint environment. Cargo fingerprints it, so without it the
# nextest pass and doctests would recompile the whole workspace.
lint="$repo_root/scripts/with-test-build-lint.sh"

if [[ "$checks" == true ]]; then
  # The hosted Check job builds no test binaries: the test-build job does, and
  # the shard jobs run nextest. The check chain starts beside the pool and the
  # exclusive lane, in the idle class like the build it replaces there, so the
  # exclusive suites keep normal-priority CPU to themselves. Doctests build
  # their own libraries here under the same lint environment.
  start_check_chain
  join_guard_pool
  join_exclusive_lane
  guards_joined=$SECONDS
  join_check_chain
elif [[ "$fast" == false ]]; then
  # Enumerating workflow tests compiles their targets; keep it out of ci-fast.
  # --workspace-build lists them from the same `--workspace --lib --bins
  # --tests` artifacts the nextest pass below runs, instead of a per-crate
  # `cargo test -p` build that resolves features differently and recompiles
  # the workspace chain a second time. [DANI-10428]
  # The list-build lints lib, bins, tests and examples with the former
  # all-targets flags (see with-test-build-lint.sh). That replaces the separate
  # clippy-all-targets phase, which re-checked the same crates after this
  # compile. Production clippy stays its own pass, in the background check
  # chain (run_check_chain). make ci-lint still runs a separate all-targets
  # clippy because that gate does not build test binaries.
  # It runs at lowered priority beside the exclusive lane.
  run_phase workflow-test-list-build run_lowered "$repo_root/scripts/check-ci-macos.sh" --workspace-build
  # The pool and the exclusive lane have overlapped the build. Join both
  # before any test runs: the workspace suites assert elapsed time and
  # timeouts, so nothing else at normal priority shares the host with them.
  join_guard_pool
  join_exclusive_lane
  guards_joined=$SECONDS
  if cargo nextest --version >/dev/null 2>&1; then
    # List the folded build's test binaries (fresh, so nothing compiles), then
    # run nextest from that listing. The run invokes no Cargo build and holds
    # no build lock, so the check chain can use Cargo meanwhile.
    run_phase workspace-test-listing "$lint" cargo nextest list --list-type binaries-only \
      --message-format json --workspace --lib --bins --tests >"$guard_logs/binaries.json"
    cargo metadata --format-version 1 --all-features --no-deps >"$guard_logs/cargo-metadata.json"
    start_check_chain
    # Profile ci raises test-threads above the 4-vCPU runner default.
    # The sampler prints one cpu-util line and keeps nextest's exit status.
    run_phase workspace-tests "$repo_root/scripts/cpu-utilisation.sh" --label nextest -- \
      cargo nextest run --profile ci --no-fail-fast \
      --success-output immediate \
      --binaries-metadata "$guard_logs/binaries.json" \
      --cargo-metadata "$guard_logs/cargo-metadata.json"
    join_check_chain
  else
    echo "cargo-nextest not found; falling back to cargo test" >&2
    run_phase workspace-tests "$lint" cargo test --no-fail-fast --workspace --lib --bins --tests
    checks_status=0
    run_check_chain || checks_status=$?
    if [[ "$failed" -eq 0 ]]; then failed="$checks_status"; fi
  fi
fi

if [[ "$fast" == false ]]; then
  # Supply-chain gate: dependency advisories + license allow-list (deny.toml).
  # [ORB-00416] [ORB-11983] Soft-presence like nextest above — CI installs a pinned version;
  # local runs without the tool warn instead of hard-failing. It may fetch the
  # advisory database, so it runs at normal priority after the tests.
  if command -v cargo-deny >/dev/null 2>&1; then
    run_phase supply-chain "$repo_root/scripts/cargo-deny.sh" check
  else
    echo "cargo-deny not found; skipping supply-chain gate (install: cargo install cargo-deny --locked)" >&2
  fi
fi

# Fast mode joins here; the other modes already joined.
if [[ "$fast" == true ]]; then
  join_guard_pool
  join_exclusive_lane
  guards_joined=$SECONDS
fi

# From the start of the pool and the exclusive lane to the later of their
# joins. In full mode that includes the workflow test build they overlap.
phase_records+=("repository-guards wall_seconds=$((guards_joined - guard_phase_started)) exit=$failed")
exit "$failed"
