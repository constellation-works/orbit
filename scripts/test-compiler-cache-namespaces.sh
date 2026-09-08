#!/usr/bin/env bash
# Concurrent Linux mount-namespace regression for the rustc compiler-cache wrapper.
#
# A shared TCP sccache daemon (SCCACHE_CLIENT_SIDE=0) compiles through whichever
# namespace started it, so a second sandbox with different source at the same
# stable mounts fails with a missing rlib. The wrapper's default client-side
# compile plus a private /tmp UDS keeps outputs in each namespace's target
# while still sharing the disk cache. [ORB-11755]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REQUIRE_NS="${ORBIT_COMPILER_CACHE_REQUIRE_NS:-0}"

skip() {
  printf 'test-compiler-cache-namespaces: skip: %s\n' "$*"
  if [[ "$REQUIRE_NS" == "1" ]]; then
    printf 'test-compiler-cache-namespaces: FAIL: namespace test required\n' >&2
    exit 1
  fi
  exit 0
}

fail() {
  printf 'test-compiler-cache-namespaces: FAIL: %s\n' "$*" >&2
  exit 1
}

# Refuse unit-test fixtures before any skip. Nested sandboxes previously
# skipped at bwrap and never noticed fake HOME/sccache on a real host.
if [[ -n "${FAKE_RUSTC_LOG-}" || -n "${FAKE_SCCACHE_LOG-}" ]]; then
  fail "unit-test fixture environment leaked into live namespace test"
fi
if [[ -n "${ORBIT_COMPILER_CACHE_BIN-}" && -f "${ORBIT_COMPILER_CACHE_BIN}" ]] \
  && grep -Fq 'FAKE_SCCACHE_LOG' "$ORBIT_COMPILER_CACHE_BIN" 2>/dev/null; then
  fail "ORBIT_COMPILER_CACHE_BIN is the unit-test fake sccache"
fi

if ! command -v bwrap >/dev/null 2>&1; then
  skip "bwrap not on PATH"
fi

probe_err="$(mktemp)"
if ! bwrap --die-with-parent --unshare-all --share-net --ro-bind / / --tmpfs /tmp --dev /dev --proc /proc -- /bin/true 2>"$probe_err"; then
  err="$(tr '\n' ' ' <"$probe_err" | sed 's/[[:space:]]*$//')"
  rm -f "$probe_err"
  skip "cannot create a mount namespace ($err)"
fi
rm -f "$probe_err"

sccache_bin="${ORBIT_COMPILER_CACHE_BIN:-}"
if [[ -z "$sccache_bin" && -x "${HOME:-}/.orbit/cache/bin/sccache" ]]; then
  sccache_bin="$HOME/.orbit/cache/bin/sccache"
fi
if [[ -z "$sccache_bin" ]] && command -v sccache >/dev/null 2>&1; then
  sccache_bin="$(command -v sccache)"
fi
if [[ -z "$sccache_bin" || ! -x "$sccache_bin" ]]; then
  skip "sccache not available for live namespace compile"
fi
if ! command -v cargo >/dev/null 2>&1; then
  skip "cargo not on PATH"
fi

alloc_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

WORKDIR="$(mktemp -d)"
PIDS=()
cleanup() {
  local pid
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  for pid in "${PIDS[@]:-}"; do
    wait "$pid" >/dev/null 2>&1 || true
  done
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

write_probe() {
  local dest="$1" value="$2"
  mkdir -p "$dest/src" "$dest/scripts" "$dest/.cargo" "$dest/bin"
  cat >"$dest/Cargo.toml" <<'EOF'
[package]
name = "orbit_cache_probe"
version = "0.1.0"
edition = "2021"
EOF
  printf 'pub fn value() -> i32 { %s }\n' "$value" >"$dest/src/lib.rs"
  cat >"$dest/src/main.rs" <<'EOF'
fn main() {
    print!("{}", orbit_cache_probe::value() + 2);
}
EOF
  cp "$ROOT/scripts/rustc-compiler-cache.sh" "$dest/scripts/rustc-compiler-cache.sh"
  chmod +x "$dest/scripts/rustc-compiler-cache.sh"
  cp "$ROOT/.cargo/config.toml" "$dest/.cargo/config.toml"
  # Copy sccache into the tree so --tmpfs /tmp cannot hide a /tmp-installed binary.
  cp "$sccache_bin" "$dest/bin/sccache"
  chmod +x "$dest/bin/sccache"
  (cd "$dest" && cargo generate-lockfile >/dev/null)
}

write_probe "$WORKDIR/src-c" 1
write_probe "$WORKDIR/src-d" 4
write_probe "$WORKDIR/src-e" 1
mkdir -p "$WORKDIR/tgt-c" "$WORKDIR/tgt-d" "$WORKDIR/tgt-e" "$WORKDIR/cache" \
  "$WORKDIR/logs" "$WORKDIR/sync"

# Inside the namespace the tree is also at /tmp/orbit-workspace; prefer that
# copy so stats and cargo use the same wrapper-visible binary.
NS_SCCACHE=/tmp/orbit-workspace/bin/sccache

run_ns() {
  local tree="$1" target="$2" logfile="$3"
  shift 3
  local extra_binds=()
  extra_binds+=(--bind "$tree" "$tree")
  extra_binds+=(--bind "$target" "$target")
  extra_binds+=(--bind "$WORKDIR/cache" "$WORKDIR/cache")
  extra_binds+=(--bind "$WORKDIR/sync" "$WORKDIR/sync")
  extra_binds+=(--dir /tmp/orbit-workspace --bind "$tree" /tmp/orbit-workspace)
  extra_binds+=(--dir /tmp/orbit-build --bind "$target" /tmp/orbit-build)
  extra_binds+=(--ro-bind "$tree/bin/sccache" "$tree/bin/sccache")
  if [[ -n "${HOME:-}" && -d "$HOME" ]]; then
    extra_binds+=(--ro-bind "$HOME" "$HOME")
  fi
  bwrap \
    --die-with-parent \
    --new-session \
    --unshare-all \
    --share-net \
    --ro-bind / / \
    --dev /dev \
    --proc /proc \
    --tmpfs /tmp \
    "${extra_binds[@]}" \
    --chdir /tmp/orbit-workspace \
    --setenv HOME "${HOME:-$WORKDIR}" \
    --setenv PATH "$PATH" \
    --setenv CARGO_TARGET_DIR "$target" \
    --setenv CARGO_INCREMENTAL 0 \
    --setenv CARGO_BUILD_JOBS 1 \
    --setenv SCCACHE_DIR "$WORKDIR/cache" \
    --setenv ORBIT_COMPILER_CACHE_BIN "$NS_SCCACHE" \
    "$@" \
    -- \
    /bin/bash -c '
      set -euo pipefail
      cargo run --offline --locked --quiet
      printf "\n__SCCACHE_STATS__\n"
      if [[ -n "${SCCACHE_SERVER_PORT:-}" ]]; then
        SCCACHE_DIR="$SCCACHE_DIR" SCCACHE_SERVER_PORT="$SCCACHE_SERVER_PORT" \
          "$ORBIT_COMPILER_CACHE_BIN" --show-stats || true
      else
        uds="${SCCACHE_SERVER_UDS:-/tmp/orbit-sccache.sock}"
        SCCACHE_DIR="$SCCACHE_DIR" SCCACHE_SERVER_UDS="$uds" \
          "$ORBIT_COMPILER_CACHE_BIN" --show-stats || true
      fi
    ' >"$logfile" 2>"$logfile.err"
}

# Owner namespace for the shared-daemon hazard: compile, signal ready, hold
# until the sibling finishes so the TCP daemon stays in this mount ns.
run_ns_hold() {
  local tree="$1" target="$2" logfile="$3" ready="$4" done="$5"
  shift 5
  local extra_binds=()
  extra_binds+=(--bind "$tree" "$tree")
  extra_binds+=(--bind "$target" "$target")
  extra_binds+=(--bind "$WORKDIR/cache" "$WORKDIR/cache")
  extra_binds+=(--bind "$WORKDIR/sync" "$WORKDIR/sync")
  extra_binds+=(--dir /tmp/orbit-workspace --bind "$tree" /tmp/orbit-workspace)
  extra_binds+=(--dir /tmp/orbit-build --bind "$target" /tmp/orbit-build)
  extra_binds+=(--ro-bind "$tree/bin/sccache" "$tree/bin/sccache")
  if [[ -n "${HOME:-}" && -d "$HOME" ]]; then
    extra_binds+=(--ro-bind "$HOME" "$HOME")
  fi
  bwrap \
    --die-with-parent \
    --new-session \
    --unshare-all \
    --share-net \
    --ro-bind / / \
    --dev /dev \
    --proc /proc \
    --tmpfs /tmp \
    "${extra_binds[@]}" \
    --chdir /tmp/orbit-workspace \
    --setenv HOME "${HOME:-$WORKDIR}" \
    --setenv PATH "$PATH" \
    --setenv CARGO_TARGET_DIR "$target" \
    --setenv CARGO_INCREMENTAL 0 \
    --setenv CARGO_BUILD_JOBS 1 \
    --setenv SCCACHE_DIR "$WORKDIR/cache" \
    --setenv ORBIT_COMPILER_CACHE_BIN "$NS_SCCACHE" \
    --setenv READY_FILE "$ready" \
    --setenv DONE_FILE "$done" \
    "$@" \
    -- \
    /bin/bash -c '
      set -euo pipefail
      cargo run --offline --locked --quiet
      printf "\n__SCCACHE_STATS__\n"
      SCCACHE_DIR="$SCCACHE_DIR" SCCACHE_SERVER_PORT="$SCCACHE_SERVER_PORT" \
        "$ORBIT_COMPILER_CACHE_BIN" --show-stats || true
      : >"$READY_FILE"
      while [[ ! -f "$DONE_FILE" ]]; do
        sleep 0.05
      done
      SCCACHE_DIR="$SCCACHE_DIR" SCCACHE_SERVER_PORT="$SCCACHE_SERVER_PORT" \
        "$ORBIT_COMPILER_CACHE_BIN" --stop-server >/dev/null 2>&1 || true
    ' >"$logfile" 2>"$logfile.err"
}

wait_for_file() {
  local path="$1" attempts=0
  while [[ ! -f "$path" ]]; do
    attempts=$((attempts + 1))
    [[ "$attempts" -lt 400 ]] || fail "timed out waiting for $path"
    sleep 0.05
  done
}

probe_out() {
  local file="$1"
  awk 'BEGIN {out=""} /^__SCCACHE_STATS__$/ {exit} {out=$0} END {printf "%s", out}' "$file"
}

has_rust_hit() {
  local file="$1"
  grep -Eq 'Cache hits \(Rust\)[[:space:]]+[1-9][0-9]*' "$file" \
    || grep -Eq '^Cache hits[[:space:]]+[1-9][0-9]*$' "$file"
}

# Wrapper defaults: per-namespace daemon + client-side compile. Different
# sources must each produce the expected binary while sharing SCCACHE_DIR.
run_ns "$WORKDIR/src-c" "$WORKDIR/tgt-c" "$WORKDIR/logs/ok-c" &
PIDS+=($!)
pid_c=$!
run_ns "$WORKDIR/src-d" "$WORKDIR/tgt-d" "$WORKDIR/logs/ok-d" &
PIDS+=($!)
pid_d=$!
status=0
wait "$pid_c" || status=1
wait "$pid_d" || status=1
PIDS=()
if [[ "$status" -ne 0 ]]; then
  cat "$WORKDIR/logs/ok-c.err" "$WORKDIR/logs/ok-d.err" >&2 || true
  fail "isolated-daemon concurrent namespaces failed"
fi
assert_out_c="$(probe_out "$WORKDIR/logs/ok-c")"
assert_out_d="$(probe_out "$WORKDIR/logs/ok-d")"
[[ "$assert_out_c" == "3" ]] || fail "namespace C expected 3, got '$assert_out_c'"
[[ "$assert_out_d" == "6" ]] || fail "namespace D expected 6, got '$assert_out_d'"
[[ -x "$WORKDIR/tgt-c/debug/orbit_cache_probe" ]] || fail "namespace C missing private binary"
[[ -x "$WORKDIR/tgt-d/debug/orbit_cache_probe" ]] || fail "namespace D missing private binary"
if ! compgen -G "$WORKDIR/tgt-c/debug/deps/liborbit_cache_probe-*.rlib" >/dev/null; then
  fail "namespace C missing private rlib"
fi
if compgen -G "$WORKDIR/src-d/target/debug/deps/liborbit_cache_probe-*.rlib" >/dev/null; then
  fail "namespace D wrote into the sibling source tree"
fi

# Cross-worktree warm reuse: identical source, private cleaned target, shared disk cache.
rm -rf "$WORKDIR/tgt-e"
mkdir -p "$WORKDIR/tgt-e"
run_ns "$WORKDIR/src-e" "$WORKDIR/tgt-e" "$WORKDIR/logs/ok-e" \
  || {
    cat "$WORKDIR/logs/ok-e.err" >&2 || true
    fail "warm identical-source namespace failed"
  }
assert_out_e="$(probe_out "$WORKDIR/logs/ok-e")"
[[ "$assert_out_e" == "3" ]] || fail "warm namespace E expected 3, got '$assert_out_e'"
if ! has_rust_hit "$WORKDIR/logs/ok-e"; then
  cat "$WORKDIR/logs/ok-e" >&2 || true
  fail "identical second worktree did not report Rust cache hits"
fi

# Shared TCP daemon + client-side compile: rustc runs in each namespace.
cs_port="$(alloc_port)"
rm -rf "$WORKDIR/tgt-c" "$WORKDIR/tgt-d"
mkdir -p "$WORKDIR/tgt-c" "$WORKDIR/tgt-d"
run_ns "$WORKDIR/src-c" "$WORKDIR/tgt-c" "$WORKDIR/logs/cs-c" \
  --setenv SCCACHE_CLIENT_SIDE 1 \
  --setenv SCCACHE_SERVER_PORT "$cs_port" &
PIDS+=($!)
pid_c=$!
run_ns "$WORKDIR/src-d" "$WORKDIR/tgt-d" "$WORKDIR/logs/cs-d" \
  --setenv SCCACHE_CLIENT_SIDE 1 \
  --setenv SCCACHE_SERVER_PORT "$cs_port" &
PIDS+=($!)
pid_d=$!
status=0
wait "$pid_c" || status=1
wait "$pid_d" || status=1
PIDS=()
if [[ "$status" -ne 0 ]]; then
  cat "$WORKDIR/logs/cs-c.err" "$WORKDIR/logs/cs-d.err" >&2 || true
  fail "client-side shared TCP daemon concurrent namespaces failed"
fi
[[ "$(probe_out "$WORKDIR/logs/cs-c")" == "3" ]] || fail "client-side namespace C expected 3"
[[ "$(probe_out "$WORKDIR/logs/cs-d")" == "6" ]] || fail "client-side namespace D expected 6"

# Shared TCP daemon, server-side compile: hold C's namespace (and daemon) until D
# has finished, reproducing the missing-artifact failure.
bad_port="$(alloc_port)"
rm -rf "$WORKDIR/tgt-c" "$WORKDIR/tgt-d" "$WORKDIR/cache" "$WORKDIR/sync"
mkdir -p "$WORKDIR/tgt-c" "$WORKDIR/tgt-d" "$WORKDIR/cache" "$WORKDIR/sync"
ready="$WORKDIR/sync/c-ready"
donef="$WORKDIR/sync/c-done"
run_ns_hold "$WORKDIR/src-c" "$WORKDIR/tgt-c" "$WORKDIR/logs/bad-c" "$ready" "$donef" \
  --setenv SCCACHE_CLIENT_SIDE 0 \
  --setenv SCCACHE_SERVER_PORT "$bad_port" &
PIDS+=($!)
pid_c=$!
wait_for_file "$ready"
run_ns "$WORKDIR/src-d" "$WORKDIR/tgt-d" "$WORKDIR/logs/bad-d" \
  --setenv SCCACHE_CLIENT_SIDE 0 \
  --setenv SCCACHE_SERVER_PORT "$bad_port" &
PIDS+=($!)
pid_d=$!
set +e
wait "$pid_d"
d_status=$?
set -e
: >"$donef"
wait "$pid_c" || true
PIDS=()
if [[ "$d_status" -eq 0 ]]; then
  fail "shared TCP daemon should not succeed for a different-source namespace"
fi
if ! grep -E -q 'does not exist|extern location|/tmp/orbit-workspace' "$WORKDIR/logs/bad-d.err" \
  && ! grep -E -q 'does not exist|extern location|/tmp/orbit-workspace' "$WORKDIR/logs/bad-d"; then
  cat "$WORKDIR/logs/bad-d.err" >&2 || true
  fail "shared-daemon failure did not look like the missing-artifact mismatch"
fi

printf 'test-compiler-cache-namespaces: ok\n'
