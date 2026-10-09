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
  ORBIT_BUILD_BUDGET_SLOT ORBIT_BUILD_SLOTS ORBIT_CARGO_JOBS CARGO_BUILD_JOBS \
  _ORBIT_BUILD_BUDGET_TEST_WAIT_INTERVAL_SECONDS
unset ORBIT_ACTIVITY_BUILD_BUDGET_DIR
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
  rm -f "$ROOT/target/release/orbit" "$ROOT/target/debug/orbit"
  rmdir "$ROOT/target/release" "$ROOT/target/debug" 2>/dev/null || true
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
    if mode == "term-delay":
        (state / "terminating").touch()
        while not (state / "release-owner").exists():
            time.sleep(0.02)
    raise SystemExit(143)


signal.signal(signal.SIGTERM, terminate)
update("start")
try:
    if mode == "sleep":
        time.sleep(duration)
    elif mode in {"block", "term-delay"}:
        while True:
            time.sleep(1)
    elif mode == "fail":
        time.sleep(duration)
        raise SystemExit(23)
    elif mode == "detached":
        if os.fork() == 0:
            os.setsid()
            signal.signal(signal.SIGTERM, signal.SIG_DFL)
            lock_file.close()
            (state / "detached-pid").write_text(str(os.getpid()))
            while True:
                time.sleep(1)
        while not (state / "release-owner").exists():
            time.sleep(0.02)
finally:
    update("end")
PY
chmod +x "$TMP/helper.py"

# Agent subprocesses retain HOME and PATH but receive no ORBIT_* settings.
# The host files must still control admission, and an explicit slot override
# must beat the host file when one is present.
run_agent_environment_case() {
  local case_name="$1" expected_slots="$2" override_slots="${3:-}"
  local home="$TMP/agent-home-$case_name"
  local state="$TMP/agent-state-$case_name"
  local pids=()
  mkdir -p "$home" "$state"

  for label in a b c d; do
    if [[ -n "$override_slots" ]]; then
      (env -i HOME="$home" PATH="$PATH" ORBIT_BUILD_SLOTS="$override_slots" \
        "$WRAPPER" -- "$TMP/helper.py" "$state" "$label" sleep 0.35) &
    else
      (env -i HOME="$home" PATH="$PATH" \
        "$WRAPPER" -- "$TMP/helper.py" "$state" "$label" sleep 0.35) &
    fi
    pids+=("$!")
  done

  local pid
  for pid in "${pids[@]}"; do
    wait "$pid"
  done
  [[ "$(cat "$state/max")" == "$expected_slots" ]] \
    || fail "$case_name agent environment admitted $(cat "$state/max") commands, expected $expected_slots"
}

run_agent_environment_case agent-default 2

agent_host_home="$TMP/agent-home-agent-host-file"
mkdir -p "$agent_host_home/.orbit/cache/build-budget"
printf '4\n' >"$agent_host_home/.orbit/cache/build-budget/slots"
printf '6\n' >"$agent_host_home/.orbit/cache/build-budget/cargo-jobs"
run_agent_environment_case agent-host-file 4
env -i HOME="$agent_host_home" PATH="$PATH" \
  "$WRAPPER" -- "$TMP/helper.py" "$TMP/agent-job-file" jobs-from-host-file sleep 0.01
grep -Fq 'jobs=6' "$TMP/agent-job-file/events" || fail "host cargo-jobs file was not honored"
env -i HOME="$agent_host_home" PATH="$PATH" ORBIT_CARGO_JOBS=7 \
  "$WRAPPER" -- "$TMP/helper.py" "$TMP/agent-job-override" jobs-from-env sleep 0.01
grep -Fq 'jobs=7' "$TMP/agent-job-override/events" || fail "ORBIT_CARGO_JOBS did not override the host file"

agent_override_home="$TMP/agent-home-agent-override"
mkdir -p "$agent_override_home/.orbit/cache/build-budget"
printf '4\n' >"$agent_override_home/.orbit/cache/build-budget/slots"
run_agent_environment_case agent-override 1 1

for invalid_slots in banana 129; do
  invalid_home="$TMP/agent-home-invalid-$invalid_slots"
  mkdir -p "$invalid_home/.orbit/cache/build-budget"
  printf '%s\n' "$invalid_slots" >"$invalid_home/.orbit/cache/build-budget/slots"
  set +e
  env -i HOME="$invalid_home" PATH="$PATH" "$WRAPPER" -- true \
    >/dev/null 2>"$TMP/invalid-host-slots.err"
  status=$?
  set -e
  [[ "$status" == "64" ]] || fail "host slots value $invalid_slots returned $status instead of 64"
  grep -Fq 'ORBIT_BUILD_SLOTS must be a decimal integer from 1 through 128' \
    "$TMP/invalid-host-slots.err" || fail "host slots value $invalid_slots had the wrong validation message"
done

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

# A detached descendant must not retain admission after its direct parent exits.
# fork() deliberately retains inherited descriptors to reproduce the leaked flock.
detached_state="$TMP/detached"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-detached" ORBIT_BUILD_SLOTS=1 \
  "$WRAPPER" -- "$TMP/helper.py" "$detached_state" owner detached &
owner_pid=$!
BACKGROUND_PIDS+=("$owner_pid")
wait_for "$detached_state/detached-pid"
detached_pid="$(cat "$detached_state/detached-pid")"
BACKGROUND_PIDS+=("$detached_pid")
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-detached" ORBIT_BUILD_SLOTS=1 \
  timeout 2 "$WRAPPER" -- "$TMP/helper.py" "$detached_state" queued sleep 0.01 \
  2>"$TMP/detached-queue.err" &
queued_pid=$!
BACKGROUND_PIDS+=("$queued_pid")
sleep 0.08
[[ ! -e "$detached_state/started-queued" ]] \
  || fail "detached case admitted a second command while the owner was running"
touch "$detached_state/release-owner"
wait "$owner_pid"
forget_pid "$owner_pid"
wait "$queued_pid" || fail "a surviving detached child retained the build slot"
forget_pid "$queued_pid"
kill -0 "$detached_pid" || fail "detached child exited before the admission assertion"
kill -TERM "$detached_pid"
forget_pid "$detached_pid"

# Forward cancellation, but keep admission until the command finishes handling it.
termination_state="$TMP/termination-delay"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-termination-delay" ORBIT_BUILD_SLOTS=1 \
  "$WRAPPER" -- "$TMP/helper.py" "$termination_state" owner term-delay &
owner_pid=$!
BACKGROUND_PIDS+=("$owner_pid")
wait_for "$termination_state/started-owner"
kill -TERM "$owner_pid"
wait_for "$termination_state/terminating"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-termination-delay" ORBIT_BUILD_SLOTS=1 \
  timeout 2 "$WRAPPER" -- "$TMP/helper.py" "$termination_state" queued sleep 0.01 &
queued_pid=$!
BACKGROUND_PIDS+=("$queued_pid")
sleep 0.08
[[ ! -e "$termination_state/started-queued" ]] \
  || fail "cancellation released the slot before the command finished"
touch "$termination_state/release-owner"
set +e
wait "$owner_pid"
owner_status=$?
set -e
forget_pid "$owner_pid"
[[ "$owner_status" == "143" ]] || fail "cancellation lost the command's exit status"
wait "$queued_pid" || fail "cancellation did not release admission after the command exited"
forget_pid "$queued_pid"

# Check actual signal termination, not just the equivalent shell exit code.
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-signals" ORBIT_BUILD_SLOTS=1 \
  timeout 10 python3 - "$WRAPPER" "$TMP" <<'PY'
from pathlib import Path
import signal
import subprocess
import sys
import time

wrapper, scratch = sys.argv[1:]
signal.signal(signal.SIGTERM, signal.SIG_DFL)
for signum in (signal.SIGTERM, signal.SIGKILL):
    child = subprocess.run([wrapper, "--", sys.executable, "-c",
                            "import os,sys; os.kill(os.getpid(), int(sys.argv[1]))",
                            str(signum)])
    assert child.returncode == -signum, child.returncode

for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    # The test runner can inherit ignored signals; this case requires delivery.
    signal.signal(signum, signal.SIG_DFL)
    ready = Path(scratch) / f"signal-ready-{signum}"
    child = subprocess.Popen([wrapper, "--", sys.executable, "-c",
                              "import pathlib,signal,sys; "
                              "pathlib.Path(sys.argv[1]).touch(); signal.pause()",
                              str(ready)], stderr=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 2
        while not ready.exists():
            assert time.monotonic() < deadline, "signal command never started"
            time.sleep(0.01)
        child.send_signal(signum)
        assert child.wait(timeout=2) == -signum, child.returncode
    finally:
        if child.poll() is None:
            child.terminate()
        child.wait(timeout=2)
PY

# A held slot produces bounded diagnostics on stderr while the wrapped command
# remains queued. The private interval override keeps this process test short.
wait_state="$TMP/wait-reporting"
mkdir -p "$wait_state"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-wait-reporting" ORBIT_BUILD_SLOTS=1 \
  "$WRAPPER" -- "$TMP/helper.py" "$wait_state" owner block 0 &
owner_pid=$!
BACKGROUND_PIDS+=("$owner_pid")
wait_for "$wait_state/started-owner"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-wait-reporting" ORBIT_BUILD_SLOTS=1 \
  _ORBIT_BUILD_BUDGET_TEST_WAIT_INTERVAL_SECONDS=0.05 \
  "$WRAPPER" -- bash -c 'printf "wrapped stdout\\n"; exit 23' \
  >"$TMP/wait-command.out" 2>"$TMP/wait-command.err" &
queued_pid=$!
BACKGROUND_PIDS+=("$queued_pid")
sleep 0.18
[[ ! -s "$TMP/wait-command.out" ]] || fail "wait-reporting command ran before the owner released its slot"
[[ -e "$wait_state/active/owner" ]] || fail "wait-reporting owner lost its held slot"
grep -Fq "build-budget: waiting for admission (slots=1, budget_dir=$TMP/locks-wait-reporting)" \
  "$TMP/wait-command.err" || fail "wait-reporting start line omitted slot count or budget directory"
grep -Eq '^build-budget: still waiting for admission \(elapsed [0-9]+\.[0-9]s\)$' \
  "$TMP/wait-command.err" || fail "wait-reporting progress line did not include elapsed time"
progress_lines="$(grep -c '^build-budget: still waiting for admission ' "$TMP/wait-command.err")"
[[ "$progress_lines" -le 5 ]] || fail "wait-reporting emitted too many progress lines while queued"

# Both documented bypass paths execute immediately even while the only slot is held.
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-wait-reporting" ORBIT_BUILD_SLOTS=1 \
  ORBIT_BUILD_BUDGET_HELD=1 _ORBIT_BUILD_BUDGET_TEST_WAIT_INTERVAL_SECONDS=0.05 \
  timeout 2 "$WRAPPER" -- bash -c 'printf "reentry stdout\\n"' \
  >"$TMP/reentry.out" 2>"$TMP/reentry.err" \
  || fail "held-marker re-entry tried to acquire a second slot"
[[ "$(cat "$TMP/reentry.out")" == "reentry stdout" ]] || fail "held-marker re-entry changed stdout"
[[ ! -s "$TMP/reentry.err" ]] || fail "held-marker re-entry emitted admission diagnostics"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-wait-reporting" ORBIT_BUILD_SLOTS=1 ORBIT_BUILD_BUDGET=0 \
  _ORBIT_BUILD_BUDGET_TEST_WAIT_INTERVAL_SECONDS=0.05 \
  timeout 2 "$WRAPPER" -- bash -c 'printf "bypass stdout\\n"' \
  >"$TMP/bypass-held.out" 2>"$TMP/bypass-held.err" \
  || fail "budget bypass tried to acquire a slot"
[[ "$(cat "$TMP/bypass-held.out")" == "bypass stdout" ]] || fail "budget bypass changed stdout"
[[ ! -s "$TMP/bypass-held.err" ]] || fail "budget bypass emitted admission diagnostics"

kill -TERM "$owner_pid"
set +e
wait "$owner_pid" 2>/dev/null
owner_status=$?
wait "$queued_pid"
queued_status=$?
set -e
forget_pid "$owner_pid"
forget_pid "$queued_pid"
[[ "$owner_status" == "143" ]] || fail "wait-reporting owner returned $owner_status after termination"
[[ "$queued_status" == "23" ]] || fail "wait-reporting changed wrapped command exit status to $queued_status"
[[ "$(cat "$TMP/wait-command.out")" == "wrapped stdout" ]] || fail "wait-reporting changed wrapped command stdout"
grep -Eq '^build-budget: acquired slot 1 after [0-9]+\.[0-9]s$' \
  "$TMP/wait-command.err" || fail "wait-reporting did not report the acquired slot and total wait"
[[ "$(grep -c '^build-budget: waiting for admission ' "$TMP/wait-command.err")" == "1" ]] \
  || fail "wait-reporting did not emit exactly one start line"
[[ "$(grep -c '^build-budget: acquired slot ' "$TMP/wait-command.err")" == "1" ]] \
  || fail "wait-reporting did not emit exactly one acquired line"

# Immediate acquisition remains silent and preserves command output.
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-immediate" ORBIT_BUILD_SLOTS=1 \
  "$WRAPPER" -- bash -c 'printf "immediate stdout\\n"' \
  >"$TMP/immediate.out" 2>"$TMP/immediate.err"
[[ "$(cat "$TMP/immediate.out")" == "immediate stdout" ]] || fail "immediate acquisition changed stdout"
[[ ! -s "$TMP/immediate.err" ]] || fail "immediate acquisition emitted wait diagnostics"

# A nested admitted entry point must not try to acquire a second slot.
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-nested" ORBIT_BUILD_SLOTS=1 ORBIT_CARGO_JOBS=5 \
  "$WRAPPER" -- "$WRAPPER" -- "$TMP/helper.py" "$TMP/nested" nested block &
owner_pid=$!
BACKGROUND_PIDS+=("$owner_pid")
wait_for "$TMP/nested/started-nested"
grep -Fq 'jobs=5' "$TMP/nested/events" || fail "nested command lost Cargo job limit"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-nested" ORBIT_BUILD_SLOTS=1 \
  timeout 2 "$WRAPPER" -- "$TMP/helper.py" "$TMP/nested" queued sleep 0.01 &
queued_pid=$!
BACKGROUND_PIDS+=("$queued_pid")
sleep 0.08
[[ ! -e "$TMP/nested/started-queued" ]] || fail "nested command lost its parent's slot"
kill -TERM "$owner_pid"
set +e
wait "$owner_pid"
owner_status=$?
set -e
forget_pid "$owner_pid"
[[ "$owner_status" == "143" ]] || fail "nested command lost its termination status"
wait "$queued_pid" || fail "nested command did not release admission"
forget_pid "$queued_pid"

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
        app="${FAKE_APP:-}"
        if [[ -z "$app" && -n "${CARGO_TARGET_DIR:-}" ]]; then
          profile="debug"
          for arg in "$@"; do
            if [[ "$arg" == "--release" ]]; then
              profile="release"
              break
            fi
          done
          app="${CARGO_TARGET_DIR}/${profile}/orbit"
        fi
        python3 -c 'import json, sys; print(json.dumps({"reason":"compiler-artifact","executable":sys.argv[1],"target":{"name":"orbit"}}))' "$app"
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

# Makefile binary consumers (install, dev, web-memory-soak) honor redirected
# Cargo output, use the admitted build's reported artifact, and do not fall back
# to stale binaries in default target directories [ORB-15053].
REDIRECTED_TARGET="$TMP/custom-target"
REDIRECTED_RELEASE="$REDIRECTED_TARGET/release"
REDIRECTED_DEBUG="$REDIRECTED_TARGET/debug"
FAKE_INSTALL_BIN_DIR="$TMP/installed-bin"
FAKE_HOME="$TMP/fake-home"
mkdir -p "$REDIRECTED_RELEASE" "$REDIRECTED_DEBUG" "$FAKE_INSTALL_BIN_DIR" "$FAKE_HOME"
mkdir -p "$ROOT/target/release" "$ROOT/target/debug"

# Populate default targets with stale binaries.
cat >"$ROOT/target/release/orbit" <<'SH'
#!/usr/bin/env bash
printf 'stale-default-release-target\n'
SH
chmod +x "$ROOT/target/release/orbit"

cat >"$ROOT/target/debug/orbit" <<'SH'
#!/usr/bin/env bash
printf 'stale-default-debug-target\n'
SH
chmod +x "$ROOT/target/debug/orbit"

# Populate redirected targets with fresh binaries.
cat >"$REDIRECTED_RELEASE/orbit" <<'SH'
#!/usr/bin/env bash
printf 'fresh-custom-release-target\n'
SH
chmod +x "$REDIRECTED_RELEASE/orbit"

cat >"$REDIRECTED_DEBUG/orbit" <<'SH'
#!/usr/bin/env bash
printf 'fresh-custom-debug-target\n'
SH
chmod +x "$REDIRECTED_DEBUG/orbit"

# Ensure FAKE_APP is unset so fake-make-cargo resolves the binary under CARGO_TARGET_DIR.
unset FAKE_APP
FAKE_ARTIFACT_MODE=ok
FAKE_BUILD_EXIT=0
export FAKE_ARTIFACT_MODE FAKE_BUILD_EXIT

# 1. make install with redirected CARGO_TARGET_DIR installs the fresh release binary.
: >"$FAKE_MAKE_LOG"
: >"$FAKE_BUILD_ARGS"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-install" ORBIT_BUILD_SLOTS=1 \
  CARGO_TARGET_DIR="$REDIRECTED_TARGET" INSTALL_BIN_DIR="$FAKE_INSTALL_BIN_DIR" \
  HOME="$FAKE_HOME" timeout 5 \
  make -s -C "$ROOT" install CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER"
grep -Eq '^event=invoke cmd=build slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "make install build was not admitted"
grep -Fxq -- '-p' "$FAKE_BUILD_ARGS" && grep -Fxq -- 'orbit-cli' "$FAKE_BUILD_ARGS" \
  && grep -Fxq -- '--bin' "$FAKE_BUILD_ARGS" && grep -Fxq -- 'orbit' "$FAKE_BUILD_ARGS" \
  && grep -Fxq -- '--release' "$FAKE_BUILD_ARGS" \
  && grep -Fxq -- '--message-format=json-render-diagnostics' "$FAKE_BUILD_ARGS" \
  || fail "make install lost package, binary, release, or artifact-format arguments"
[[ -x "$FAKE_INSTALL_BIN_DIR/orbit" ]] || fail "make install did not create installed binary"
installed_out="$("$FAKE_INSTALL_BIN_DIR/orbit")"
[[ "$installed_out" == "fresh-custom-release-target" ]] \
  || fail "make install installed stale or wrong binary: got '$installed_out'"

# 1b. make install with INSTALL_PROFILE=debug installs the fresh debug binary.
rm -f "$FAKE_INSTALL_BIN_DIR/orbit"
: >"$FAKE_MAKE_LOG"
: >"$FAKE_BUILD_ARGS"
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-install-dbg" ORBIT_BUILD_SLOTS=1 \
  CARGO_TARGET_DIR="$REDIRECTED_TARGET" INSTALL_BIN_DIR="$FAKE_INSTALL_BIN_DIR" \
  HOME="$FAKE_HOME" timeout 5 \
  make -s -C "$ROOT" install INSTALL_PROFILE=debug CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER"
grep -Eq '^event=invoke cmd=build slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "make install debug build was not admitted"
[[ -x "$FAKE_INSTALL_BIN_DIR/orbit" ]] || fail "make install debug did not create installed binary"
installed_dbg_out="$("$FAKE_INSTALL_BIN_DIR/orbit")"
[[ "$installed_dbg_out" == "fresh-custom-debug-target" ]] \
  || fail "make install debug installed stale or wrong binary: got '$installed_dbg_out'"

# 2. make dev executes the redirected debug binary directly and releases build admission before running.
FAKE_DEV_ARGS="$TMP/make-dev-app-args"
FAKE_DEV_STARTED="$TMP/make-dev-app-started"
FAKE_DEV_RELEASE="$TMP/make-dev-app-release"
rm -f "$FAKE_DEV_STARTED" "$FAKE_DEV_RELEASE"
cat >"$REDIRECTED_DEBUG/orbit" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" >"${FAKE_DEV_ARGS}"
touch "${FAKE_DEV_STARTED}"
while [[ ! -e "${FAKE_DEV_RELEASE}" ]]; do
  sleep 0.02
done
printf 'fresh-custom-dev-out\n'
SH
chmod +x "$REDIRECTED_DEBUG/orbit"

: >"$FAKE_MAKE_LOG"
: >"$FAKE_BUILD_ARGS"
export FAKE_DEV_ARGS FAKE_DEV_STARTED FAKE_DEV_RELEASE
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-dev" ORBIT_BUILD_SLOTS=1 \
  CARGO_TARGET_DIR="$REDIRECTED_TARGET" \
  make -s -C "$ROOT" dev CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER" \
  ARGS='dev-arg1 dev-arg2' >"$TMP/make-dev.out" 2>"$TMP/make-dev.err" &
DEV_PID=$!
BACKGROUND_PIDS+=("$DEV_PID")
wait_for "$FAKE_DEV_STARTED"
grep -Eq '^event=invoke cmd=build slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "make dev build was not admitted"
grep -Fxq -- '-p' "$FAKE_BUILD_ARGS" && grep -Fxq -- 'orbit-cli' "$FAKE_BUILD_ARGS" \
  && grep -Fxq -- '--bin' "$FAKE_BUILD_ARGS" && grep -Fxq -- 'orbit' "$FAKE_BUILD_ARGS" \
  && grep -Fxq -- '--message-format=json-render-diagnostics' "$FAKE_BUILD_ARGS" \
  || fail "make dev lost package, binary, or artifact-format arguments"
printf 'dev-arg1\ndev-arg2\n' >"$TMP/expected-dev-args"
diff -q "$FAKE_DEV_ARGS" "$TMP/expected-dev-args" >/dev/null \
  || fail "make dev did not preserve application arguments"
# Build slot must be released before application runtime.
ORBIT_BUILD_BUDGET_DIR="$TMP/locks-dev" ORBIT_BUILD_SLOTS=1 \
  timeout 2 "$WRAPPER" -- true \
  || fail "make dev application runtime retained the build slot"
touch "$FAKE_DEV_RELEASE"
wait "$DEV_PID"
forget_pid "$DEV_PID"
grep -Fxq 'fresh-custom-dev-out' "$TMP/make-dev.out" \
  || fail "make dev did not execute fresh custom binary"

# 3. make web-memory-soak consumes the emitted binary rather than assuming target/release.
FAKE_SOAK_LOG="$TMP/soak-invoked.log"
FAKE_BIN_DIR="$TMP/fake-bin"
mkdir -p "$FAKE_BIN_DIR"
cat >"$FAKE_BIN_DIR/python3" <<'SH'
#!/usr/bin/env bash
if [[ "${1:-}" == "-c" ]]; then
  exec /usr/bin/python3 "$@"
fi
for arg in "$@"; do
  if [[ "$arg" == *"web-memory-soak"* ]]; then
    printf '%s\n' "$@" >"${FAKE_SOAK_LOG}"
    exit 0
  fi
done
exec /usr/bin/python3 "$@"
SH
chmod +x "$FAKE_BIN_DIR/python3"

# Restore standard release binary in redirected target.
cat >"$REDIRECTED_RELEASE/orbit" <<'SH'
#!/usr/bin/env bash
printf 'fresh-custom-release-target\n'
SH
chmod +x "$REDIRECTED_RELEASE/orbit"

: >"$FAKE_MAKE_LOG"
: >"$FAKE_BUILD_ARGS"
: >"$FAKE_SOAK_LOG"
export FAKE_SOAK_LOG
PATH="$FAKE_BIN_DIR:$PATH" ORBIT_BUILD_BUDGET_DIR="$TMP/locks-soak" ORBIT_BUILD_SLOTS=1 \
  CARGO_TARGET_DIR="$REDIRECTED_TARGET" \
  make -s -C "$ROOT" web-memory-soak CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER" \
  SOAK_FLAGS='--rounds 3'
grep -Eq '^event=invoke cmd=build slot=1 held=1$' "$FAKE_MAKE_LOG" \
  || fail "web-memory-soak build was not admitted"
grep -Fxq -- '--bin' "$FAKE_SOAK_LOG" || fail "web-memory-soak did not pass --bin argument"
grep -Fxq -- "$REDIRECTED_RELEASE/orbit" "$FAKE_SOAK_LOG" \
  || fail "web-memory-soak consumed stale or incorrect binary"
if grep -Fxq -- 'target/release/orbit' "$FAKE_SOAK_LOG"; then
  fail "web-memory-soak assumed default target/release/orbit"
fi
grep -Fxq -- '--rounds' "$FAKE_SOAK_LOG" && grep -Fxq -- '3' "$FAKE_SOAK_LOG" \
  || fail "web-memory-soak did not propagate SOAK_FLAGS"

# 4. Error reporting when cargo does not report an executable names the invoking make target.
for target_name in install dev web-memory-soak; do
  FAKE_ARTIFACT_MODE=none
  set +e
  CARGO_TARGET_DIR="$REDIRECTED_TARGET" INSTALL_BIN_DIR="$FAKE_INSTALL_BIN_DIR" \
    HOME="$FAKE_HOME" ORBIT_BUILD_BUDGET_DIR="$TMP/locks-err" ORBIT_BUILD_SLOTS=1 \
    PATH="$FAKE_BIN_DIR:$PATH" \
    make -s -C "$ROOT" "$target_name" CARGO="$TMP/fake-make-cargo" BUILD_BUDGET="$WRAPPER" \
    >"$TMP/make-$target_name-err.out" 2>"$TMP/make-$target_name-err.err"
  status=$?
  set -e
  [[ "$status" != 0 ]] || fail "make $target_name unexpectedly succeeded with missing executable"
  grep -Fq "make $target_name: cargo did not report an executable" "$TMP/make-$target_name-err.err" \
    || fail "make $target_name did not report target-specific missing executable error"
done

# Clean up temporary test binaries in ROOT/target.
rm -f "$ROOT/target/release/orbit" "$ROOT/target/debug/orbit"
rmdir "$ROOT/target/release" "$ROOT/target/debug" 2>/dev/null || true

TEST_COMPLETE=1
printf 'test-build-budget: ok\n'
