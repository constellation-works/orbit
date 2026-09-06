#!/usr/bin/env bash
# Fallback and enablement tests for scripts/rustc-compiler-cache.sh [ORB-11259].
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
} > "${FAKE_SCCACHE_ENV:-/dev/null}"
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

# 4. Writable cache + sccache -> sccache then rustc.
export FAKE_SCCACHE_LOG="$TMP/sccache-hit.log"
export FAKE_SCCACHE_ENV="$TMP/sccache-hit.env"
export FAKE_RUSTC_LOG="$TMP/rustc-hit.log"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_ENV" "$FAKE_RUSTC_LOG"
"$WRAPPER" "$TMP/bin/rustc" --crate-name cached
[[ -f "$FAKE_SCCACHE_LOG" ]] || fail "writable cache did not exec sccache"
[[ -f "$FAKE_RUSTC_LOG" ]] || fail "sccache did not exec rustc"
head -n 1 "$FAKE_SCCACHE_LOG" | grep -Fq "$TMP/bin/rustc" || fail "sccache first arg should be rustc"
grep -E -q '^SCCACHE_DIR=/.+' "$FAKE_SCCACHE_ENV" || fail "wrapper must export SCCACHE_DIR"

# 5. Operator surfaces exist and do not require a host sccache.
"$ROOT/scripts/compiler-cache.sh" --help >/dev/null
HOME="$HOME" "$ROOT/scripts/compiler-cache.sh" status >/dev/null

# 6. When the Linux stable mounts alias this checkout, argv paths are rewritten.
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
export FAKE_RUSTC_LOG="$TMP/rustc-rewrite.log"
export CARGO_TARGET_DIR="$fake_tgt"
rm -f "$FAKE_SCCACHE_LOG" "$FAKE_SCCACHE_ENV" "$FAKE_RUSTC_LOG"
"$WRAPPER" "$TMP/bin/rustc" --out-dir "$fake_tgt/debug" "$ROOT/crates/orbit-types/src/lib.rs"
grep -Fq "$stable_tgt/debug" "$FAKE_SCCACHE_LOG" || fail "out-dir should rewrite onto the stable build mount"
grep -Fq "$stable_src/crates/orbit-types/src/lib.rs" "$FAKE_SCCACHE_LOG" || fail "source path should rewrite onto the stable workspace mount"

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

# sccache must return the compiler's status unchanged.
export FAKE_RUSTC_EXIT_STATUS=23
set +e
"$WRAPPER" "$TMP/bin/rustc" --crate-name status-check
status=$?
set -e
assert_eq "$status" "23" "compiler exit status"
unset CARGO_TARGET_DIR
unset ORBIT_COMPILER_CACHE_STABLE_SRC ORBIT_COMPILER_CACHE_STABLE_TGT

printf 'test-compiler-cache: ok\n'
