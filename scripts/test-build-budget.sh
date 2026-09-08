#!/usr/bin/env bash
# Deterministic process tests for the cross-worktree build budget [ORB-11754].
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAPPER="$ROOT/scripts/build-budget.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() {
  printf 'test-build-budget: FAIL: %s\n' "$*" >&2
  exit 1
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

printf 'test-build-budget: ok\n'
