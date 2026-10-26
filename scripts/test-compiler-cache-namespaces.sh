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

# Whether this process runs in an Orbit agent lane. Orbit's CLI runner sets
# ORBIT_SCRATCH_DIR, an existing directory it creates before the provider
# starts, on every agent it dispatches into a worktree, and nowhere else. On
# Linux that lane is the agent's Bubblewrap sandbox, which refuses the nested
# namespaces this regression needs. The owner's candidate_validate and a claim
# host's claim_validate run the required commands from the worker's own
# allowlisted environment, outside any agent sandbox, so they never carry it.
in_orbit_agent_lane() {
  [[ -n "${ORBIT_SCRATCH_DIR:-}" && -d "$ORBIT_SCRATCH_DIR" ]]
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

# Resolve the cache as rustc-compiler-cache.sh does. Without an enabled cache,
# sccache binary, and writable cache directory the wrapper runs plain rustc, so
# there is no cached compile to verify. Say so; do not skip or probe namespaces.
sccache_bin=""
if [[ -n "${ORBIT_COMPILER_CACHE_BIN:-}" && -x "$ORBIT_COMPILER_CACHE_BIN" ]]; then
  sccache_bin="$ORBIT_COMPILER_CACHE_BIN"
elif [[ -n "${HOME:-}" && -x "$HOME/.orbit/cache/bin/sccache" ]]; then
  sccache_bin="$HOME/.orbit/cache/bin/sccache"
elif command -v sccache >/dev/null 2>&1; then
  sccache_bin="$(command -v sccache)"
fi
cache_dir="${SCCACHE_DIR:-}"
if [[ -z "$cache_dir" && -n "${HOME:-}" ]]; then
  cache_dir="$HOME/.orbit/cache/compiler"
fi
cache_disabled_reason=""
if [[ "${ORBIT_COMPILER_CACHE:-}" == "0" ]]; then
  cache_disabled_reason="ORBIT_COMPILER_CACHE=0"
elif [[ -z "$sccache_bin" ]]; then
  cache_disabled_reason="no sccache binary"
elif [[ -z "$cache_dir" ]] || ! mkdir -p "$cache_dir" 2>/dev/null || [[ ! -w "$cache_dir" ]]; then
  cache_disabled_reason="cache directory unset or not writable"
fi
if [[ -n "$cache_disabled_reason" ]]; then
  printf 'test-compiler-cache-namespaces: not applicable: host compiler cache is disabled (%s)\n' "$cache_disabled_reason"
  exit 0
fi

if ! command -v bwrap >/dev/null 2>&1; then
  skip "bwrap not on PATH"
fi

probe_err="$(mktemp)"
if ! bwrap --die-with-parent --unshare-all --share-net --ro-bind / / --tmpfs /tmp --dev /dev --proc /proc -- /bin/true 2>"$probe_err"; then
  err="$(tr '\n' ' ' <"$probe_err" | sed 's/[[:space:]]*$//')"
  rm -f "$probe_err"
  # Inside the agent sandbox this is the expected refusal, not lost coverage:
  # the same required command runs again outside it before delivery. An
  # explicit REQUIRE_NS=1 still demands the namespaces here and fails below.
  if in_orbit_agent_lane && [[ "$REQUIRE_NS" != "1" ]]; then
    printf "test-compiler-cache-namespaces: not applicable inside the Orbit agent sandbox, which refuses nested namespaces (%s); covered outside the sandbox by the owner's candidate_validate or the claim host's claim_validate run of the required commands\n" "$err"
    exit 0
  fi
  skip "cannot create a mount namespace ($err)"
fi
rm -f "$probe_err"

if ! command -v cargo >/dev/null 2>&1; then
  skip "cargo not on PATH"
fi

HOLD_PID=""
HOLD_RELEASE=""
alloc_port() {
  # Hold the socket until release_port so no other process takes the port while
  # the caller reads it. Callers release it before starting any sccache daemon:
  # the daemon binds this same address, which the held socket blocks.
  HOLD_RELEASE="$WORKDIR/port-release.$$.${RANDOM:-0}"
  # One output file per allocation: a reused file still holds the previous
  # port until this python child truncates it, so polling it would return a
  # stale or empty port.
  local out="$HOLD_RELEASE.port"
  rm -f "$out"
  python3 - "$HOLD_RELEASE" >"$out" <<'PY' &
import os, socket, sys, time
release = sys.argv[1]
sock = socket.socket()
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1], flush=True)
while not os.path.exists(release):
    time.sleep(0.02)
sock.close()
PY
  HOLD_PID=$!
  local attempts=0
  while [[ ! -s "$out" ]]; do
    attempts=$((attempts + 1))
    [[ "$attempts" -lt 200 ]] || fail "timed out allocating a port"
    sleep 0.05
  done
  ALLOC_PORT="$(cat "$out")"
  # An empty port makes the wrapper fall back to a private UDS daemon, which
  # silently skips the shared-TCP scenario, so refuse anything but a number.
  [[ "$ALLOC_PORT" =~ ^[0-9]+$ ]] || fail "allocated an invalid port: '$ALLOC_PORT'"
}

release_port() {
  [[ -n "$HOLD_RELEASE" ]] || return 0
  : >"$HOLD_RELEASE"
  if [[ -n "$HOLD_PID" ]]; then
    local attempts=0
    while kill -0 "$HOLD_PID" 2>/dev/null && [[ "$attempts" -lt 200 ]]; do
      attempts=$((attempts + 1))
      sleep 0.05
    done
    kill "$HOLD_PID" >/dev/null 2>&1 || true
    wait "$HOLD_PID" 2>/dev/null || true
  fi
  HOLD_PID=""
  HOLD_RELEASE=""
}

WORKDIR="$(mktemp -d)"
PIDS=()
STAGE="fixture-setup"
PARTICIPANTS=()
LOGFILES=()
STATUSES=()

# Failure-only I/O: at most 4096 payload bytes per stream (head and tail so
# compiler startup and final statistics survive), plus labels/byte counts.
# Print while the fixture still exists; cleanup below then removes it.
report_stream() {
  local stream="$1" file="$2" bytes limit=4096 half=2048
  if [[ ! -f "$file" ]]; then
    printf '%s unavailable\n' "$stream" >&2
    return
  fi
  bytes="$(wc -c <"$file")"
  bytes="${bytes//[[:space:]]/}"
  printf '%s bytes=%s limit=%s\n' "$stream" "$bytes" "$limit" >&2
  if [[ "$bytes" -gt "$limit" ]]; then
    head -c "$half" "$file" >&2
    printf '\n[truncated: %s bytes omitted]\n' "$((bytes - limit))" >&2
    tail -c "$half" "$file" >&2
  else
    cat "$file" >&2
  fi
  printf '\n' >&2
}

report_participants() {
  local i
  for ((i=0; i<${#PARTICIPANTS[@]}; i++)); do
    printf 'test-compiler-cache-namespaces: stage=%s participant=%s exit_status=%s\n' \
      "$STAGE" "${PARTICIPANTS[$i]}" "${STATUSES[$i]}" >&2
    report_stream stdout "${LOGFILES[$i]}"
    report_stream stderr "${LOGFILES[$i]}.err"
  done
}

WAIT_TIMEOUT="${ORBIT_COMPILER_CACHE_NS_WAIT_SECONDS:-300}"

pid_running() {
  kill -0 "$1" 2>/dev/null || return 1
  if [[ -r "/proc/$1/status" ]] && grep -q '^State:[[:space:]]*Z' "/proc/$1/status"; then
    return 1
  fi
  return 0
}

# Bounded wait for one child; sets BOUNDED_STATUS (124 on timeout).
bounded_wait() {
  local pid="$1" attempts=0 max_attempts
  max_attempts=$((WAIT_TIMEOUT * 20))
  BOUNDED_STATUS=0
  while pid_running "$pid"; do
    attempts=$((attempts + 1))
    if [[ "$attempts" -ge "$max_attempts" ]]; then
      kill "$pid" >/dev/null 2>&1 || true
      wait "$pid" 2>/dev/null || true
      BOUNDED_STATUS=124
      return 0
    fi
    sleep 0.05
  done
  wait "$pid" || BOUNDED_STATUS=$?
  return 0
}

# Reap both peers even when the first fails; never collapse their statuses.
wait_participants() {
  local i status=0
  for ((i=0; i<${#PIDS[@]}; i++)); do
    bounded_wait "${PIDS[$i]}"
    STATUSES[$i]="$BOUNDED_STATUS"
    [[ "${STATUSES[$i]}" -eq 0 ]] || status=1
  done
  PIDS=()
  return "$status"
}

cleanup() {
  local exit_status=$? pid i
  if [[ -n "${HOLD_PID:-}" ]]; then
    kill "$HOLD_PID" >/dev/null 2>&1 || true
  fi
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  for ((i=0; i<${#PIDS[@]}; i++)); do
    STATUSES[$i]=0
    wait "${PIDS[$i]}" >/dev/null 2>&1 || STATUSES[$i]=$?
  done
  if [[ "$exit_status" -ne 0 ]]; then
    report_participants || true
  fi
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
      hold_attempts=0
      while [[ ! -f "$DONE_FILE" ]]; do
        hold_attempts=$((hold_attempts + 1))
        [[ "$hold_attempts" -lt 6000 ]] || exit 124
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
STAGE=isolated-daemon
PARTICIPANTS=(C D)
LOGFILES=("$WORKDIR/logs/ok-c" "$WORKDIR/logs/ok-d")
STATUSES=(pending pending)
run_ns "$WORKDIR/src-c" "$WORKDIR/tgt-c" "$WORKDIR/logs/ok-c" &
PIDS+=($!)
run_ns "$WORKDIR/src-d" "$WORKDIR/tgt-d" "$WORKDIR/logs/ok-d" &
PIDS+=($!)
wait_participants || fail "isolated-daemon concurrent namespaces failed"
assert_out_c="$(probe_out "$WORKDIR/logs/ok-c")"
assert_out_d="$(probe_out "$WORKDIR/logs/ok-d")"
[[ "$assert_out_c" == "3" ]] || fail "namespace C expected 3"
[[ "$assert_out_d" == "6" ]] || fail "namespace D expected 6"
[[ -x "$WORKDIR/tgt-c/debug/orbit_cache_probe" ]] || fail "namespace C missing private binary"
[[ -x "$WORKDIR/tgt-d/debug/orbit_cache_probe" ]] || fail "namespace D missing private binary"
if ! compgen -G "$WORKDIR/tgt-c/debug/deps/liborbit_cache_probe-*.rlib" >/dev/null; then
  fail "namespace C missing private rlib"
fi
if ! compgen -G "$WORKDIR/tgt-d/debug/deps/liborbit_cache_probe-*.rlib" >/dev/null; then
  fail "namespace D missing private rlib"
fi
if compgen -G "$WORKDIR/src-d/target/debug/deps/liborbit_cache_probe-*.rlib" >/dev/null; then
  fail "namespace D wrote into the sibling source tree"
fi

# Cross-worktree warm reuse: identical source, private cleaned target, shared disk cache.
STAGE=warm-identical-source
PARTICIPANTS=(E)
LOGFILES=("$WORKDIR/logs/ok-e")
STATUSES=(0)
rm -rf "$WORKDIR/tgt-e"
mkdir -p "$WORKDIR/tgt-e"
run_ns "$WORKDIR/src-e" "$WORKDIR/tgt-e" "$WORKDIR/logs/ok-e" \
  || STATUSES[0]=$?
[[ "${STATUSES[0]}" -eq 0 ]] || fail "warm identical-source namespace failed"
assert_out_e="$(probe_out "$WORKDIR/logs/ok-e")"
[[ "$assert_out_e" == "3" ]] || fail "warm namespace E expected 3"
if ! has_rust_hit "$WORKDIR/logs/ok-e"; then
  fail "identical second worktree did not report Rust cache hits"
fi

# Shared TCP daemon + client-side compile: rustc runs in each namespace.
STAGE=client-side-shared-tcp
PARTICIPANTS=(C D)
LOGFILES=("$WORKDIR/logs/cs-c" "$WORKDIR/logs/cs-d")
STATUSES=(pending pending)
alloc_port; cs_port="$ALLOC_PORT"
release_port
rm -rf "$WORKDIR/tgt-c" "$WORKDIR/tgt-d"
mkdir -p "$WORKDIR/tgt-c" "$WORKDIR/tgt-d"
run_ns "$WORKDIR/src-c" "$WORKDIR/tgt-c" "$WORKDIR/logs/cs-c" \
  --setenv SCCACHE_CLIENT_SIDE 1 \
  --setenv SCCACHE_SERVER_PORT "$cs_port" &
PIDS+=($!)
run_ns "$WORKDIR/src-d" "$WORKDIR/tgt-d" "$WORKDIR/logs/cs-d" \
  --setenv SCCACHE_CLIENT_SIDE 1 \
  --setenv SCCACHE_SERVER_PORT "$cs_port" &
PIDS+=($!)
wait_participants || fail "client-side shared TCP daemon concurrent namespaces failed"
[[ "$(probe_out "$WORKDIR/logs/cs-c")" == "3" ]] || fail "client-side namespace C expected 3"
[[ "$(probe_out "$WORKDIR/logs/cs-d")" == "6" ]] || fail "client-side namespace D expected 6"

# Shared TCP daemon, server-side compile: hold C's namespace (and daemon) until D
# has finished, reproducing the missing-artifact failure.
STAGE=server-side-shared-tcp
PARTICIPANTS=(C D)
LOGFILES=("$WORKDIR/logs/bad-c" "$WORKDIR/logs/bad-d")
STATUSES=(pending pending)
alloc_port; bad_port="$ALLOC_PORT"
release_port
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
# D's failure only means something if C compiled through the daemon on this
# port. Otherwise D simply started its own daemon, so fail on the precondition.
if ! grep -Eq 'Compile requests[[:space:]]+[1-9]' "$WORKDIR/logs/bad-c"; then
  fail "participant C did not compile through the shared daemon on port $bad_port"
fi
run_ns "$WORKDIR/src-d" "$WORKDIR/tgt-d" "$WORKDIR/logs/bad-d" \
  --setenv SCCACHE_CLIENT_SIDE 0 \
  --setenv SCCACHE_SERVER_PORT "$bad_port" &
PIDS+=($!)
pid_d=$!
bounded_wait "$pid_d"; STATUSES[1]="$BOUNDED_STATUS"
d_status="${STATUSES[1]}"
: >"$donef"
bounded_wait "$pid_c"; STATUSES[0]="$BOUNDED_STATUS"
PIDS=()
if [[ "$d_status" -eq 0 ]]; then
  fail "shared TCP daemon should not succeed for a different-source namespace"
fi
if ! grep -E -q 'does not exist|extern location|/tmp/orbit-workspace' "$WORKDIR/logs/bad-d.err" \
  && ! grep -E -q 'does not exist|extern location|/tmp/orbit-workspace' "$WORKDIR/logs/bad-d"; then
  fail "shared-daemon failure did not look like the missing-artifact mismatch"
fi

printf 'test-compiler-cache-namespaces: ok\n'
