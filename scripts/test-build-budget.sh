#!/usr/bin/env bash
# Deterministic process tests for the cross-worktree build budget [ORB-11754] [ORB-11760].
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAPPER="$ROOT/scripts/build-budget.py"
TMP="$(mktemp -d)"
BACKGROUND_PIDS=()
TEST_COMPLETE=0

# Clear any build-budget environment inherited from an outer wrapper (e.g. `make ci`).
# Fixtures manage their own slots, lock directory, and admission hermetically [ORB-12350].
unset ORBIT_BUILD_BUDGET ORBIT_BUILD_BUDGET_DIR ORBIT_BUILD_BUDGET_HELD \
  ORBIT_BUILD_BUDGET_SLOT ORBIT_BUILD_SLOTS ORBIT_CARGO_JOBS CARGO_BUILD_JOBS
for var in $(compgen -v ORBIT_BUILD_ 2>/dev/null || true); do
  unset "$var"
done

cleanup() {
  local status=$?
  local pid

  # Bash 3.2 can report success after a nounset error inside a function.
  # Cleanup must not turn an incomplete assertion run into a passing gate.
  if [[ "$status" == 0 && "$TEST_COMPLETE" != 1 ]]; then
    status=1
  fi

  if ((${#BACKGROUND_PIDS[@]})); then
    for pid in "${BACKGROUND_PIDS[@]}"; do
      kill "$pid" 2>/dev/null || true
    done
  fi
  rm -rf "$TMP"
  exit "$status"
}
trap cleanup EXIT

fail() {
  printf 'test-build-budget: FAIL: %s\n' "$*" >&2
  exit 1
}

forget_pid() {
  local target="$1"
  local pid
  local remaining=()
  for pid in "${BACKGROUND_PIDS[@]}"; do
    if [[ "$pid" != "$target" ]]; then
      remaining+=("$pid")
    fi
  done

  BACKGROUND_PIDS=()
  # Bash 3.2 treats an empty array expansion as unset under nounset.
  if ((${#remaining[@]})); then
    BACKGROUND_PIDS=("${remaining[@]}")
  fi
}

wait_for() {
  local path="$1"
  local attempts=0
  while [[ ! -e "$path" ]]; do
    attempts=$((attempts + 1))
    [[ "$attempts" -lt 200 ]] || fail "timed out waiting for $path"
    sleep 0.02
  done
}

mkdir -p "$TMP/state" "$TMP/worktrees/a" "$TMP/worktrees/b" \
  "$TMP/worktrees/c" "$TMP/worktrees/d"

cat >"$TMP/helper.py" <<'PY'
#!/usr/bin/env python3
import fcntl
import os
from pathlib import Path
import signal
import sys
import time

state = Path(sys.argv[1])
label = sys.argv[2]
mode = sys.argv[3]
duration = float(sys.argv[4]) if len(sys.argv) > 4 else 0.0
active = state / "active"
active.mkdir(parents=True, exist_ok=True)
lock_file = (state / "state.lock").open("a+")


def update(event):
    fcntl.flock(lock_file, fcntl.LOCK_EX)
    try:
        if event == "start":
            (active / label).touch()
            current = len(list(active.iterdir()))
            maximum_path = state / "max"
            previous = int(maximum_path.read_text()) if maximum_path.exists() else 0
            maximum_path.write_text(f"{max(previous, current)}\n")
            (state / f"started-{label}").touch()
        else:
            (active / label).unlink(missing_ok=True)
        with (state / "events").open("a") as events:
            events.write(
                f"{event} {label} cwd={Path.cwd()} jobs={os.environ.get('CARGO_BUILD_JOBS')} "
                f"slot={os.environ.get('ORBIT_BUILD_BUDGET_SLOT')} "
                f"target={os.environ.get('CARGO_TARGET_DIR')}\n"
            )
    finally:
        fcntl.flock(lock_file, fcntl.LOCK_UN)


def terminate(_signal, _frame):
    raise SystemExit(143)


signal.signal(signal.SIGTERM, terminate)
update("start")
try:
    if mode == "sleep":
        time.sleep(duration)
    elif mode == "block":
        while True:
            time.sleep(1)
    elif mode == "fail":
        time.sleep(duration)
        raise SystemExit(23)
finally:
    update("end")
PY
chmod +x "$TMP/helper.py"

# Different worktree paths share two slots, and enough overlapping work reaches both.
pids=()
for label in a b c d; do
  (
    cd "$TMP/worktrees/$label"
    CARGO_TARGET_DIR="$TMP/targets/$label" ORBIT_BUILD_BUDGET_DIR="$TMP/locks-cross" \
      ORBIT_BUILD_SLOTS=2 ORBIT_CARGO_JOBS=3 \
      "$WRAPPER" -- "$TMP/helper.py" "$TMP/state" "$label" sleep 0.25
  ) &
  pids+=("$!")
done
for pid in "${pids[@]}"; do
  wait "$pid"
done
[[ "$(cat "$TMP/state/max")" == "2" ]] || fail "cross-worktree concurrency was not exactly two"
[[ "$(grep -c '^start ' "$TMP/state/events")" == "4" ]] || fail "not all cross-worktree commands ran"
[[ "$(grep -c '^start .* jobs=3 ' "$TMP/state/events")" == "4" ]] || fail "Cargo job limit was not exported"
for label in a b c d; do
  grep -Fq "target=$TMP/targets/$label" "$TMP/state/events" || fail "$label lost its private target directory"
done

run_queue_case() {
  local case_name="$1" first_mode="$2"
  local case_state="$TMP/$case_name"
  local owner_status
  mkdir -p "$case_state"
  ORBIT_BUILD_BUDGET_DIR="$TMP/locks-$case_name" ORBIT_BUILD_SLOTS=1 \
    "$WRAPPER" -- "$TMP/helper.py" "$case_state" owner "$first_mode" 0.25 &
  local owner_pid=$!
  wait_for "$case_state/started-owner"
  ORBIT_BUILD_BUDGET_DIR="$TMP/locks-$case_name" ORBIT_BUILD_SLOTS=1 \
    "$WRAPPER" -- "$TMP/helper.py" "$case_state" queued sleep 0.01 &
  local queued_pid=$!
  sleep 0.08
  [[ ! -e "$case_state/started-queued" ]] || fail "$case_name queue started before the owner released its slot"

  if [[ "$first_mode" == "block" ]]; then
    kill -TERM "$owner_pid"
  fi
  set +e
  wait "$owner_pid" 2>/dev/null
  owner_status=$?
  set -e
  case "$first_mode" in
    sleep) [[ "$owner_status" == "0" ]] || fail "$case_name owner returned $owner_status" ;;
    fail) [[ "$owner_status" == "23" ]] || fail "$case_name did not preserve failure status" ;;
    block) [[ "$owner_status" == "143" ]] || fail "$case_name terminated owner returned $owner_status" ;;
  esac
  wait "$queued_pid"
  [[ -e "$case_state/started-queued" ]] || fail "$case_name queue did not proceed"
}

run_queue_case after-success sleep
run_queue_case after-failure fail
run_queue_case after-termination block

# A nested admitted entry point must not try to acquire a second slot.
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-nested" ORBIT_BUILD_SLOTS=1 ORBIT_CARGO_JOBS=5 \
  timeout 5 "$WRAPPER" -- "$WRAPPER" -- "$TMP/helper.py" "$TMP/nested" nested sleep 0.01
grep -Fq 'jobs=5' "$TMP/nested/events" || fail "nested command lost Cargo job limit"

# Exercise a real Make entry point inside an existing admission. The inner
# wrapper inherits the marker and must not wait on the only slot.
cat >"$TMP/fake-cargo" <<'SH'
#!/usr/bin/env bash
printf 'jobs=%s\n' "${CARGO_BUILD_JOBS:-}" >"${FAKE_CARGO_LOG}"
printf '%s\n' "$@" >>"${FAKE_CARGO_LOG}"
SH
chmod +x "$TMP/fake-cargo"
FAKE_CARGO_LOG="$TMP/make.log" ORBIT_BUILD_BUDGET_DIR="$TMP/locks-make" \
  ORBIT_BUILD_SLOTS=1 ORBIT_CARGO_JOBS=9 timeout 5 "$WRAPPER" -- \
  make -s -C "$ROOT" check CARGO="$TMP/fake-cargo" BUILD_BUDGET="$WRAPPER"
grep -Fxq 'jobs=9' "$TMP/make.log" || fail "nested Make entry lost Cargo job limit"
grep -Fxq 'check' "$TMP/make.log" || fail "nested Make entry did not invoke cargo check"
grep -Fxq -- '--workspace' "$TMP/make.log" || fail "nested Make entry lost workspace arguments"

# CARGO_BUILD_JOBS is a supported caller override; ORBIT_CARGO_JOBS is more specific.
CARGO_BUILD_JOBS=6 ORBIT_BUILD_BUDGET_DIR="$TMP/locks-override" \
  "$WRAPPER" -- "$TMP/helper.py" "$TMP/override" cargo-override sleep 0.01
grep -Fq 'jobs=6' "$TMP/override/events" || fail "CARGO_BUILD_JOBS override was not honored"
CARGO_BUILD_JOBS=6 ORBIT_CARGO_JOBS=7 ORBIT_BUILD_BUDGET_DIR="$TMP/locks-override" \
  "$WRAPPER" -- "$TMP/helper.py" "$TMP/override" orbit-override sleep 0.01
grep -Fq 'jobs=7' "$TMP/override/events" || fail "ORBIT_CARGO_JOBS did not take precedence"

for setting in 'ORBIT_BUILD_SLOTS=0' 'ORBIT_BUILD_SLOTS=banana' \
  'ORBIT_CARGO_JOBS=0' 'ORBIT_CARGO_JOBS=4.5' 'ORBIT_BUILD_BUDGET=maybe'; do
  set +e
  env "$setting" "$WRAPPER" -- true >/dev/null 2>"$TMP/invalid.err"
  status=$?
  set -e
  [[ "$status" == "64" ]] || fail "$setting returned $status instead of 64"
  grep -Fq 'build-budget:' "$TMP/invalid.err" || fail "$setting did not explain the invalid value"
done

# Explicit bypass avoids admission but retains the resolved Cargo job setting.
ORBIT_BUILD_BUDGET=0 ORBIT_CARGO_JOBS=8 \
  "$WRAPPER" -- "$TMP/helper.py" "$TMP/bypass" bypass sleep 0.01
grep -Fq 'jobs=8' "$TMP/bypass/events" || fail "bypass lost Cargo job setting"
grep -Fq 'slot=None' "$TMP/bypass/events" || fail "bypass unexpectedly acquired a slot"

cat >"$TMP/fake-app" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" >"${FAKE_RUN_ARGS}"
touch "${FAKE_RUN_STARTED}"
while [[ ! -e "${FAKE_RUN_RELEASE}" ]]; do
  sleep 0.02
done
exit "${FAKE_RUN_EXIT:-0}"
SH
chmod +x "$TMP/fake-app"

cat >"$TMP/fake-make-cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
cmd="${1:-}"
if [[ "$#" -gt 0 ]]; then
  shift
fi
printf 'event=invoke cmd=%s slot=%s held=%s\n' \
  "$cmd" "${ORBIT_BUILD_BUDGET_SLOT:-}" "${ORBIT_BUILD_BUDGET_HELD:-}" \
  >>"${FAKE_MAKE_LOG}"

case "$cmd" in
  build)
    printf '%s\n' "$@" >>"${FAKE_BUILD_ARGS}"
    case "${FAKE_ARTIFACT_MODE:-ok}" in
      ok)
        python3 -c 'import json, os; print(json.dumps({"reason":"compiler-artifact","executable":os.environ["FAKE_APP"],"target":{"name":"orbit"}}))'
        ;;
      none)
        python3 -c 'import json; print(json.dumps({"reason":"compiler-artifact","executable":None,"target":{"name":"orbit"}}))'
        ;;
      malformed)
        printf 'not-json\n'
        ;;
      *)
        exit 64
        ;;
    esac
    touch "${FAKE_BUILD_STARTED}"
    exit "${FAKE_BUILD_EXIT:-0}"
    ;;
  run)
    printf 'event=invoke cmd=run-after-release\n' >>"${FAKE_MAKE_LOG}"
    printf 'fake cargo run must not run after the build slot is released\n' >&2
    exit 64
    ;;
  watch)
    trap 'exit 143' TERM
    printf '%s\n' "$$" >"${FAKE_WATCH_PID}"
    commands=()
    while [[ "$#" -gt 0 ]]; do
      if [[ "$1" == "-s" ]]; then
        [[ "$#" -ge 2 ]] || exit 2
        commands+=("$2")
        shift 2
      else
        shift
      fi
    done
    [[ "${#commands[@]}" -gt 0 ]] || exit 2
    export FAKE_WATCH_ITER=1
    for shell_cmd in "${commands[@]}"; do
      sh -c "$shell_cmd"
    done
    touch "${FAKE_WATCH_IDLE}"
    while [[ ! -e "${FAKE_WATCH_AGAIN}" ]]; do
      sleep 0.02
    done
    export FAKE_WATCH_ITER=2
    for shell_cmd in "${commands[@]}"; do
      sh -c "$shell_cmd"
    done
    touch "${FAKE_WATCH_SECOND}"
    while true; do
      sleep 1
    done
    ;;
  check|test)
    printf 'event=iteration cmd=%s iter=%s slot=%s held=%s\n' \
      "$cmd" "${FAKE_WATCH_ITER:-}" "${ORBIT_BUILD_BUDGET_SLOT:-}" \
      "${ORBIT_BUILD_BUDGET_HELD:-}" >>"${FAKE_MAKE_LOG}"
    touch "${FAKE_WATCH_DIR}/started-${cmd}-${FAKE_WATCH_ITER:-unknown}"
    if [[ "$cmd" == "check" && "${FAKE_WATCH_ITER:-}" == "1" ]]; then
      sleep "${FAKE_CHECK_SLEEP:-0}"
    fi
    ;;
  *)
    exit 64
    ;;
esac
SH
chmod +x "$TMP/fake-make-cargo"

# make run admits compilation, then launches the resolved binary without cargo run.
FAKE_APP="$TMP/fake-app"
FAKE_MAKE_LOG="$TMP/make-run.log"
FAKE_BUILD_ARGS="$TMP/make-run-build-args"
FAKE_BUILD_STARTED="$TMP/make-run-build-started"
FAKE_RUN_ARGS="$TMP/make-run-app-args"
FAKE_RUN_STARTED="$TMP/make-run-app-started"
FAKE_RUN_RELEASE="$TMP/make-run-app-release"
FAKE_RUN_EXIT=0
FAKE_BUILD_EXIT=0
FAKE_ARTIFACT_MODE=ok
FAKE_WATCH_DIR="$TMP/run-unused-watch"
mkdir -p "$FAKE_WATCH_DIR"
: >"$FAKE_MAKE_LOG"
: >"$FAKE_BUILD_ARGS"
export FAKE_APP FAKE_MAKE_LOG FAKE_BUILD_ARGS FAKE_BUILD_STARTED FAKE_RUN_ARGS \
  FAKE_RUN_STARTED FAKE_RUN_RELEASE FAKE_RUN_EXIT FAKE_BUILD_EXIT \
  FAKE_ARTIFACT_MODE FAKE_WATCH_DIR

ORBIT_BUILD_BUDGET_DIR="$TMP/locks-run" ORBIT_BUILD_SLOTS=1 \
  make -s -C "$ROOT" run CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER" \
  ARGS='alpha beta' >"$TMP/make-run.out" 2>"$TMP/make-run.err" &
RUN_PID=$!
BACKGROUND_PIDS+=("$RUN_PID")
wait_for "$FAKE_RUN_STARTED"
grep -Eq '^event=invoke cmd=build slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "make run build was not admitted"
[[ "$(grep -c '^event=invoke cmd=build ' "$FAKE_MAKE_LOG")" == "1" ]] \
  || fail "make run invoked cargo build more than once"
if grep -Eq '^event=invoke cmd=run' "$FAKE_MAKE_LOG"; then
  fail "make run invoked a compilation-capable cargo run after slot release"
fi
grep -Fxq -- '-p' "$FAKE_BUILD_ARGS" && grep -Fxq -- 'orbit-cli' "$FAKE_BUILD_ARGS" \
  && grep -Fxq -- '--bin' "$FAKE_BUILD_ARGS" && grep -Fxq -- 'orbit' "$FAKE_BUILD_ARGS" \
  && grep -Fxq -- '--message-format=json-render-diagnostics' "$FAKE_BUILD_ARGS" \
  || fail "make run build lost package, binary, or artifact-format arguments"
printf 'alpha\nbeta\n' >"$TMP/expected-run-args"
diff -q "$FAKE_RUN_ARGS" "$TMP/expected-run-args" >/dev/null \
  || fail "make run did not preserve application arguments"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-run" ORBIT_BUILD_SLOTS=1 \
  timeout 2 "$WRAPPER" -- true \
  || fail "make run application runtime retained the build slot"
touch "$FAKE_RUN_RELEASE"
set +e
wait "$RUN_PID"
run_status=$?
set -e
forget_pid "$RUN_PID"
[[ "$run_status" == "0" ]] || fail "make run lost a successful application exit ($run_status)"

FAKE_RUN_EXIT=42
export FAKE_RUN_EXIT
rm -f "$FAKE_RUN_STARTED"
: >"$FAKE_MAKE_LOG"
: >"$FAKE_BUILD_ARGS"
touch "$FAKE_RUN_RELEASE"
set +e
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-run" ORBIT_BUILD_SLOTS=1 \
  make -s -C "$ROOT" run CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER" \
  ARGS='alpha beta' >"$TMP/make-run-fail.out" 2>"$TMP/make-run-fail.err"
fail_status=$?
set -e
[[ "$fail_status" != "0" ]] || fail "make run succeeded despite application exit 42"
grep -Fq 'Error 42' "$TMP/make-run-fail.err" \
  || fail "make run did not report application exit status 42"
if grep -Eq '^event=invoke cmd=run' "$FAKE_MAKE_LOG"; then
  fail "failing make run invoked cargo run after slot release"
fi

assert_make_run_does_not_start_app() {
  local label="$1"
  rm -f "$FAKE_RUN_STARTED"
  : >"$FAKE_MAKE_LOG"
  : >"$FAKE_BUILD_ARGS"
  set +e
  ORBIT_BUILD_BUDGET_DIR="$TMP/locks-run" ORBIT_BUILD_SLOTS=1 \
    make -s -C "$ROOT" run CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER" \
    ARGS='should-not-run' >"$TMP/make-run-$label.out" 2>"$TMP/make-run-$label.err"
  local status=$?
  set -e
  [[ "$status" != "0" ]] || fail "make run $label succeeded"
  [[ ! -e "$FAKE_RUN_STARTED" ]] || fail "make run $label executed the built artifact"
  if grep -Eq '^event=invoke cmd=run' "$FAKE_MAKE_LOG"; then
    fail "make run $label invoked cargo run"
  fi
}

FAKE_ARTIFACT_MODE=ok
FAKE_BUILD_EXIT=23
export FAKE_ARTIFACT_MODE FAKE_BUILD_EXIT
assert_make_run_does_not_start_app failed-build
grep -Fq 'Error 23' "$TMP/make-run-failed-build.err" \
  || fail "make run did not stop on cargo build failure"

FAKE_ARTIFACT_MODE=malformed
FAKE_BUILD_EXIT=0
export FAKE_ARTIFACT_MODE FAKE_BUILD_EXIT
assert_make_run_does_not_start_app malformed-artifact
grep -Fq 'make run: cargo did not report an executable' "$TMP/make-run-malformed-artifact.err" \
  || fail "make run did not reject malformed cargo artifact JSON"

FAKE_ARTIFACT_MODE=none
export FAKE_ARTIFACT_MODE
assert_make_run_does_not_start_app missing-executable
grep -Fq 'make run: cargo did not report an executable' "$TMP/make-run-missing-executable.err" \
  || fail "make run did not reject a cargo artifact without an executable"

FAKE_ARTIFACT_MODE=ok
FAKE_BUILD_EXIT=0
export FAKE_ARTIFACT_MODE FAKE_BUILD_EXIT

# make watch admits each check/test iteration; idle watcher lifetime does not hold a slot.
FAKE_MAKE_LOG="$TMP/make-watch.log"
FAKE_WATCH_DIR="$TMP/watch-state"
FAKE_WATCH_IDLE="$TMP/watch-idle"
FAKE_WATCH_AGAIN="$TMP/watch-again"
FAKE_WATCH_SECOND="$TMP/watch-second"
FAKE_WATCH_PID="$TMP/watch-pid"
FAKE_CHECK_SLEEP=0.8
mkdir -p "$FAKE_WATCH_DIR"
: >"$FAKE_MAKE_LOG"
export FAKE_MAKE_LOG FAKE_WATCH_DIR FAKE_WATCH_IDLE FAKE_WATCH_AGAIN \
  FAKE_WATCH_SECOND FAKE_WATCH_PID FAKE_CHECK_SLEEP

ORBIT_BUILD_BUDGET_DIR="$TMP/locks-watch" ORBIT_BUILD_SLOTS=1 \
  make -s -C "$ROOT" watch CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER" \
  >"$TMP/make-watch.out" 2>"$TMP/make-watch.err" &
WATCH_MAKE_PID=$!
BACKGROUND_PIDS+=("$WATCH_MAKE_PID")
wait_for "$FAKE_WATCH_DIR/started-check-1"
set +e
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-watch" ORBIT_BUILD_SLOTS=1 \
  timeout 0.4 "$WRAPPER" -- true
during_check=$?
set -e
[[ "$during_check" == "124" ]] || fail "watch check iteration did not hold a slot (status $during_check)"
wait_for "$FAKE_WATCH_IDLE"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-watch" ORBIT_BUILD_SLOTS=1 \
  timeout 2 "$WRAPPER" -- true \
  || fail "idle watch retained a build slot"
grep -Eq '^event=invoke cmd=watch slot= held=$' "$FAKE_MAKE_LOG" \
  || fail "watch driver should not hold a slot"
grep -Eq '^event=iteration cmd=check iter=1 slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "first watch check was not admitted"
grep -Eq '^event=iteration cmd=test iter=1 slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "first watch test was not admitted"
touch "$FAKE_WATCH_AGAIN"
wait_for "$FAKE_WATCH_SECOND"
grep -Eq '^event=iteration cmd=check iter=2 slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "second watch check was not admitted"
grep -Eq '^event=iteration cmd=test iter=2 slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "second watch test was not admitted"
if [[ -f "$FAKE_WATCH_PID" ]]; then
  kill "$(cat "$FAKE_WATCH_PID")" 2>/dev/null || true
fi
kill "$WATCH_MAKE_PID" 2>/dev/null || true
wait "$WATCH_MAKE_PID" 2>/dev/null || true
forget_pid "$WATCH_MAKE_PID"

TEST_COMPLETE=1
printf 'test-build-budget: ok\n'
