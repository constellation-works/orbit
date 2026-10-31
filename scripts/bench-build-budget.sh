#!/usr/bin/env bash
# Bounded, private-target comparison of concurrent Cargo build admission [ORB-11754].
set -euo pipefail
umask 077

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="$ROOT/scripts/bench-build-budget.sh"
WRAPPER="$ROOT/scripts/build-budget.py"
PACKAGE="${ORBIT_BUILD_BUDGET_BENCH_PACKAGE:-orbit-types}"
PROCESSES="${ORBIT_BUILD_BUDGET_BENCH_PROCESSES:-4}"
SLOTS="${ORBIT_BUILD_BUDGET_BENCH_SLOTS:-2}"
JOBS="${ORBIT_BUILD_BUDGET_BENCH_JOBS:-2}"

refuse() {
  printf 'bench-build-budget: refusing paths: %s\n' "$*" >&2
  return 1
}

# Validate a path relative to the admitted workdir and verify the ownership
# marker first. Python's lstat checks reject symlinks at every path component.
validate_owned_path() {
  python3 - "$WORKDIR" "$OWNER_TOKEN" "$1" "$2" <<'PY'
from pathlib import Path, PurePosixPath
import stat
import sys

root = Path(sys.argv[1])
token = sys.argv[2]
relative = sys.argv[3]
expected = sys.argv[4]


def refuse(message):
    print(f"bench-build-budget: refusing paths: {message}", file=sys.stderr)
    raise SystemExit(1)


try:
    if not token or "\n" in str(root) or "\r" in str(root):
        refuse("missing ownership token or invalid workdir")
    root_stat = root.lstat()
    if stat.S_ISLNK(root_stat.st_mode) or not stat.S_ISDIR(root_stat.st_mode):
        refuse(f"workdir is not an owned directory: {root}")
    if root.resolve(strict=True) != root:
        refuse(f"workdir path is aliased: {root}")
    marker = root / ".orbit-build-budget-owner"
    marker_stat = marker.lstat()
    if stat.S_ISLNK(marker_stat.st_mode) or not stat.S_ISREG(marker_stat.st_mode):
        refuse(f"ownership marker is not a regular file: {marker}")
    if marker.read_text(encoding="utf-8").rstrip("\n") != token:
        refuse(f"workdir is not owned by this invocation: {root}")

    if expected == "root":
        if relative:
            refuse("invalid root validation request")
        raise SystemExit(0)
    path_parts = PurePosixPath(relative).parts
    if (not relative or PurePosixPath(relative).is_absolute()
            or any(part in ("", ".", "..") for part in path_parts)):
        refuse(f"invalid workdir-relative path: {relative}")
    path = root
    for index, part in enumerate(path_parts):
        path = path / part
        final = index == len(path_parts) - 1
        try:
            item_stat = path.lstat()
        except FileNotFoundError:
            if final and expected == "absent":
                raise SystemExit(0)
            refuse(f"benchmark path is missing: {path}")
        if stat.S_ISLNK(item_stat.st_mode):
            refuse(f"benchmark path is a symlink: {path}")
        if not final and not stat.S_ISDIR(item_stat.st_mode):
            refuse(f"benchmark path parent is not a directory: {path}")
        if final:
            if expected == "absent":
                refuse(f"benchmark destination already exists: {path}")
            if expected == "dir" and not stat.S_ISDIR(item_stat.st_mode):
                refuse(f"benchmark destination is not a directory: {path}")
            if expected == "file" and not stat.S_ISREG(item_stat.st_mode):
                refuse(f"benchmark destination is not a regular file: {path}")
except (OSError, RuntimeError, UnicodeError) as error:
    refuse(str(error))
PY
}

if [[ "${1:-}" == "__arm" ]]; then
  if [[ "$#" != 2 || ( "$2" != "current" && "$2" != "budgeted" ) ]]; then
    refuse "invalid internal benchmark arm"
    exit 2
  fi
  WORKDIR="${ORBIT_BUILD_BUDGET_BENCH_DIR:-}"
  RESULT="${ORBIT_BUILD_BUDGET_BENCH_RESULT:-}"
  OWNER_TOKEN="${ORBIT_BUILD_BUDGET_BENCH_OWNER:-}"
  [[ -n "$WORKDIR" && -n "$RESULT" && -n "$OWNER_TOKEN" ]] || {
    refuse "internal arm requires an owned benchmark workdir"
    exit 2
  }
  validate_owned_path "" root
  validate_owned_path trees dir
  validate_owned_path targets dir
  validate_owned_path locks dir
  [[ "$(dirname "$RESULT")" == "$WORKDIR" ]] || {
    refuse "result must be a direct child of the benchmark workdir"
    exit 2
  }
  validate_owned_path "$(basename "$RESULT")" file

  arm="$2"
  pids=()
  for index in $(seq 1 "$PROCESSES"); do
    validate_owned_path "trees/$index" dir
    validate_owned_path "targets/$arm-$index" absent
  done
  for index in $(seq 1 "$PROCESSES"); do
    target="$WORKDIR/targets/$arm-$index"
    mkdir -- "$target"
    validate_owned_path "targets/$arm-$index" dir
  done
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

if [[ "${1:-}" != "" ]]; then
  refuse "unexpected argument: $1"
  exit 2
fi

RAW_WORKDIR="${ORBIT_BUILD_BUDGET_BENCH_DIR:-}"
RAW_RESULT="${ORBIT_BUILD_BUDGET_BENCH_RESULT:-}"
if [[ -z "$RAW_WORKDIR" ]]; then
  DEFAULT_WORKDIR=1
else
  DEFAULT_WORKDIR=0
fi

# A caller-selected workdir is a one-run destination. os.mkdir is the
# collision boundary: concurrent invocations cannot both claim the same root.
# The default instead uses mkdtemp, which acquires a unique directory atomically.
admission="$(python3 - "$RAW_WORKDIR" "$RAW_RESULT" "$DEFAULT_WORKDIR" <<'PY'
from pathlib import Path
import os
import secrets
import sys
import tempfile

raw_work, raw_result, use_default = sys.argv[1:]


def refuse(message):
    print(f"bench-build-budget: refusing paths: {message}", file=sys.stderr)
    raise SystemExit(2)


try:
    if any("\n" in value or "\r" in value for value in (raw_work, raw_result)):
        refuse("paths must not contain newlines")
    reserved = {
        "", ".", "..", ".orbit-build-budget-owner", "trees", "targets", "locks",
        "current.time", "current.log", "budgeted.time", "budgeted.log",
    }
    if use_default == "1":
        result_name = Path(raw_result).expanduser() if raw_result else Path("results.tsv")
        if result_name.is_absolute() or len(result_name.parts) != 1:
            refuse("with the default workdir, result override must be a filename")
        if result_name.name in reserved:
            refuse(f"result collides with a reserved benchmark path: {result_name}")
        work = Path(tempfile.mkdtemp(prefix="orbit-build-budget-bench-", dir="/tmp"))
        work = work.resolve(strict=True)
        result = work / result_name.name
    else:
        selected = Path(raw_work).expanduser()
        if selected.is_symlink() or selected.exists():
            refuse(f"benchmark workdir already exists or is a symlink: {selected}")
        work = selected.resolve(strict=False)
        if work == Path(work.anchor):
            refuse("filesystem root cannot be the benchmark workdir")
        if raw_result:
            selected_result = Path(raw_result).expanduser()
            if not selected_result.is_absolute():
                selected_result = work / selected_result
            if selected_result.is_symlink():
                refuse(f"result destination is a symlink: {selected_result}")
            result = selected_result.resolve(strict=False)
        else:
            result = work / "results.tsv"
        if result.parent != work:
            refuse("result must be a direct child of the benchmark workdir")
        if result.name in reserved:
            refuse(f"result collides with a reserved benchmark path: {result}")
        if result.exists() or result.is_symlink():
            refuse(f"result destination already exists: {result}")
        try:
            os.mkdir(work, 0o700)
        except FileExistsError:
            refuse(f"benchmark workdir was claimed by another invocation: {work}")
    if work == Path(work.anchor):
        refuse("filesystem root cannot be the benchmark workdir")
    if "\n" in str(work) or "\r" in str(work):
        refuse("resolved workdir must not contain newlines")

    if result.parent != work:
        refuse("result must be a direct child of the benchmark workdir")
    if result.name in reserved:
        refuse(f"result collides with a reserved benchmark path: {result}")
    if result.exists() or result.is_symlink():
        refuse(f"result destination already exists: {result}")
    token = secrets.token_hex(32)
    marker = work / ".orbit-build-budget-owner"
    try:
        fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as output:
            output.write(token + "\n")
            output.flush()
            os.fsync(output.fileno())
    except OSError:
        try:
            os.rmdir(work)
        except OSError:
            pass
        raise
    print(work)
    print(result)
    print(token)
except (OSError, RuntimeError, ValueError) as error:
    print(f"bench-build-budget: refusing paths: {error}", file=sys.stderr)
    raise SystemExit(2)
PY
)"
{
  IFS= read -r WORKDIR
  IFS= read -r RESULT
  IFS= read -r OWNER_TOKEN
} <<<"$admission"
[[ -n "$WORKDIR" && -n "$RESULT" && -n "$OWNER_TOKEN" ]] || {
  refuse "could not acquire benchmark workdir"
  exit 2
}
OWNED_WORKDIR=1
ARM_PGID=""
ARM_SPAWNING=0
PENDING_CANCEL=""

# Stop this invocation's isolated arm (its own session/process group, led by the
# pid measure() recorded) and join it, so nothing still runs in the trees,
# targets or locks that cleanup is about to remove.
stop_arm() {
  local pgid="$ARM_PGID" waited
  [[ -n "$pgid" ]] || return 0
  ARM_PGID=""
  kill -TERM -- "-$pgid" 2>/dev/null || true
  for waited in $(seq 1 100); do
    kill -0 -- "-$pgid" 2>/dev/null || break
    sleep 0.05
  done
  kill -KILL -- "-$pgid" 2>/dev/null || true
  wait "$pgid" 2>/dev/null || true
  for waited in $(seq 1 100); do
    kill -0 -- "-$pgid" 2>/dev/null || break
    sleep 0.05
  done
}

cancel() {
  if [[ "${ARM_SPAWNING:-0}" == 1 ]]; then
    if [[ -z "${PENDING_CANCEL:-}" ]]; then
      PENDING_CANCEL="$1"
    fi
    return 0
  fi
  trap '' HUP INT TERM
  exit "$1"
}
trap 'cancel 129' HUP
trap 'cancel 130' INT
trap 'cancel 143' TERM

cleanup() {
  local status=$?
  trap - EXIT
  trap '' HUP INT TERM
  stop_arm
  if [[ "$OWNED_WORKDIR" == 1 ]] && validate_owned_path "" root; then
    for path in trees targets locks; do
      if validate_owned_path "$path" dir; then
        rm -rf -- "$WORKDIR/$path" || status=1
      elif [[ -e "$WORKDIR/$path" || -L "$WORKDIR/$path" ]]; then
        printf 'bench-build-budget: preserving unverified cleanup path: %s\n' "$WORKDIR/$path" >&2
        status=1
      fi
    done
  fi
  return "$status"
}
trap cleanup EXIT

validate_owned_path "" root
mkdir -- "$WORKDIR/trees" "$WORKDIR/targets" "$WORKDIR/locks"
validate_owned_path trees dir
validate_owned_path targets dir
validate_owned_path locks dir
HEAD_SHA="$(git -C "$ROOT" rev-parse HEAD)"

for index in $(seq 1 "$PROCESSES"); do
  tree="$WORKDIR/trees/$index"
  validate_owned_path "trees/$index" absent
  mkdir -- "$tree"
  validate_owned_path "trees/$index" dir
  git -C "$ROOT" archive "$HEAD_SHA" | tar -x -C "$tree"
done

validate_owned_path "$(basename "$RESULT")" absent
set -o noclobber
: >"$RESULT"
set +o noclobber

measure() {
  local arm="$1"
  local timing="$WORKDIR/$arm.time" output="$WORKDIR/$arm.log"
  local start end wall peak current
  validate_owned_path "$arm.time" absent
  validate_owned_path "$arm.log" absent
  validate_owned_path "$(basename "$RESULT")" file
  start="$(date +%s.%N)"
  PENDING_CANCEL="" ARM_SPAWNING=1
  setsid /usr/bin/time -f '%U\t%S' -o "$timing" \
    env ORBIT_BUILD_BUDGET_BENCH_PACKAGE="$PACKAGE" \
    ORBIT_BUILD_BUDGET_BENCH_PROCESSES="$PROCESSES" \
    ORBIT_BUILD_BUDGET_BENCH_SLOTS="$SLOTS" \
    ORBIT_BUILD_BUDGET_BENCH_JOBS="$JOBS" \
    ORBIT_BUILD_BUDGET_BENCH_DIR="$WORKDIR" \
    ORBIT_BUILD_BUDGET_BENCH_RESULT="$RESULT" \
    ORBIT_BUILD_BUDGET_BENCH_OWNER="$OWNER_TOKEN" \
    "$SELF" __arm "$arm" >"$output" 2>&1 &
  local group_pid=$!
  ARM_PGID="$group_pid"
  ARM_SPAWNING=0
  if [[ -n "$PENDING_CANCEL" ]]; then
    cancel "$PENDING_CANCEL"
  fi
  peak=0
  while kill -0 "$group_pid" 2>/dev/null; do
    current="$(ps -eo pgid=,rss= | awk -v pgid="$group_pid" '$1 == pgid { total += $2 } END { print total + 0 }')"
    (( current > peak )) && peak="$current"
    sleep 0.05
  done
  local arm_status=0
  wait "$group_pid" || arm_status=$?
  ARM_PGID=""
  (( arm_status == 0 )) || return "$arm_status"
  end="$(date +%s.%N)"
  wall="$(awk -v start="$start" -v end="$end" 'BEGIN { printf "%.3f", end - start }')"
  read -r user system <"$timing"
  validate_owned_path "$(basename "$RESULT")" file
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
} | tee -a "$RESULT"

measure current
measure budgeted
printf 'results=%s\n' "$RESULT"
