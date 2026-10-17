#!/usr/bin/env bash
# Report aggregate CPU utilisation across one command from /proc/stat.
#
# The Check job wraps its nextest phase with this so the log shows whether
# the runner's cores were busy while every nextest slot was taken. Busy time
# is user + nice + system + irq + softirq + steal. Idle and iowait stay
# separate: a wait-bound phase is idle, not saturated.
#
# CPU_UTIL_STAT_FILE overrides the stat path (tests substitute a fixture).
# A missing file, a counter that does not move, or a counter that steps
# backwards prints unavailable and does not change the command's exit status.

set -euo pipefail

label="command"
stat_file="${CPU_UTIL_STAT_FILE:-/proc/stat}"

usage() {
  echo "usage: cpu-utilisation.sh [--label NAME] -- COMMAND [ARG...]" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --label)
      [[ $# -ge 2 ]] || usage
      label="$2"
      shift 2
      ;;
    --)
      shift
      break
      ;;
    *)
      usage
      ;;
  esac
done

[[ $# -ge 1 ]] || usage
# One log token, so the report stays a single greppable line.
[[ "$label" =~ ^[A-Za-z0-9_-]+$ ]] || usage

# Eight aggregate counters plus the cpuN count. Empty output means unreadable.
read_snapshot() {
  awk '
    BEGIN { ok = 0; cores = 0 }
    NR == 1 {
      if ($1 != "cpu" || NF < 9) exit 1
      for (i = 2; i <= 9; i++) if ($i !~ /^[0-9]+$/) exit 1
      user_j = $2; nice_j = $3; sys_j = $4; idle_j = $5
      iowait_j = $6; irq_j = $7; softirq_j = $8; steal_j = $9
      ok = 1
    }
    $1 ~ /^cpu[0-9]+$/ { cores++ }
    END {
      if (!ok) exit 1
      printf "%s %s %s %s %s %s %s %s %d\n", user_j, nice_j, sys_j, idle_j, iowait_j, irq_j, softirq_j, steal_j, cores
    }
  ' "$1" 2>/dev/null
}

report_delta() {
  awk -v label="$label" -v elapsed="$elapsed" -v start="$1" -v end="$2" 'BEGIN {
    if (split(start, s, " ") != 9 || split(end, e, " ") != 9) {
      printf "cpu-util: label=%s unavailable=proc_stat elapsed_s=%s\n", label, elapsed
      exit 0
    }
    delta = 0
    decreased = 0
    for (i = 1; i <= 8; i++) {
      if ((e[i] + 0) < (s[i] + 0)) decreased = 1
      delta += (e[i] + 0) - (s[i] + 0)
    }
    cores = e[9]
    if (decreased) {
      printf "cpu-util: label=%s unavailable=decreased elapsed_s=%s cores=%s\n", label, elapsed, cores
      exit 0
    }
    if (delta <= 0) {
      printf "cpu-util: label=%s unavailable=unchanged elapsed_s=%s cores=%s\n", label, elapsed, cores
      exit 0
    }
    idle = (e[4] + 0) - (s[4] + 0)
    iowait = (e[5] + 0) - (s[5] + 0)
    busy = delta - idle - iowait
    printf "cpu-util: label=%s busy_pct=%.1f idle_pct=%.1f iowait_pct=%.1f elapsed_s=%s cores=%s\n", label, 100 * busy / delta, 100 * idle / delta, 100 * iowait / delta, elapsed, cores
  }'
}

if ! start_snapshot="$(read_snapshot "$stat_file")"; then
  start_snapshot=""
fi

started=$SECONDS
set +e
"$@"
status=$?
set -e
elapsed=$((SECONDS - started))

if ! end_snapshot="$(read_snapshot "$stat_file")"; then
  end_snapshot=""
fi

set +e
if [[ -z "$start_snapshot" || -z "$end_snapshot" ]]; then
  printf 'cpu-util: label=%s unavailable=proc_stat elapsed_s=%s\n' "$label" "$elapsed" >&2
else
  report_delta "$start_snapshot" "$end_snapshot" >&2
fi
set -e
exit "$status"
