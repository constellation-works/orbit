#!/usr/bin/env bash
# Friction scan-memory benchmark [ORB-10680].
#
# Runs the ignored `bench_friction_scan_baseline_versus_candidate` harness in
# `orbit-store`. Both arms see the same generated corpus in the same process:
# the baseline replays the retired file scan (discover every record, parse
# every YAML envelope and body, collect, then filter/sort/paginate) and the
# candidate issues the same two requests — `orbit friction list --status open
# --limit 50 --json` and `orbit friction stats` — against SQLite.
#
# Reports wall time and peak RSS (`/proc/self/status` VmHWM) per arm. The
# candidate runs first so the baseline's high-water mark is attributable to the
# baseline rather than inherited from it.
#
# Usage:
#   scripts/bench-friction-scan.sh [corpus_size]
#
# Corpus size defaults to 5000 records and can also be set with
# ORBIT_FRICTION_BENCH_N. Release profile keeps the parse cost representative.
set -euo pipefail

CORPUS="${1:-${ORBIT_FRICTION_BENCH_N:-5000}}"
export ORBIT_FRICTION_BENCH_N="$CORPUS"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "friction scan benchmark: corpus=${CORPUS} (ORB-10680)"
output="$(cargo test --release -p orbit-store \
  --lib repository::friction::tests::bench::bench_friction_scan_baseline_versus_candidate \
  -- --exact --ignored --nocapture --test-threads=1 2>&1 | tee /dev/stderr)"

# A stale test path selects zero tests and still exits 0; refuse that.
if ! grep -q '^test result: ok\. 1 passed' <<<"$output"; then
  echo "bench-friction-scan: the benchmark test did not run" >&2
  exit 1
fi
