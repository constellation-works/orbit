#!/usr/bin/env bash
# Bounded, private-target comparison of concurrent Cargo build admission [ORB-11754].
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="$ROOT/scripts/bench-build-budget.sh"
WRAPPER="$ROOT/scripts/build-budget.py"
PACKAGE="${ORBIT_BUILD_BUDGET_BENCH_PACKAGE:-orbit-types}"
PROCESSES="${ORBIT_BUILD_BUDGET_BENCH_PROCESSES:-4}"
SLOTS="${ORBIT_BUILD_BUDGET_BENCH_SLOTS:-2}"
JOBS="${ORBIT_BUILD_BUDGET_BENCH_JOBS:-2}"
WORKDIR="${ORBIT_BUILD_BUDGET_BENCH_DIR:-/tmp/orbit-build-budget-bench-$$}"
RESULT="${ORBIT_BUILD_BUDGET_BENCH_RESULT:-$WORKDIR/results.tsv}"
HEAD_SHA="$(git -C "$ROOT" rev-parse HEAD)"

cleanup() {
  rm -rf "$WORKDIR/trees" "$WORKDIR/targets" "$WORKDIR/locks"
}

if [[ "${1:-}" == "__arm" ]]; then
  arm="$2"
  pids=()
  for index in $(seq 1 "$PROCESSES"); do
    tree="$WORKDIR/trees/$index"
    target="$WORKDIR/targets/$arm-$index"
    if [[ "$arm" == "current" ]]; then
      (
        cd "$tree"
        CARGO_BUILD_JOBS="$JOBS" CARGO_TARGET_DIR="$target" CARGO_INCREMENTAL=0 \
          cargo check -p "$PACKAGE" --offline
      ) &
    else
      (
        cd "$tree"
        ORBIT_BUILD_BUDGET_DIR="$WORKDIR/locks" ORBIT_BUILD_SLOTS="$SLOTS" \
          ORBIT_CARGO_JOBS="$JOBS" CARGO_TARGET_DIR="$target" CARGO_INCREMENTAL=0 \
          "$WRAPPER" -- cargo check -p "$PACKAGE" --offline
      ) &
    fi
    pids+=("$!")
  done
  status=0
  for pid in "${pids[@]}"; do
    wait "$pid" || status=1
  done
  exit "$status"
fi

trap cleanup EXIT

mkdir -p "$WORKDIR/trees" "$WORKDIR/targets" "$WORKDIR/locks"
for index in $(seq 1 "$PROCESSES"); do
  mkdir -p "$WORKDIR/trees/$index"
  git -C "$ROOT" archive "$HEAD_SHA" | tar -x -C "$WORKDIR/trees/$index"
done

measure() {
  local arm="$1"
  local timing="$WORKDIR/$arm.time" output="$WORKDIR/$arm.log"
  local start end wall peak current
  start="$(date +%s.%N)"
  setsid /usr/bin/time -f '%U\t%S' -o "$timing" \
    env ORBIT_BUILD_BUDGET_BENCH_PACKAGE="$PACKAGE" \
    ORBIT_BUILD_BUDGET_BENCH_PROCESSES="$PROCESSES" \
    ORBIT_BUILD_BUDGET_BENCH_SLOTS="$SLOTS" \
    ORBIT_BUILD_BUDGET_BENCH_JOBS="$JOBS" \
    ORBIT_BUILD_BUDGET_BENCH_DIR="$WORKDIR" \
    "$SELF" __arm "$arm" >"$output" 2>&1 &
  local group_pid=$!
  peak=0
  while kill -0 "$group_pid" 2>/dev/null; do
    current="$(ps -eo pgid=,rss= | awk -v pgid="$group_pid" '$1 == pgid { total += $2 } END { print total + 0 }')"
    (( current > peak )) && peak="$current"
    sleep 0.05
  done
  wait "$group_pid"
  end="$(date +%s.%N)"
  wall="$(awk -v start="$start" -v end="$end" 'BEGIN { printf "%.3f", end - start }')"
  read -r user system <"$timing"
  printf '%s\t%s\t%s\t%s\t%s\n' "$arm" "$wall" "$user" "$system" "$peak" | tee -a "$RESULT"
}

{
  printf 'build-budget benchmark\n'
  printf 'head=%s\n' "$HEAD_SHA"
  printf 'rustc=%s\n' "$(rustc --version)"
  printf 'cargo=%s\n' "$(cargo --version)"
  printf 'workload=%s concurrent private targets; cargo check -p %s --offline; CARGO_INCREMENTAL=0\n' "$PROCESSES" "$PACKAGE"
  printf 'budget=%s slots x %s Cargo jobs; current arm has no admission but is safety-capped at %s Cargo jobs\n' "$SLOTS" "$JOBS" "$JOBS"
  printf 'arm\twall_s\tuser_cpu_s\tsystem_cpu_s\tpeak_sampled_rss_kib\n'
} | tee "$RESULT"

measure current
measure budgeted
printf 'results=%s\n' "$RESULT"
