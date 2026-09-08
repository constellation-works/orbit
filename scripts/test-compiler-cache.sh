#!/usr/bin/env bash
# Fallback and enablement tests for scripts/rustc-compiler-cache.sh [ORB-11259] [ORB-11755] [ORB-11767].
# No full crate compile: the wrapper is a rustc argv passthrough.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAPPER="$ROOT/scripts/rustc-compiler-cache.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() {
  printf 'test-compiler-cache: FAIL: %s\n' "$*" >&2
  exit 1
}

assert_eq() {
  local got="$1" want="$2" msg="$3"
  [[ "$got" == "$want" ]] || fail "$msg: got '$got' want '$want'"
}

file_id() {
  if stat -L -c '%d:%i' "$1" >/dev/null 2>&1; then
    stat -L -c '%d:%i' "$1"
  else
    stat -L -f '%d:%i' "$1"
  fi
}

mkdir -p "$TMP/bin"
# Fake rustc: records argv and exits 0.
cat > "$TMP/bin/rustc" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "${FAKE_RUSTC_LOG}"
exit "${FAKE_RUSTC_EXIT_STATUS:-0}"
EOF
chmod +x "$TMP/bin/rustc"

# Fake sccache kept off PATH; tests opt in through ORBIT_COMPILER_CACHE_BIN.
cat > "$TMP/sccache" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "${FAKE_SCCACHE_LOG}"
{
  printf 'SCCACHE_DIR=%s\n' "${SCCACHE_DIR-}"
  printf 'SCCACHE_BASEDIRS=%s\n' "${SCCACHE_BASEDIRS-}"
  printf 'SCCACHE_CLIENT_SIDE=%s\n' "${SCCACHE_CLIENT_SIDE-}"
  printf 'SCCACHE_SERVER_UDS=%s\n' "${SCCACHE_SERVER_UDS-}"
  printf 'SCCACHE_SERVER_PORT=%s\n' "${SCCACHE_SERVER_PORT-}"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-}"
} > "${FAKE_SCCACHE_ENV:-/dev/null}"
pwd > "${FAKE_SCCACHE_CWD:-/dev/null}"
printf '%s' "${CACHE_ENV_SPACES-}" > "${FAKE_SCCACHE_SPACES:-/dev/null}"
printf '%s' "${CACHE_ENV_EQUALS-}" > "${FAKE_SCCACHE_EQUALS:-/dev/null}"
printf '%s' "${CACHE_ENV_MULTILINE-}" > "${FAKE_SCCACHE_MULTILINE:-/dev/null}"
printf '%s' "${CACHE_ENV_EMPTY-}" > "${FAKE_SCCACHE_EMPTY:-/dev/null}"
printf '%s' "${CACHE_ENV_BOUNDARY-}" > "${FAKE_SCCACHE_BOUNDARY:-/dev/null}"
printf '%s' "${CACHE_ENV_SOURCE_BOUNDARY-}" > "${FAKE_SCCACHE_SOURCE_BOUNDARY:-/dev/null}"
exec "$@"
EOF
chmod +x "$TMP/sccache"

export HOME="$TMP/home"
mkdir -p "$HOME"
unset SCCACHE_DIR ORBIT_COMPILER_CACHE_BIN RUSTC_WRAPPER ORBIT_COMPILER_CACHE
unset SCCACHE_CLIENT_SIDE SCCACHE_SERVER_UDS SCCACHE_SERVER_PORT
unset ORBIT_COMPILER_CACHE_STABLE_SRC ORBIT_COMPILER_CACHE_STABLE_TGT
unset ORBIT_COMPILER_CACHE_SERVER_UDS
export ORBIT_COMPILER_CACHE_DEBUG=0
# Isolate PATH so a host sccache cannot leak into fallback cases.
export PATH="$TMP/bin:/usr/bin:/bin"

# 1. Forced off -> exec rustc with original argv even if sccache is configured.
export ORBIT_COMPILER_CACHE_BIN="$TMP/sccache"
export FAKE_RUSTC_LOG="$TMP/rustc-forced-off.log"
export FAKE_SCCACHE_LOG="$TMP/sccache-forced-off.log"
rm -f "$FAKE_RUSTC_LOG" "$FAKE_SCCACHE_LOG"
ORBIT_COMPILER_CACHE=0 "$WRAPPER" "$TMP/bin/rustc" --crate-name demo -o /dev/null
[[ -f "$FAKE_RUSTC_LOG" ]] || fail "forced-off wrapper did not exec rustc"
[[ ! -f "$FAKE_SCCACHE_LOG" ]] || fail "forced-off wrapper must not exec sccache"
assert_eq "$(tr '\n' ' ' < "$FAKE_RUSTC_LOG" | sed 's/ *$//')" "--crate-name demo -o /dev/null" "forced-off argv"

# 2. Missing sccache -> exec rustc.
unset ORBIT_COMPILER_CACHE_BIN
export FAKE_RUSTC_LOG="$TMP/rustc-missing.log"
export FAKE_SCCACHE_LOG="$TMP/sccache-missing.log"
rm -f "$FAKE_RUSTC_LOG" "$FAKE_SCCACHE_LOG"
"$WRAPPER" "$TMP/bin/rustc" --crate-name demo
[[ -f "$FAKE_RUSTC_LOG" ]] || fail "missing-sccache wrapper did not exec rustc"
[[ ! -f "$FAKE_SCCACHE_LOG" ]] || fail "missing sccache must not exec sccache"

# 3. Unwritable cache dir -> rustc, not sccache.
mkdir -p "$HOME/.orbit/cache/compiler"
chmod a-w "$HOME/.orbit/cache/compiler"
export ORBIT_COMPILER_CACHE_BIN="$TMP/sccache"
export FAKE_SCCACHE_LOG="$TMP/sccache-unwritable.log"
export FAKE_RUSTC_LOG="$TMP/rustc-unwritable.log"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_RUSTC_LOG"
"$WRAPPER" "$TMP/bin/rustc" --emit metadata
[[ -f "$FAKE_RUSTC_LOG" ]] || fail "unwritable cache did not exec rustc"
[[ ! -f "$FAKE_SCCACHE_LOG" ]] || fail "unwritable cache must not exec sccache"
chmod u+w "$HOME/.orbit/cache/compiler"

# 4. Writable cache + sccache -> sccache then rustc, with per-namespace daemon defaults.
export FAKE_SCCACHE_LOG="$TMP/sccache-hit.log"
export FAKE_SCCACHE_ENV="$TMP/sccache-hit.env"
export FAKE_SCCACHE_CWD="$TMP/sccache-hit.cwd"
export FAKE_RUSTC_LOG="$TMP/rustc-hit.log"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_ENV" "$FAKE_SCCACHE_CWD" "$FAKE_RUSTC_LOG"
"$WRAPPER" "$TMP/bin/rustc" --crate-name cached
[[ -f "$FAKE_SCCACHE_LOG" ]] || fail "writable cache did not exec sccache"
[[ -f "$FAKE_RUSTC_LOG" ]] || fail "sccache did not exec rustc"
head -n 1 "$FAKE_SCCACHE_LOG" | grep -Fq "$TMP/bin/rustc" || fail "sccache first arg should be rustc"
grep -E -q '^SCCACHE_DIR=/.+' "$FAKE_SCCACHE_ENV" || fail "wrapper must export SCCACHE_DIR"
grep -E -q '^SCCACHE_CLIENT_SIDE=1$' "$FAKE_SCCACHE_ENV" || fail "wrapper must default SCCACHE_CLIENT_SIDE=1"
grep -E -q '^SCCACHE_SERVER_UDS=/tmp/orbit-sccache.sock$' "$FAKE_SCCACHE_ENV" || fail "wrapper must default a private /tmp UDS"
grep -E -q '^SCCACHE_SERVER_PORT=$' "$FAKE_SCCACHE_ENV" || fail "wrapper must not invent a TCP port when using UDS"

# 5. Operator surfaces exist and do not require a host sccache.
"$ROOT/scripts/compiler-cache.sh" --help >/dev/null
HOME="$HOME" "$ROOT/scripts/compiler-cache.sh" status >/dev/null

# 6. When the Linux stable mounts alias this checkout, argv paths are rewritten
# and rustc cwd is the stable source mount (provider agent cwd is not changed).
fake_tgt="$TMP/stable-target"
stable_src="$TMP/orbit-workspace"
stable_tgt="$TMP/orbit-build"
mkdir -p "$fake_tgt" "$stable_src"
ln -s "$ROOT/Cargo.toml" "$stable_src/Cargo.toml"
ln -s "$fake_tgt" "$stable_tgt"
export ORBIT_COMPILER_CACHE_STABLE_SRC="$stable_src"
export ORBIT_COMPILER_CACHE_STABLE_TGT="$stable_tgt"
export FAKE_SCCACHE_LOG="$TMP/sccache-rewrite.log"
export FAKE_SCCACHE_ENV="$TMP/sccache-rewrite.env"
export FAKE_SCCACHE_CWD="$TMP/sccache-rewrite.cwd"
export FAKE_RUSTC_LOG="$TMP/rustc-rewrite.log"
export CARGO_TARGET_DIR="$fake_tgt"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_ENV" "$FAKE_SCCACHE_CWD" "$FAKE_RUSTC_LOG"
"$WRAPPER" "$TMP/bin/rustc" --out-dir "$fake_tgt/debug" "$ROOT/crates/orbit-types/src/lib.rs"
grep -Fq "$stable_tgt/debug" "$FAKE_SCCACHE_LOG" || fail "out-dir should rewrite onto the stable build mount"
grep -Fq "$stable_src/crates/orbit-types/src/lib.rs" "$FAKE_SCCACHE_LOG" || fail "source path should rewrite onto the stable workspace mount"
assert_eq "$(cat "$FAKE_SCCACHE_CWD")" "$stable_src" "stable-mount rustc cwd"

# Environment values are read without splitting, and only paths rooted at the
# checkout or target directory are rewritten. Keep expected files in TMP so
# this remains fully disposable under Bash 3.2 and current Linux Bash.
export CACHE_ENV_SPACES='value with spaces = preserved'
export CACHE_ENV_EQUALS='left=middle=right'
export CACHE_ENV_MULTILINE="$fake_tgt/debug
last line"
export CACHE_ENV_EMPTY=''
export CACHE_ENV_BOUNDARY="$fake_tgt-sibling"
export CACHE_ENV_SOURCE_BOUNDARY="$ROOT-sibling"
export FAKE_SCCACHE_SPACES="$TMP/env-spaces.actual"
export FAKE_SCCACHE_EQUALS="$TMP/env-equals.actual"
export FAKE_SCCACHE_MULTILINE="$TMP/env-multiline.actual"
export FAKE_SCCACHE_EMPTY="$TMP/env-empty.actual"
export FAKE_SCCACHE_BOUNDARY="$TMP/env-boundary.actual"
export FAKE_SCCACHE_SOURCE_BOUNDARY="$TMP/env-source-boundary.actual"
printf '%s' "$CACHE_ENV_SPACES" > "$TMP/env-spaces.expected"
printf '%s' "$CACHE_ENV_EQUALS" > "$TMP/env-equals.expected"
printf '%s\nlast line' "$stable_tgt/debug" > "$TMP/env-multiline.expected"
: > "$TMP/env-empty.expected"
printf '%s' "$CACHE_ENV_BOUNDARY" > "$TMP/env-boundary.expected"
printf '%s' "$CACHE_ENV_SOURCE_BOUNDARY" > "$TMP/env-source-boundary.expected"
"$WRAPPER" "$TMP/bin/rustc" --out-dir "$fake_tgt/debug" "$ROOT/crates/orbit-types/src/lib.rs" "$fake_tgt-sibling" "$ROOT-sibling"
grep -Fq "$fake_tgt-sibling" "$FAKE_SCCACHE_LOG" || fail "target path-prefix boundary should remain unchanged in argv"
grep -Fq "$ROOT-sibling" "$FAKE_SCCACHE_LOG" || fail "source path-prefix boundary should remain unchanged in argv"
cmp "$TMP/env-spaces.expected" "$FAKE_SCCACHE_SPACES" || fail "spaces in environment value were not preserved"
cmp "$TMP/env-equals.expected" "$FAKE_SCCACHE_EQUALS" || fail "equals signs in environment value were not preserved"
cmp "$TMP/env-multiline.expected" "$FAKE_SCCACHE_MULTILINE" || fail "multiline environment value was not rewritten exactly"
cmp "$TMP/env-empty.expected" "$FAKE_SCCACHE_EMPTY" || fail "empty environment value was not preserved"
cmp "$TMP/env-boundary.expected" "$FAKE_SCCACHE_BOUNDARY" || fail "target path-prefix boundary was rewritten unexpectedly"
cmp "$TMP/env-source-boundary.expected" "$FAKE_SCCACHE_SOURCE_BOUNDARY" || fail "source path-prefix boundary was rewritten unexpectedly"

# Relative compiler inputs stay relative so they resolve after the stable chdir
# instead of being expanded to a unique worktree prefix.
export FAKE_SCCACHE_LOG="$TMP/sccache-relative.log"
export FAKE_SCCACHE_CWD="$TMP/sccache-relative.cwd"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_CWD"
(
  cd "$ROOT"
  "$WRAPPER" "$TMP/bin/rustc" --out-dir "$fake_tgt/debug" crates/orbit-types/src/lib.rs
)
grep -Fq "crates/orbit-types/src/lib.rs" "$FAKE_SCCACHE_LOG" || fail "relative source arg should stay relative"
grep -Fq "$ROOT/crates/orbit-types/src/lib.rs" "$FAKE_SCCACHE_LOG" && fail "relative source arg must not expand to the original worktree path"
assert_eq "$(cat "$FAKE_SCCACHE_CWD")" "$stable_src" "relative-arg rustc cwd after stable chdir"

# Nested workspace-member cwd keeps its suffix so relative src/lib.rs does not
# change referent when the stable source mount aliases the repo root.
mkdir -p "$stable_src/crates/orbit-types"
export FAKE_SCCACHE_LOG="$TMP/sccache-nested-cwd.log"
export FAKE_SCCACHE_CWD="$TMP/sccache-nested-cwd.cwd"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_CWD"
(
  cd "$ROOT/crates/orbit-types"
  "$WRAPPER" "$TMP/bin/rustc" --out-dir "$fake_tgt/debug" src/lib.rs
)
grep -Fq "src/lib.rs" "$FAKE_SCCACHE_LOG" || fail "nested-cwd relative src/lib.rs should stay relative"
grep -Fq "$ROOT/crates/orbit-types/src/lib.rs" "$FAKE_SCCACHE_LOG" && fail "nested-cwd relative input must not expand to the original worktree path"
assert_eq "$(cat "$FAKE_SCCACHE_CWD")" "$stable_src/crates/orbit-types" "nested member rustc cwd"

# cwd outside the checkout is left alone even when the stable source mount aliases.
mkdir -p "$TMP/external-cwd"
export FAKE_SCCACHE_LOG="$TMP/sccache-external-cwd.log"
export FAKE_SCCACHE_CWD="$TMP/sccache-external-cwd.cwd"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_CWD"
(
  cd "$TMP/external-cwd"
  "$WRAPPER" "$TMP/bin/rustc" --crate-name external src/lib.rs
)
assert_eq "$(cat "$FAKE_SCCACHE_CWD")" "$TMP/external-cwd" "external cwd must not move to the repo root"

# 7. Custom CARGO_TARGET_DIR that is not aliased to STABLE_TGT still rewrites
# source paths and cwd. Silently skipping all rewriting was the measured miss.
custom_tgt="$TMP/custom-target"
mkdir -p "$custom_tgt"
export CARGO_TARGET_DIR="$custom_tgt"
export FAKE_SCCACHE_LOG="$TMP/sccache-custom-tgt.log"
export FAKE_SCCACHE_ENV="$TMP/sccache-custom-tgt.env"
export FAKE_SCCACHE_CWD="$TMP/sccache-custom-tgt.cwd"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_ENV" "$FAKE_SCCACHE_CWD"
"$WRAPPER" "$TMP/bin/rustc" --out-dir "$custom_tgt/debug" "$ROOT/crates/orbit-types/src/lib.rs"
grep -Fq "$stable_src/crates/orbit-types/src/lib.rs" "$FAKE_SCCACHE_LOG" || fail "custom target must not prevent source rewrite"
grep -Fq "$custom_tgt/debug" "$FAKE_SCCACHE_LOG" || fail "custom --out-dir should stay in the private target"
grep -Fq "$stable_tgt/debug" "$FAKE_SCCACHE_LOG" && fail "unaliased custom target must not rewrite onto STABLE_TGT"
grep -E -q "^CARGO_TARGET_DIR=${custom_tgt}$" "$FAKE_SCCACHE_ENV" || fail "unaliased CARGO_TARGET_DIR must not be rewritten"
assert_eq "$(cat "$FAKE_SCCACHE_CWD")" "$stable_src" "custom-target rustc cwd still normalizes"

# 8. Explicit daemon overrides are preserved (shared-daemon regression setup).
unset CARGO_TARGET_DIR
export SCCACHE_CLIENT_SIDE=0
export SCCACHE_SERVER_PORT=45265
unset SCCACHE_SERVER_UDS
export FAKE_SCCACHE_ENV="$TMP/sccache-override.env"
export FAKE_SCCACHE_LOG="$TMP/sccache-override.log"
rm -f "$FAKE_SCCACHE_ENV" "$FAKE_SCCACHE_LOG"
"$WRAPPER" "$TMP/bin/rustc" --crate-name override
grep -E -q '^SCCACHE_CLIENT_SIDE=0$' "$FAKE_SCCACHE_ENV" || fail "explicit SCCACHE_CLIENT_SIDE=0 must be preserved"
grep -E -q '^SCCACHE_SERVER_PORT=45265$' "$FAKE_SCCACHE_ENV" || fail "explicit SCCACHE_SERVER_PORT must be preserved"
grep -E -q '^SCCACHE_SERVER_UDS=$' "$FAKE_SCCACHE_ENV" || fail "explicit TCP port must not also set a UDS"
unset SCCACHE_CLIENT_SIDE SCCACHE_SERVER_PORT

# 9. Without a source alias, rustc cwd stays the caller's directory. Force
# non-matching aliases so a live /tmp/orbit-workspace mount cannot leak in.
mkdir -p "$TMP/no-src" "$TMP/no-tgt"
export ORBIT_COMPILER_CACHE_STABLE_SRC="$TMP/no-src"
export ORBIT_COMPILER_CACHE_STABLE_TGT="$TMP/no-tgt"
export FAKE_SCCACHE_CWD="$TMP/sccache-noalias.cwd"
export FAKE_SCCACHE_LOG="$TMP/sccache-noalias.log"
rm -f "$FAKE_SCCACHE_CWD" "$FAKE_SCCACHE_LOG"
noalias_cwd="$(pwd)"
"$WRAPPER" "$TMP/bin/rustc" --crate-name noalias
assert_eq "$(cat "$FAKE_SCCACHE_CWD")" "$noalias_cwd" "unaliased wrapper must not chdir"
unset ORBIT_COMPILER_CACHE_STABLE_SRC ORBIT_COMPILER_CACHE_STABLE_TGT

# sccache must return the compiler's status unchanged.
export FAKE_RUSTC_EXIT_STATUS=23
set +e
"$WRAPPER" "$TMP/bin/rustc" --crate-name status-check
status=$?
set -e
assert_eq "$status" "23" "compiler exit status"
unset FAKE_RUSTC_EXIT_STATUS
unset CARGO_TARGET_DIR

# 10. Actual Linux stable mounts in this sandbox: source rewrite + cwd, even
# with a custom target path that is not /tmp/orbit-build.
if [[ -e /tmp/orbit-workspace/Cargo.toml && -e "$ROOT/Cargo.toml" ]] \
  && [[ "$(file_id /tmp/orbit-workspace/Cargo.toml)" == "$(file_id "$ROOT/Cargo.toml")" ]]; then
  real_custom="$TMP/real-custom-target"
  mkdir -p "$real_custom"
  unset ORBIT_COMPILER_CACHE_STABLE_SRC ORBIT_COMPILER_CACHE_STABLE_TGT
  export CARGO_TARGET_DIR="$real_custom"
  export FAKE_SCCACHE_LOG="$TMP/sccache-real-mount.log"
  export FAKE_SCCACHE_ENV="$TMP/sccache-real-mount.env"
  export FAKE_SCCACHE_CWD="$TMP/sccache-real-mount.cwd"
  rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_ENV" "$FAKE_SCCACHE_CWD"
  (
    cd "$ROOT"
    "$WRAPPER" "$TMP/bin/rustc" --out-dir "$real_custom/debug" "$ROOT/crates/orbit-types/src/lib.rs"
  )
  grep -Fq "/tmp/orbit-workspace/crates/orbit-types/src/lib.rs" "$FAKE_SCCACHE_LOG" \
    || fail "real stable source mount should rewrite the input path"
  grep -Fq "$real_custom/debug" "$FAKE_SCCACHE_LOG" \
    || fail "real-mount custom --out-dir should remain private"
  grep -Fq "/tmp/orbit-build/debug" "$FAKE_SCCACHE_LOG" \
    && fail "custom target must not silently alias onto /tmp/orbit-build"
  grep -E -q "^CARGO_TARGET_DIR=${real_custom}$" "$FAKE_SCCACHE_ENV" \
    || fail "real-mount custom CARGO_TARGET_DIR must stay unaliased"
  assert_eq "$(cat "$FAKE_SCCACHE_CWD")" "/tmp/orbit-workspace" "real stable-mount rustc cwd"
  export FAKE_SCCACHE_LOG="$TMP/sccache-real-nested.log"
  export FAKE_SCCACHE_CWD="$TMP/sccache-real-nested.cwd"
  rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_CWD"
  (
    cd "$ROOT/crates/orbit-types"
    "$WRAPPER" "$TMP/bin/rustc" --out-dir "$real_custom/debug" src/lib.rs
  )
  grep -Fq "src/lib.rs" "$FAKE_SCCACHE_LOG" || fail "real-mount nested relative src/lib.rs should stay relative"
  assert_eq "$(cat "$FAKE_SCCACHE_CWD")" "/tmp/orbit-workspace/crates/orbit-types" "real-mount nested member rustc cwd"
  unset CARGO_TARGET_DIR
fi

# The live sibling must refuse fixture env before any bwrap skip, so a nested
# call cannot pass by skipping namespaces.
set +e
FAKE_RUSTC_LOG="$TMP/leaked-rustc.log" \
  "$ROOT/scripts/test-compiler-cache-namespaces.sh" \
  >"$TMP/guard.out" 2>"$TMP/guard.err"
guard_status=$?
set -e
[[ "$guard_status" -ne 0 ]] || fail "live suite must reject fixture FAKE_* vars"
grep -F -q 'unit-test fixture environment leaked' "$TMP/guard.err" \
  || fail "live suite should name the fixture leak, got: $(tr '\n' ' ' <"$TMP/guard.err")"

set +e
FAKE_RUSTC_LOG= FAKE_SCCACHE_LOG= \
  ORBIT_COMPILER_CACHE_BIN="$TMP/sccache" \
  "$ROOT/scripts/test-compiler-cache-namespaces.sh" \
  >"$TMP/guard-bin.out" 2>"$TMP/guard-bin.err"
guard_bin_status=$?
set -e
[[ "$guard_bin_status" -ne 0 ]] || fail "live suite must reject the fixture sccache binary"
grep -F -q 'unit-test fake sccache' "$TMP/guard-bin.err" \
  || fail "live suite should reject fixture sccache, got: $(tr '\n' ' ' <"$TMP/guard-bin.err")"

# Live mount-namespace compile lives in the sibling script and is invoked as
# a separate CI process after this unit process exits.

printf 'test-compiler-cache: ok\n'
