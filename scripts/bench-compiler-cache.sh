#!/usr/bin/env bash
# Isolated compiler-cache benchmark [ORB-11259] [ORB-11755].
#
# Records source revision, toolchain, workload, wall/CPU time, and sccache
# hits/misses/non-cacheable reasons for: no-cache baseline, cache cold fill,
# same-path cold/warm builds, a different-worktree miss control, Clippy,
# incremental, and concurrent
# different-source Linux mount namespaces when Bubblewrap can create them.
#
# Uses private CARGO_TARGET_DIR values per tree. The same-path pair empties
# and reuses one private target sequentially to keep cache keys identical.
# The disk cache and UDS live
# only under the bench workdir this invocation exclusively claims. Python 3
# resolves paths and claims the workdir before any writes; the cache must be
# a strict descendant, separate from generated trees, targets and outputs, so
# resets cannot remove an operator cache or the active benchmark tree. Does
# not enable a global host cache.
set -euo pipefail
umask 077

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PACKAGE="${ORBIT_COMPILER_CACHE_BENCH_PACKAGE:-orbit-types}"
CARGO_CMD=(cargo check -p "$PACKAGE" --offline --locked)
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
WORKDIR="${ORBIT_COMPILER_CACHE_BENCH_DIR:-/tmp/orbit-compiler-cache-bench-$$}"
CACHE_DIR="${SCCACHE_DIR:-$WORKDIR/cache}"
RESULT="${ORBIT_COMPILER_CACHE_BENCH_RESULT:-$WORKDIR/results-$STAMP.txt}"
JOBS="${CARGO_BUILD_JOBS:-1}"
REQUIRE_NS="${ORBIT_COMPILER_CACHE_REQUIRE_NS:-0}"
SCCACHE_VERSION="v0.17.0"
SCCACHE_BIN=""

# Resolve existing symlink ancestors and missing tails together, before even
# installing the cleanup trap. Use these canonical paths for every consumer,
# including daemon controls. The workdir must not be modified by other processes
# during the benchmark; this is path admission, not a filesystem sandbox.
validated_paths="$(python3 - "$WORKDIR" "$CACHE_DIR" "$RESULT" <<'PYPATHS'
from pathlib import Path
import os
import secrets
import sys


def refuse(message):
    raise ValueError(message)


def overlaps(left, right):
    return left == right or left in right.parents or right in left.parents


try:
    if any("\n" in path or "\r" in path for path in sys.argv[1:]):
        refuse("paths must not contain newlines")
    work, cache, result = (Path(path).resolve() for path in sys.argv[1:])
    if any("\n" in str(path) or "\r" in str(path) for path in (work, cache, result)):
        refuse("resolved paths must not contain newlines")
    if work == Path(work.anchor):
        refuse("the filesystem root cannot be the benchmark workdir")
    if work not in cache.parents:
        refuse("SCCACHE_DIR must resolve strictly below the benchmark workdir")
    if work not in result.parents or Path(sys.argv[3]).is_symlink():
        refuse("the result must be a regular output below the benchmark workdir")
    # Every fixed destination used by copies, target resets, installation,
    # daemon controls and cleanup. Reject aliases before any side effects.
    directories = (
        "a", "b", "a-target", "b-target", "warm-target", "real-custom-a",
        "real-custom-b", "probe", "probe-target", "probe-c", "probe-d",
        "probe-c-target", "probe-d-target", "bin",
    )
    files = ("sccache.sock", "last-elapsed", "bwrap-probe.err",
             ".orbit-compiler-cache-bench-owner")
    reserved = [work / name for name in directories + files]
    for path in reserved + [work / "bin" / "sccache"]:
        if path.is_symlink():
            refuse(f"benchmark destination is a symlink: {path}")
    for name in directories:
        path = work / name
        if path.exists() and not path.is_dir():
            refuse(f"benchmark directory is not a directory: {path}")
    for path in reserved:
        if overlaps(cache, path) or overlaps(result, path):
            refuse(f"cache/result overlaps a reserved benchmark destination: {path}")
    if overlaps(cache, result):
        refuse("cache and result must be separate")
    for path in (work, cache):
        if path.exists() and not path.is_dir():
            refuse(f"not a directory: {path}")
    if result.exists() and not result.is_file():
        refuse(f"not a regular result file: {result}")
    # Establish exclusive ownership before the caller installs cleanup,
    # writes the result/cache, copies trees or invokes any transport. A new
    # workdir is claimed with an atomic mkdir; a pre-existing workdir may be
    # claimed only while it holds no file at all (empty subdirectories and
    # internal symlinks, as in private cache layouts, are fine). Either way
    # an O_EXCL owner marker is the collision boundary a second invocation
    # cannot cross, and every destructive consumer re-verifies it.
    if Path(sys.argv[1]).is_symlink():
        refuse(f"benchmark workdir is a symlink: {sys.argv[1]}")
    marker = work / ".orbit-compiler-cache-bench-owner"
    if overlaps(cache, marker) or overlaps(result, marker):
        refuse(f"cache/result overlaps the ownership marker: {marker}")
    token = secrets.token_hex(32)
    created_workdir = False
    if not work.exists():
        work.parent.mkdir(parents=True, exist_ok=True)
        try:
            os.mkdir(work, 0o700)
            created_workdir = True
        except FileExistsError:
            refuse(f"benchmark workdir was claimed by another invocation: {work}")
    else:
        stack = [work]
        while stack:
            current = stack.pop()
            with os.scandir(current) as entries:
                for entry in entries:
                    if entry.is_symlink():
                        target = Path(entry.path).resolve()
                        if target != work and work not in target.parents:
                            refuse("benchmark workdir contains a symlink "
                                   f"leaving the workdir: {entry.path}")
                        continue
                    if entry.is_dir(follow_symlinks=False):
                        stack.append(Path(entry.path))
                        continue
                    refuse(f"benchmark workdir already contains a file: {entry.path}")
    try:
        fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as output:
            output.write(token + "\n")
            output.flush()
            os.fsync(output.fileno())
    except FileExistsError:
        refuse(f"benchmark workdir is already claimed: {work}")
    except OSError:
        if created_workdir:
            try:
                os.rmdir(work)
            except OSError:
                pass
        raise
    print(work)
    print(cache)
    print(result)
    print(token)
except (OSError, RuntimeError, ValueError) as error:
    print(f"bench-compiler-cache: refusing paths: {error}", file=sys.stderr)
    sys.exit(1)
PYPATHS
)"
{
  IFS= read -r WORKDIR
  IFS= read -r CACHE_DIR
  IFS= read -r RESULT
  IFS= read -r OWNER_TOKEN
} <<<"$validated_paths"
[[ -n "$WORKDIR" && -n "$CACHE_DIR" && -n "$RESULT" && -n "$OWNER_TOKEN" ]] || {
  printf 'bench-compiler-cache: refusing paths: could not claim benchmark workdir\n' >&2
  exit 1
}
BENCH_UDS="$WORKDIR/sccache.sock"
HEAD_SHA="$(git rev-parse HEAD 2>/dev/null || printf 'unknown')"

# Every destructive or writing consumer re-verifies the claim made during
# admission, so a marker removed or replaced after admission preserves the
# trees instead of deleting through an unowned workdir.
verify_bench_ownership() {
  python3 - "$WORKDIR" "$OWNER_TOKEN" <<'PYOWN'
from pathlib import Path
import stat
import sys

work, token = Path(sys.argv[1]), sys.argv[2]
try:
    if not token or "\n" in token or "\r" in token:
        raise ValueError("missing ownership token")
    work_stat = work.lstat()
    if stat.S_ISLNK(work_stat.st_mode) or not stat.S_ISDIR(work_stat.st_mode):
        raise ValueError(f"workdir is not an owned directory: {work}")
    if work.resolve(strict=True) != work:
        raise ValueError(f"workdir path is aliased: {work}")
    marker = work / ".orbit-compiler-cache-bench-owner"
    marker_stat = marker.lstat()
    if stat.S_ISLNK(marker_stat.st_mode) or not stat.S_ISREG(marker_stat.st_mode):
        raise ValueError(f"ownership marker is not a regular file: {marker}")
    if marker.read_text(encoding="utf-8").rstrip("\n") != token:
        raise ValueError(f"workdir is not owned by this invocation: {work}")
except (OSError, RuntimeError, UnicodeError, ValueError) as error:
    print(f"bench-compiler-cache: refusing paths: {error}", file=sys.stderr)
    raise SystemExit(1)
PYOWN
}

require_bench_ownership() {
  verify_bench_ownership || exit 1
}

stop_owned_daemon() {
  [[ -n "$SCCACHE_BIN" && -x "$SCCACHE_BIN" ]] || return 0
  SCCACHE_DIR="$CACHE_DIR" SCCACHE_SERVER_UDS="$BENCH_UDS" \
    "$SCCACHE_BIN" --stop-server >/dev/null 2>&1 || true
}

cleanup() {
  local status=$?
  trap - EXIT
  trap '' HUP INT TERM
  if verify_bench_ownership; then
    stop_owned_daemon
    rm -rf "$WORKDIR/a" "$WORKDIR/b" "$WORKDIR/a-target" "$WORKDIR/b-target" \
      "$WORKDIR/warm-target" \
      "$WORKDIR/probe-c" "$WORKDIR/probe-d" "$WORKDIR/probe-c-target" "$WORKDIR/probe-d-target" \
      || status=1
  else
    printf 'bench-compiler-cache: preserving unverified cleanup paths under: %s\n' "$WORKDIR" >&2
    if [[ "$status" == 0 ]]; then
      status=1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

require_bench_ownership
mkdir -p "$WORKDIR" "$CACHE_DIR" "$(dirname "$RESULT")"
require_bench_ownership
set -o noclobber
: >"$RESULT"
set +o noclobber

# Remove the directory entry itself, never glob through a preexisting target
# or source tree. rm does not follow symlinks within it. All callers use fixed
# direct children of the canonical workdir claimed above, and each reset
# re-verifies that claim before deleting.
reset_bench_directory() {
  require_bench_ownership
  rm -rf -- "$1"
  mkdir -p "$1"
}

reset_cache_directory() {
  require_bench_ownership
  rm -rf -- "$CACHE_DIR"
  mkdir -p "$CACHE_DIR"
}

log() {
  printf '%s\n' "$*" | tee -a "$RESULT"
}

secs() {
  local start="$1" end="$2"
  awk -v s="$start" -v e="$end" 'BEGIN { printf "%.3f", e - s }'
}

now() {
  date +%s.%N
}

resolve_sccache() {
  if [[ -n "${ORBIT_COMPILER_CACHE_BIN:-}" && -x "${ORBIT_COMPILER_CACHE_BIN}" ]]; then
    printf '%s\n' "${ORBIT_COMPILER_CACHE_BIN}"
    return 0
  fi
  if [[ -x "$WORKDIR/bin/sccache" ]]; then
    printf '%s\n' "$WORKDIR/bin/sccache"
    return 0
  fi
  if [[ -x "${HOME:-}/.orbit/cache/bin/sccache" ]]; then
    printf '%s\n' "$HOME/.orbit/cache/bin/sccache"
    return 0
  fi
  if command -v sccache >/dev/null 2>&1; then
    command -v sccache
    return 0
  fi
  return 1
}

install_isolated_sccache() {
  local os arch asset url tmp dest checksum actual
  require_bench_ownership
  os="$(uname -s)"
  arch="$(uname -m)"
  # SHA-256 digests from the v0.17.0 release assets; keep in step with
  # scripts/compiler-cache.sh sccache_asset().
  case "${os}/${arch}" in
    Linux/x86_64 | Linux/amd64)
      asset="sccache-${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz"
      checksum="67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006"
      ;;
    Linux/aarch64 | Linux/arm64)
      asset="sccache-${SCCACHE_VERSION}-aarch64-unknown-linux-musl.tar.gz"
      checksum="821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77"
      ;;
    Darwin/x86_64 | Darwin/amd64)
      asset="sccache-${SCCACHE_VERSION}-x86_64-apple-darwin.tar.gz"
      checksum="c2144cafbfe3d22e34ae637f9974ce53613543ac19477fdb287df22ea3668261"
      ;;
    Darwin/arm64 | Darwin/aarch64)
      asset="sccache-${SCCACHE_VERSION}-aarch64-apple-darwin.tar.gz"
      checksum="0c560bfba31aef5bdfb4fb3d2677f6e61d71c5c00952f2a83344f47aa31f00f1"
      ;;
    *)
      return 1
      ;;
  esac
  dest="$WORKDIR/bin/sccache"
  mkdir -p "$WORKDIR/bin"
  url="https://github.com/mozilla/sccache/releases/download/${SCCACHE_VERSION}/${asset}"
  tmp="$(mktemp -d "$WORKDIR/download.XXXXXX")"
  log "downloading isolated $url"
  if ! curl -fsSL "$url" -o "$tmp/$asset"; then
    rm -rf "$tmp"
    return 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$asset")"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$asset")"
  else
    log "bench-compiler-cache: SHA-256 verification requires sha256sum or shasum"
    rm -rf "$tmp"
    return 1
  fi
  if [[ "${actual%% *}" != "$checksum" ]]; then
    log "bench-compiler-cache: SHA-256 mismatch for $asset"
    rm -rf "$tmp"
    return 1
  fi
  tar -xzf "$tmp/$asset" -C "$tmp"
  find "$tmp" -type f -name sccache -exec cp {} "$dest" \;
  chmod +x "$dest"
  rm -rf "$tmp"
  [[ -x "$dest" ]]
}

time_cmd() {
  local label="$1"
  shift
  local start end elapsed timefile status
  timefile="$(mktemp "$WORKDIR/time-$label.XXXXXX")"
  start="$(now)"
  status=0
  # GNU time supports -f/-o; BSD time (macOS /usr/bin/time) does not.
  if [[ -x /usr/bin/time ]] && /usr/bin/time -f '%e' -o /dev/null true 2>/dev/null; then
    /usr/bin/time -f 'wall=%e user=%U sys=%S' -o "$timefile" "$@" || status=$?
  else
    "$@" || status=$?
    printf 'wall=unknown user=unknown sys=unknown (no GNU /usr/bin/time)\n' >"$timefile"
  fi
  end="$(now)"
  elapsed="$(secs "$start" "$end")"
  log "    wall_s=$elapsed  $(tr '\n' ' ' <"$timefile")"
  printf '%s\n' "$elapsed" >"$WORKDIR/last-elapsed"
  return "$status"
}

run_build() {
  local label="$1" tree="$2" target="$3"
  shift 3
  reset_bench_directory "$target"
  log "==> $label  (tree=$tree target=$target cwd=$(pwd))"
  (
    cd "$tree"
    time_cmd "$label" env "$@" \
      CARGO_TARGET_DIR="$target" \
      CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}" \
      CARGO_BUILD_JOBS="$JOBS" \
      "${CARGO_CMD[@]}"
  )
}

sccache_stats() {
  local bin="${SCCACHE_BIN:-}"
  [[ -n "$bin" && -x "$bin" ]] || return 0
  log "--- sccache --show-stats (uds=$BENCH_UDS) ---"
  SCCACHE_DIR="$CACHE_DIR" SCCACHE_SERVER_UDS="$BENCH_UDS" \
    SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-5G}" \
    "$bin" --show-stats 2>&1 | tee -a "$RESULT" || true
}

can_create_ns() {
  command -v bwrap >/dev/null 2>&1 || return 1
  bwrap --die-with-parent --unshare-all --share-net --ro-bind / / --tmpfs /tmp \
    --dev /dev --proc /proc -- /bin/true >/dev/null 2>"$WORKDIR/bwrap-probe.err"
}

copy_tree() {
  local dest="$1"
  reset_bench_directory "$dest"
  if git archive "$HEAD_SHA" >/dev/null 2>&1; then
    git archive "$HEAD_SHA" | tar -x -C "$dest"
  else
    tar -C "$ROOT" --exclude target --exclude .git --exclude tmp -cf - . \
      | tar -C "$dest" -xf -
  fi
  mkdir -p "$dest/.cargo" "$dest/scripts"
  cp "$ROOT/.cargo/config.toml" "$dest/.cargo/config.toml"
  cp "$ROOT/scripts/rustc-compiler-cache.sh" "$dest/scripts/rustc-compiler-cache.sh"
  chmod +x "$dest/scripts/rustc-compiler-cache.sh"
}

log "compiler-cache bench $STAMP"
log "head=$HEAD_SHA"
log "rustc=$(rustc --version 2>/dev/null || echo missing)"
log "cargo=$(cargo --version 2>/dev/null || echo missing)"
log "host=$(uname -srm)"
log "package=$PACKAGE"
log "cmd=${CARGO_CMD[*]}"
log "jobs=$JOBS"
log "workdir=$WORKDIR"
log "cache_dir=$CACHE_DIR"
log "note=/usr/bin/time user/sys is the cargo process tree; it excludes a detached sccache daemon. Wrapper defaults (SCCACHE_CLIENT_SIDE=1) keep compiler CPU in the client tree."

copy_tree "$WORKDIR/a"
copy_tree "$WORKDIR/b"

log ""
log "### baseline (ORBIT_COMPILER_CACHE=0, sequential cold, custom CARGO_TARGET_DIR)"
run_build "baseline-a-cold" "$WORKDIR/a" "$WORKDIR/a-target" ORBIT_COMPILER_CACHE=0
base_a="$(cat "$WORKDIR/last-elapsed")"
run_build "baseline-b-cold" "$WORKDIR/b" "$WORKDIR/b-target" ORBIT_COMPILER_CACHE=0
base_b="$(cat "$WORKDIR/last-elapsed")"

SCCACHE_BIN=""
if ! SCCACHE_BIN="$(resolve_sccache)"; then
  if install_isolated_sccache; then
    SCCACHE_BIN="$WORKDIR/bin/sccache"
  fi
fi

if [[ -z "$SCCACHE_BIN" || ! -x "$SCCACHE_BIN" ]]; then
  log ""
  log "sccache is not available; cache arms skipped."
  log "Host setup: curl the pinned $SCCACHE_VERSION musl binary into a private dir and set ORBIT_COMPILER_CACHE_BIN. Do not write ~/.orbit/cache from a managed worker."
  log "baseline_a_s=$base_a"
  log "baseline_b_s=$base_b"
  log "results=$RESULT"
  exit 0
fi

log "sccache_bin=$SCCACHE_BIN"
log "sccache_version=$(SCCACHE_DIR="$CACHE_DIR" SCCACHE_SERVER_UDS="$BENCH_UDS" "$SCCACHE_BIN" --version 2>/dev/null || echo unknown)"
log "sccache_uds=$BENCH_UDS"
CACHE_ENV=(
  SCCACHE_DIR="$CACHE_DIR"
  SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-5G}"
  ORBIT_COMPILER_CACHE_BIN="$SCCACHE_BIN"
  SCCACHE_SERVER_UDS="$BENCH_UDS"
  SCCACHE_CLIENT_SIDE=1
)

log ""
log "### unnormalized copy-tree baseline (no stable mounts; unique cwd/paths expected to miss)"
reset_cache_directory
sccache_stats
run_build "unnormalized-a-cold" "$WORKDIR/a" "$WORKDIR/a-target" "${CACHE_ENV[@]}"
unnormalized_a="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
run_build "unnormalized-b-second" "$WORKDIR/b" "$WORKDIR/b-target" "${CACHE_ENV[@]}"
unnormalized_b="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
log "unnormalized_note=these copies are not bind-mounted at /tmp/orbit-workspace; treat as a miss-path control, not cross-worktree reuse."

log ""
log "### same-path cache cold fill and warm rebuild (identical cwd, argv and CARGO_* environment)"
log "Fresh build output on both arms; the private target path is reused sequentially, never shared between workers. This measures dependency reuse, not cross-worktree normalization."
stop_owned_daemon
reset_cache_directory
sccache_stats
run_build "same-path-cold" "$WORKDIR/a" "$WORKDIR/warm-target" "${CACHE_ENV[@]}"
cache_a="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
SCCACHE_DIR="$CACHE_DIR" SCCACHE_SERVER_UDS="$BENCH_UDS" \
  "$SCCACHE_BIN" --zero-stats >/dev/null
run_build "same-path-warm" "$WORKDIR/a" "$WORKDIR/warm-target" "${CACHE_ENV[@]}"
cache_b="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
log "same_path_note=stats were zeroed before the warm rebuild; warm hits come from the preceding cold fill, not retained Cargo artifacts."

real_custom_cold=""
real_custom_second=""
if [[ -e /tmp/orbit-workspace/Cargo.toml && -e "$ROOT/Cargo.toml" ]] \
  && [[ "$(stat -L -c '%d:%i' /tmp/orbit-workspace/Cargo.toml 2>/dev/null || stat -L -f '%d:%i' /tmp/orbit-workspace/Cargo.toml)" \
    == "$(stat -L -c '%d:%i' "$ROOT/Cargo.toml" 2>/dev/null || stat -L -f '%d:%i' "$ROOT/Cargo.toml")" ]]; then
  log ""
  log "### real Linux stable source mount + custom CARGO_TARGET_DIR (same checkout, two private targets)"
  log "Demonstrates cwd normalization and unaliased custom targets. Different CARGO_TARGET_DIR values can still miss; this is not the same-path warm pair or a cross-worktree reuse measurement."
  stop_owned_daemon
  reset_cache_directory
  mkdir -p "$WORKDIR/real-custom-a" "$WORKDIR/real-custom-b"
  sccache_stats
  run_build "real-mount-custom-cold" "$ROOT" "$WORKDIR/real-custom-a" "${CACHE_ENV[@]}"
  real_custom_cold="$(cat "$WORKDIR/last-elapsed")"
  sccache_stats
  run_build "real-mount-custom-second" "$ROOT" "$WORKDIR/real-custom-b" "${CACHE_ENV[@]}"
  real_custom_second="$(cat "$WORKDIR/last-elapsed")"
  sccache_stats
else
  log ""
  log "### real Linux stable mounts: not aliased in this environment"
fi

log ""
log "### clippy (tiny probe; sccache 0.17 does not cache clippy-driver itself)"
reset_bench_directory "$WORKDIR/probe"
mkdir -p "$WORKDIR/probe/src" "$WORKDIR/probe/scripts" "$WORKDIR/probe/.cargo" "$WORKDIR/probe-target"
cat >"$WORKDIR/probe/Cargo.toml" <<'EOF'
[package]
name = "orbit_cache_probe"
version = "0.1.0"
edition = "2021"

# Keep the fixture standalone even when WORKDIR is inside a checkout.
[workspace]
EOF
printf 'pub fn value() -> i32 { 1 }\n' >"$WORKDIR/probe/src/lib.rs"
cp "$ROOT/scripts/rustc-compiler-cache.sh" "$WORKDIR/probe/scripts/rustc-compiler-cache.sh"
cp "$ROOT/.cargo/config.toml" "$WORKDIR/probe/.cargo/config.toml"
chmod +x "$WORKDIR/probe/scripts/rustc-compiler-cache.sh"
(cd "$WORKDIR/probe" && cargo generate-lockfile >/dev/null)
reset_bench_directory "$WORKDIR/probe-target"
log "==> clippy-probe"
(
  cd "$WORKDIR/probe"
  time_cmd clippy env "${CACHE_ENV[@]}" \
    CARGO_TARGET_DIR="$WORKDIR/probe-target" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS="$JOBS" \
    cargo clippy --offline --locked --quiet
) || log "    clippy command failed (recorded, not fatal for the harness)"
clippy_s="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
log "clippy_s=$clippy_s"
log "clippy_note=clippy-driver analysis invocations are non-cacheable (missing output_dir / multiple input files on the tiny fixture). rustc dependency compilations on a representative graph may still hit. Do not treat clippy-driver misses as proof that Clippy phases cannot reuse cached rlibs. Opt out of the wrapper with ORBIT_COMPILER_CACHE=0; keep bounded CARGO_BUILD_JOBS."

log ""
log "### incremental (sccache 0.17.0 prohibits CARGO_INCREMENTAL=1; it does not degrade to rustc)"
reset_bench_directory "$WORKDIR/probe-target"
log "==> incremental-probe"
incr_status=0
(
  cd "$WORKDIR/probe"
  time_cmd incremental env "${CACHE_ENV[@]}" \
    CARGO_TARGET_DIR="$WORKDIR/probe-target" CARGO_INCREMENTAL=1 CARGO_BUILD_JOBS="$JOBS" \
    cargo check --offline --locked --quiet
) || incr_status=$?
incr_s="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
log "incremental_s=$incr_s"
log "incremental_exit=$incr_status"
log "incremental_note=pinned sccache 0.17.0 exits with 'incremental compilation is prohibited: Unset CARGO_INCREMENTAL to continue' rather than falling through to uncached rustc. Keep CARGO_INCREMENTAL=0 on workers. To use incremental, set ORBIT_COMPILER_CACHE=0."

conc_wall="skipped"
ns_detail="bwrap namespace probe not run"
if can_create_ns; then
  ns_detail="bwrap can create mount namespaces"
  log ""
  log "### concurrent Linux namespaces (different source, shared disk cache)"
  reset_bench_directory "$WORKDIR/probe-c"
  reset_bench_directory "$WORKDIR/probe-d"
  mkdir -p "$WORKDIR/probe-c/src" "$WORKDIR/probe-d/src" \
    "$WORKDIR/probe-c/scripts" "$WORKDIR/probe-d/scripts" \
    "$WORKDIR/probe-c/.cargo" "$WORKDIR/probe-d/.cargo" \
    "$WORKDIR/probe-c-target" "$WORKDIR/probe-d-target"
  cp "$WORKDIR/probe/Cargo.toml" "$WORKDIR/probe-c/Cargo.toml"
  cp "$WORKDIR/probe/Cargo.toml" "$WORKDIR/probe-d/Cargo.toml"
  printf 'pub fn value() -> i32 { 1 }\n' >"$WORKDIR/probe-c/src/lib.rs"
  printf 'pub fn value() -> i32 { 4 }\n' >"$WORKDIR/probe-d/src/lib.rs"
  printf 'fn main() { print!("{}", orbit_cache_probe::value() + 2); }\n' \
    >"$WORKDIR/probe-c/src/main.rs"
  cp "$WORKDIR/probe-c/src/main.rs" "$WORKDIR/probe-d/src/main.rs"
  cp "$ROOT/scripts/rustc-compiler-cache.sh" "$WORKDIR/probe-c/scripts/"
  cp "$ROOT/scripts/rustc-compiler-cache.sh" "$WORKDIR/probe-d/scripts/"
  cp "$ROOT/.cargo/config.toml" "$WORKDIR/probe-c/.cargo/"
  cp "$ROOT/.cargo/config.toml" "$WORKDIR/probe-d/.cargo/"
  (cd "$WORKDIR/probe-c" && cargo generate-lockfile >/dev/null)
  cp "$WORKDIR/probe-c/Cargo.lock" "$WORKDIR/probe-d/Cargo.lock"
  require_bench_ownership
  conc_start="$(now)"
  PATH="$PATH" TMPDIR="$WORKDIR" SCCACHE_DIR="$CACHE_DIR" \
    SCCACHE_SERVER_UDS="$BENCH_UDS" ORBIT_COMPILER_CACHE_BIN="$SCCACHE_BIN" \
    ORBIT_COMPILER_CACHE_BENCH_OWNER="$OWNER_TOKEN" \
    ORBIT_COMPILER_CACHE_REQUIRE_NS="$REQUIRE_NS" \
    "$ROOT/scripts/test-compiler-cache-namespaces.sh" | tee -a "$RESULT"
  conc_wall="$(secs "$conc_start" "$(now)")"
else
  err="$(tr '\n' ' ' <"$WORKDIR/bwrap-probe.err" 2>/dev/null || true)"
  ns_detail="cannot create mount namespace: ${err:-bwrap missing}"
  log ""
  log "### concurrent Linux namespaces: skipped ($ns_detail)"
  if [[ "$REQUIRE_NS" == "1" ]]; then
    log "ORBIT_COMPILER_CACHE_REQUIRE_NS=1: treating skip as failure"
    exit 1
  fi
  log "Run scripts/test-compiler-cache-namespaces.sh on a host that can create user namespaces (not nested inside an implementer sandbox)."
fi

log ""
log "### summary"
log "baseline_a_cold_s=$base_a"
log "baseline_b_cold_s=$base_b"
log "unnormalized_a_cold_s=$unnormalized_a"
log "unnormalized_b_second_s=$unnormalized_b"
log "cache_a_cold_s=$cache_a"
log "cache_b_warm_s=$cache_b"
log "real_mount_custom_cold_s=${real_custom_cold:-skipped}"
log "real_mount_custom_second_s=${real_custom_second:-skipped}"
log "clippy_s=$clippy_s"
log "incremental_s=$incr_s"
log "cache_concurrent=$conc_wall"
log "namespace_probe=$ns_detail"
log "results=$RESULT"
log "opt_out=ORBIT_COMPILER_CACHE=0"
log "do_not_enable_global_cache=1"
printf 'compiler-cache bench complete: %s\n' "$RESULT"
