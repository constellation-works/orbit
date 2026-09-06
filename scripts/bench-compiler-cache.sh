#!/usr/bin/env bash
# Two-worktree cold/warm/concurrent compiler-cache benchmark [ORB-11259].
#
# Uses equivalent cargo commands against an unchanged HEAD, with a private
# CARGO_TARGET_DIR per worktree. Does not share a target directory.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PACKAGE="${ORBIT_COMPILER_CACHE_BENCH_PACKAGE:-orbit-types}"
CARGO_CMD=(cargo check -p "$PACKAGE" --offline)
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
WORKDIR="${ORBIT_COMPILER_CACHE_BENCH_DIR:-/tmp/orbit-compiler-cache-bench-$$}"
CACHE_DIR="${SCCACHE_DIR:-$WORKDIR/cache}"
RESULT="${ORBIT_COMPILER_CACHE_BENCH_RESULT:-$WORKDIR/results-$STAMP.txt}"

HEAD_SHA="$(git rev-parse HEAD)"
cleanup() {
  rm -rf "$WORKDIR/a" "$WORKDIR/b" "$WORKDIR/a-target" "$WORKDIR/b-target"
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

run_build() {
  local label="$1" tree="$2" target="$3"
  shift 3
  local start end elapsed
  mkdir -p "$target"
  rm -rf "${target:?}/"*
  log "==> $label  (tree=$tree target=$target)"
  start="$(now)"
  (
    cd "$tree"
    env "$@" CARGO_TARGET_DIR="$target" CARGO_INCREMENTAL=0 \
      "${CARGO_CMD[@]}"
  )
  end="$(now)"
  elapsed="$(secs "$start" "$end")"
  log "    wall_s=$elapsed"
  printf '%s\n' "$elapsed" >"$WORKDIR/last-elapsed"
}

sccache_stats() {
  local bin="${ORBIT_COMPILER_CACHE_BIN:-}"
  if [[ -z "$bin" ]]; then
    if [[ -x "${HOME:-}/.orbit/cache/bin/sccache" ]]; then
      bin="$HOME/.orbit/cache/bin/sccache"
    elif command -v sccache >/dev/null 2>&1; then
      bin="$(command -v sccache)"
    else
      return 0
    fi
  fi
  log "--- sccache --show-stats ---"
  SCCACHE_DIR="$CACHE_DIR" "$bin" --show-stats 2>&1 | tee -a "$RESULT" || true
}

log "compiler-cache bench $STAMP"
log "head=$HEAD_SHA"
log "package=$PACKAGE"
log "cmd=${CARGO_CMD[*]}"
log "workdir=$WORKDIR"
log "cache_dir=$CACHE_DIR"

# Copy two identical trees. `git worktree add` writes the shared git dir, which
# a managed Linux worktree cannot do; the copies still share one commit and
# keep private target directories.
mkdir -p "$WORKDIR/a" "$WORKDIR/b"
if git archive "$HEAD_SHA" >/dev/null 2>&1; then
  git archive "$HEAD_SHA" | tar -x -C "$WORKDIR/a"
  git archive "$HEAD_SHA" | tar -x -C "$WORKDIR/b"
else
  tar -C "$ROOT" --exclude target --exclude .git --exclude tmp -cf - . \
    | tar -C "$WORKDIR/a" -xf -
  tar -C "$ROOT" --exclude target --exclude .git --exclude tmp -cf - . \
    | tar -C "$WORKDIR/b" -xf -
fi
# Overlay the wrapper and cargo config so both trees exercise the same hook
# even when those files are still untracked in this worktree.
mkdir -p "$WORKDIR/a/.cargo" "$WORKDIR/b/.cargo" "$WORKDIR/a/scripts" "$WORKDIR/b/scripts"
cp "$ROOT/.cargo/config.toml" "$WORKDIR/a/.cargo/config.toml"
cp "$ROOT/.cargo/config.toml" "$WORKDIR/b/.cargo/config.toml"
cp "$ROOT/scripts/rustc-compiler-cache.sh" "$WORKDIR/a/scripts/rustc-compiler-cache.sh"
cp "$ROOT/scripts/rustc-compiler-cache.sh" "$WORKDIR/b/scripts/rustc-compiler-cache.sh"
chmod +x "$WORKDIR/a/scripts/rustc-compiler-cache.sh" "$WORKDIR/b/scripts/rustc-compiler-cache.sh"

# Baseline: wrapper forced off, two sequential cold worktrees.
log ""
log "### baseline (ORBIT_COMPILER_CACHE=0, sequential cold)"
run_build "baseline-a-cold" "$WORKDIR/a" "$WORKDIR/a-target" ORBIT_COMPILER_CACHE=0
base_a="$(cat "$WORKDIR/last-elapsed")"
run_build "baseline-b-cold" "$WORKDIR/b" "$WORKDIR/b-target" ORBIT_COMPILER_CACHE=0
base_b="$(cat "$WORKDIR/last-elapsed")"

if ! command -v sccache >/dev/null 2>&1 && [[ ! -x "${ORBIT_COMPILER_CACHE_BIN:-}" && ! -x "${HOME:-}/.orbit/cache/bin/sccache" ]]; then
  log ""
  log "sccache is not available; cache arms skipped. Run: scripts/compiler-cache.sh setup --install"
  log "baseline_a_s=$base_a"
  log "baseline_b_s=$base_b"
  log "results=$RESULT"
  exit 0
fi

CACHE_ENV=(
  SCCACHE_DIR="$CACHE_DIR"
  SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-5G}"
)
if [[ -n "${ORBIT_COMPILER_CACHE_BIN:-}" ]]; then
  CACHE_ENV+=(ORBIT_COMPILER_CACHE_BIN="$ORBIT_COMPILER_CACHE_BIN")
fi

log ""
log "### cache sequential (cold then warm, private targets)"
# Reset cache for a true cold fill. This directory is the bench-private cache,
# never the live worker cache, unless the caller overrode SCCACHE_DIR.
if [[ "$CACHE_DIR" == "$WORKDIR/cache" ]]; then
  rm -rf "$CACHE_DIR"
  mkdir -p "$CACHE_DIR"
fi
sccache_stats
run_build "cache-a-cold" "$WORKDIR/a" "$WORKDIR/a-target" "${CACHE_ENV[@]}"
cache_a="$(cat "$WORKDIR/last-elapsed")"
sccache_stats
run_build "cache-b-warm" "$WORKDIR/b" "$WORKDIR/b-target" "${CACHE_ENV[@]}"
cache_b="$(cat "$WORKDIR/last-elapsed")"
sccache_stats

log ""
log "### cache concurrent (two warm-empty-target worktrees)"
rm -rf "$WORKDIR/a-target" "$WORKDIR/b-target"
mkdir -p "$WORKDIR/a-target" "$WORKDIR/b-target"
conc_start="$(now)"
(
  cd "$WORKDIR/a"
  env "${CACHE_ENV[@]}" CARGO_TARGET_DIR="$WORKDIR/a-target" CARGO_INCREMENTAL=0 \
    "${CARGO_CMD[@]}"
) >"$WORKDIR/conc-a.log" 2>&1 &
pid_a=$!
(
  cd "$WORKDIR/b"
  env "${CACHE_ENV[@]}" CARGO_TARGET_DIR="$WORKDIR/b-target" CARGO_INCREMENTAL=0 \
    "${CARGO_CMD[@]}"
) >"$WORKDIR/conc-b.log" 2>&1 &
pid_b=$!
status=0
wait "$pid_a" || status=1
wait "$pid_b" || status=1
conc_end="$(now)"
conc_wall="$(secs "$conc_start" "$conc_end")"
cat "$WORKDIR/conc-a.log" >>"$RESULT"
cat "$WORKDIR/conc-b.log" >>"$RESULT"
[[ "$status" -eq 0 ]] || {
  log "concurrent builds failed"
  exit 1
}
log "    concurrent_wall_s=$conc_wall"
sccache_stats

log ""
log "### summary"
log "baseline_a_cold_s=$base_a"
log "baseline_b_cold_s=$base_b"
log "cache_a_cold_s=$cache_a"
log "cache_b_warm_s=$cache_b"
log "cache_concurrent_wall_s=$conc_wall"
log "results=$RESULT"
printf 'compiler-cache bench complete: %s\n' "$RESULT"
