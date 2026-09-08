#!/usr/bin/env bash
# Isolated compiler-cache benchmark [ORB-11259] [ORB-11755].
#
# Records source revision, toolchain, workload, wall/CPU time, and sccache
# hits/misses/non-cacheable reasons for: no-cache baseline, cache cold fill,
# a different worktree warm build, Clippy, incremental, and concurrent
# different-source Linux mount namespaces when Bubblewrap can create them.
#
# Uses private CARGO_TARGET_DIR values per tree. The disk cache and UDS live
# only under the bench workdir; the harness refuses a caller SCCACHE_DIR outside
# that tree so it cannot reset an operator cache. Does not enable a global host
# cache.
set -euo pipefail

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
BENCH_UDS="$WORKDIR/sccache.sock"
SCCACHE_BIN=""

HEAD_SHA="$(git rev-parse HEAD 2>/dev/null || printf 'unknown')"
case "$CACHE_DIR" in
  "$WORKDIR" | "$WORKDIR"/*) ;;
  *)
    printf 'bench-compiler-cache: refusing SCCACHE_DIR=%s (must be under %s)\n' \
      "$CACHE_DIR" "$WORKDIR" >&2
    exit 1
    ;;
esac

stop_owned_daemon() {
  [[ -n "$SCCACHE_BIN" && -x "$SCCACHE_BIN" ]] || return 0
  SCCACHE_DIR="$CACHE_DIR" SCCACHE_SERVER_UDS="$BENCH_UDS" \
    "$SCCACHE_BIN" --stop-server >/dev/null 2>&1 || true
}

cleanup() {
  stop_owned_daemon
  rm -rf "$WORKDIR/a" "$WORKDIR/b" "$WORKDIR/a-target" "$WORKDIR/b-target" \
    "$WORKDIR/probe-c" "$WORKDIR/probe-d" "$WORKDIR/probe-c-target" "$WORKDIR/probe-d-target"
}
trap cleanup EXIT

mkdir -p "$WORKDIR" "$CACHE_DIR"
: >"$RESULT"

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
  local os arch asset url tmp dest
  os="$(uname -s)"
  arch="$(uname -m)"
  case "${os}/${arch}" in
    Linux/x86_64 | Linux/amd64)
      asset="sccache-${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz"
      ;;
    Linux/aarch64 | Linux/arm64)
      asset="sccache-${SCCACHE_VERSION}-aarch64-unknown-linux-musl.tar.gz"
      ;;
    Darwin/x86_64 | Darwin/amd64)
      asset="sccache-${SCCACHE_VERSION}-x86_64-apple-darwin.tar.gz"
      ;;
    Darwin/arm64 | Darwin/aarch64)
      asset="sccache-${SCCACHE_VERSION}-aarch64-apple-darwin.tar.gz"
      ;;
    *)
      return 1
      ;;
  esac
  dest="$WORKDIR/bin/sccache"
  mkdir -p "$WORKDIR/bin"
  url="https://github.com/mozilla/sccache/releases/download/${SCCACHE_VERSION}/${asset}"
  tmp="$(mktemp -d)"
  log "downloading isolated $url"
  if ! curl -fsSL "$url" -o "$tmp/$asset"; then
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
  timefile="$WORKDIR/time-$label"
  start="$(now)"
  status=0
  if [[ -x /usr/bin/time ]]; then
    /usr/bin/time -f 'wall=%e user=%U sys=%S' -o "$timefile" "$@" || status=$?
  else
    "$@" || status=$?
    printf 'wall=unknown user=unknown sys=unknown (no /usr/bin/time)\n' >"$timefile"
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
  mkdir -p "$target"
  rm -rf "${target:?}/"*
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
    "$bin" --show-stats 2>&1 | tee -a "$RESULT" || true
}

can_create_ns() {
  command -v bwrap >/dev/null 2>&1 || return 1
  bwrap --die-with-parent --unshare-all --share-net --ro-bind / / --tmpfs /tmp \
    --dev /dev --proc /proc -- /bin/true >/dev/null 2>"$WORKDIR/bwrap-probe.err"
}

copy_tree() {
  local dest="$1"
  mkdir -p "$dest"
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

mkdir -p "$WORKDIR/a" "$WORKDIR/b"
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
log "sccache_version=$("$SCCACHE_BIN" --version 2>/dev/null || echo unknown)"
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
rm -rf "$CACHE_DIR"
mkdir -p "$CACHE_DIR"
sccache_stats
run_build "unnormalized-a-cold" "$WORKDIR/a" "$WORKDIR/a-target" "${CACHE_ENV[@]}"
cache_a="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
run_build "unnormalized-b-warm" "$WORKDIR/b" "$WORKDIR/b-target" "${CACHE_ENV[@]}"
cache_b="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
log "unnormalized_note=these copies are not bind-mounted at /tmp/orbit-workspace; treat as a miss-path control, not cross-worktree reuse."

real_custom_cold=""
real_custom_warm=""
if [[ -e /tmp/orbit-workspace/Cargo.toml && -e "$ROOT/Cargo.toml" ]] \
  && [[ "$(stat -L -c '%d:%i' /tmp/orbit-workspace/Cargo.toml 2>/dev/null || stat -L -f '%d:%i' /tmp/orbit-workspace/Cargo.toml)" \
    == "$(stat -L -c '%d:%i' "$ROOT/Cargo.toml" 2>/dev/null || stat -L -f '%d:%i' "$ROOT/Cargo.toml")" ]]; then
  log ""
  log "### real Linux stable source mount + custom CARGO_TARGET_DIR (same checkout, two private targets)"
  log "Demonstrates cwd normalization and unaliased custom targets. Not a second worktree; cross-worktree reuse is the namespace arm."
  stop_owned_daemon
  rm -rf "$CACHE_DIR"
  mkdir -p "$CACHE_DIR" "$WORKDIR/real-custom-a" "$WORKDIR/real-custom-b"
  sccache_stats
  run_build "real-mount-custom-cold" "$ROOT" "$WORKDIR/real-custom-a" "${CACHE_ENV[@]}"
  real_custom_cold="$(cat "$WORKDIR/last-elapsed")"
  sccache_stats
  run_build "real-mount-custom-warm" "$ROOT" "$WORKDIR/real-custom-b" "${CACHE_ENV[@]}"
  real_custom_warm="$(cat "$WORKDIR/last-elapsed")"
  sccache_stats
else
  log ""
  log "### real Linux stable mounts: not aliased in this environment"
fi

log ""
log "### clippy (tiny probe; sccache 0.17 does not cache clippy-driver itself)"
mkdir -p "$WORKDIR/probe/src" "$WORKDIR/probe/scripts" "$WORKDIR/probe/.cargo" "$WORKDIR/probe-target"
cat >"$WORKDIR/probe/Cargo.toml" <<'EOF'
[package]
name = "orbit_cache_probe"
version = "0.1.0"
edition = "2021"
EOF
printf 'pub fn value() -> i32 { 1 }\n' >"$WORKDIR/probe/src/lib.rs"
cp "$ROOT/scripts/rustc-compiler-cache.sh" "$WORKDIR/probe/scripts/rustc-compiler-cache.sh"
cp "$ROOT/.cargo/config.toml" "$WORKDIR/probe/.cargo/config.toml"
chmod +x "$WORKDIR/probe/scripts/rustc-compiler-cache.sh"
(cd "$WORKDIR/probe" && cargo generate-lockfile >/dev/null)
rm -rf "$WORKDIR/probe-target/"*
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
rm -rf "$WORKDIR/probe-target/"*
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
  conc_start="$(now)"
  PATH="$PATH" ORBIT_COMPILER_CACHE_BIN="$SCCACHE_BIN" \
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
log "cache_a_cold_s=$cache_a"
log "cache_b_warm_s=$cache_b"
log "real_mount_custom_cold_s=${real_custom_cold:-skipped}"
log "real_mount_custom_warm_s=${real_custom_warm:-skipped}"
log "clippy_s=$clippy_s"
log "incremental_s=$incr_s"
log "cache_concurrent=$conc_wall"
log "namespace_probe=$ns_detail"
log "results=$RESULT"
log "opt_out=ORBIT_COMPILER_CACHE=0"
log "do_not_enable_global_cache=1"
printf 'compiler-cache bench complete: %s\n' "$RESULT"
