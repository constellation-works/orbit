#!/usr/bin/env bash
# rustc wrapper that uses sccache when a host compiler cache is available.
#
# Cargo invokes this as: rustc-compiler-cache.sh <rustc> [args...]
# It must never write to stdout (that stream is rustc's). Missing or unusable
# cache degrades to ordinary rustc. [ORB-11259] [ORB-11755]
set -euo pipefail

# Tests may override these aliases to avoid sharing host-global /tmp paths.
# Managed Linux sandboxes use the defaults below.
STABLE_SRC="${ORBIT_COMPILER_CACHE_STABLE_SRC:-/tmp/orbit-workspace}"
STABLE_TGT="${ORBIT_COMPILER_CACHE_STABLE_TGT:-/tmp/orbit-build}"
# Per-namespace daemon socket. Linux Bubblewrap replaces /tmp with a private
# tmpfs, so concurrent sandboxes that share the host network do not attach to
# one TCP sccache daemon (that daemon would compile through another
# namespace's stable mounts).
DEFAULT_SERVER_UDS="${ORBIT_COMPILER_CACHE_SERVER_UDS:-/tmp/orbit-sccache.sock}"

debug() {
  if [[ "${ORBIT_COMPILER_CACHE_DEBUG:-}" == "1" ]]; then
    printf 'orbit-compiler-cache: %s\n' "$*" >&2
  fi
}

if [[ "${ORBIT_COMPILER_CACHE:-}" == "0" ]]; then
  debug "disabled by ORBIT_COMPILER_CACHE=0"
  exec "$@"
fi

home="${HOME:-}"
default_dir=""
if [[ -n "$home" ]]; then
  default_dir="$home/.orbit/cache/compiler"
fi
cache_dir="${SCCACHE_DIR:-$default_dir}"

resolve_sccache() {
  if [[ -n "${ORBIT_COMPILER_CACHE_BIN:-}" && -x "${ORBIT_COMPILER_CACHE_BIN}" ]]; then
    printf '%s\n' "${ORBIT_COMPILER_CACHE_BIN}"
    return 0
  fi
  if [[ -n "$home" && -x "$home/.orbit/cache/bin/sccache" ]]; then
    printf '%s\n' "$home/.orbit/cache/bin/sccache"
    return 0
  fi
  if command -v sccache >/dev/null 2>&1; then
    command -v sccache
    return 0
  fi
  return 1
}

if ! cache_bin="$(resolve_sccache)"; then
  debug "sccache not found; using rustc"
  exec "$@"
fi

if [[ -z "$cache_dir" ]]; then
  debug "no cache directory (HOME unset); using rustc"
  exec "$@"
fi

if [[ ! -d "$cache_dir" ]]; then
  if ! mkdir -p "$cache_dir" 2>/dev/null; then
    debug "cannot create $cache_dir; using rustc"
    exec "$@"
  fi
fi

if [[ ! -w "$cache_dir" ]]; then
  debug "$cache_dir is not writable; using rustc"
  exec "$@"
fi

script_dir="$(cd "$(dirname "$0")" && pwd -P)"
repo_root="$(cd "$script_dir/.." && pwd -P)"
orig_cwd="$(pwd)"
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  target_real="${CARGO_TARGET_DIR}"
  if [[ "$target_real" != /* ]]; then
    target_real="${orig_cwd}/${target_real}"
  fi
else
  target_real="${repo_root}/target"
fi

file_id() {
  local path="$1"
  if stat -L -c '%d:%i' "$path" >/dev/null 2>&1; then
    stat -L -c '%d:%i' "$path"
  else
    stat -L -f '%d:%i' "$path"
  fi
}

same_inode() {
  local a="$1" b="$2"
  [[ -e "$a" && -e "$b" ]] || return 1
  [[ "$(file_id "$a")" == "$(file_id "$b")" ]]
}

# Source and target aliasing are independent. A private custom CARGO_TARGET_DIR
# that is not bind-mounted at STABLE_TGT used to skip *all* rewriting, so
# worktree prefixes stayed in argv, CARGO_* env, and cwd — silent cache misses.
# sccache hashes rustc cwd and the input path; --out-dir is excluded from the
# key, so source/cwd normalization is enough for reuse when outputs stay private.
source_aliased=0
target_aliased=0
if same_inode "$STABLE_SRC/Cargo.toml" "$repo_root/Cargo.toml"; then
  source_aliased=1
fi
if same_inode "$STABLE_TGT" "$target_real"; then
  target_aliased=1
fi

# Managed Linux sandboxes bind the worktree and its target/ at stable /tmp
# paths so sccache keys do not include the jrun worktree prefix.
rewrite_value() {
  local s="$1"
  if [[ "$target_aliased" -eq 1 && -n "$target_real" && ( "$s" == "$target_real" || "$s" == "$target_real/"* ) ]]; then
    printf '%s%s' "$STABLE_TGT" "${s#"$target_real"}"
    return
  fi
  if [[ "$source_aliased" -eq 1 && ( "$s" == "$repo_root" || "$s" == "$repo_root/"* ) ]]; then
    printf '%s%s' "$STABLE_SRC" "${s#"$repo_root"}"
    return
  fi
  printf '%s' "$s"
}

if [[ "$source_aliased" -eq 1 || "$target_aliased" -eq 1 ]]; then
  debug "rewriting paths onto source=$source_aliased ($STABLE_SRC) target=$target_aliased ($STABLE_TGT)"
  rewritten=()
  for arg in "$@"; do
    rewritten+=("$(rewrite_value "$arg")")
  done
  set -- "${rewritten[@]}"
  # Bash 3.2 lacks array loading, and macOS env does not provide GNU env -0.
  # compgen emits exported names without touching their values, so indirect
  # expansion keeps spaces, equals signs, newlines, and empty values intact.
  # Relative compiler args are left relative: after a stable-src chdir they
  # resolve to the same files, and rewriting them to the original worktree
  # absolute path would re-introduce unique prefixes into sccache's key.
  while IFS= read -r name; do
    value="${!name}"
    [[ -n "$name" ]] || continue
    new="$(rewrite_value "$value")"
    if [[ "$new" != "$value" ]]; then
      export "${name}=${new}"
    fi
  done < <(compgen -e)
  if [[ "$source_aliased" -eq 1 && "$target_aliased" -eq 0 ]]; then
    debug "custom CARGO_TARGET_DIR is not aliased to $STABLE_TGT; source/cwd only"
  fi
fi

# sccache hashes rustc cwd into the rustc cache key (and embeds it in rlibs).
# Bubblewrap --chdir stays on the real worktree so the provider agent cwd is
# unchanged; only this rustc wrapper remaps compiler cwd onto the stable
# source mount. Preserve a nested suffix (crates/foo) so relative rustc
# inputs keep their referent. Do not move an unrelated cwd onto the repo root.
if [[ "$source_aliased" -eq 1 ]]; then
  dest_cwd=""
  case "$orig_cwd" in
    "$STABLE_SRC" | "$STABLE_SRC"/*)
      dest_cwd="$orig_cwd"
      ;;
    "$repo_root")
      dest_cwd="$STABLE_SRC"
      ;;
    "$repo_root"/*)
      dest_cwd="$STABLE_SRC${orig_cwd#"$repo_root"}"
      ;;
  esac
  if [[ -n "$dest_cwd" && "$orig_cwd" != "$dest_cwd" ]]; then
    if cd "$dest_cwd"; then
      export PWD="$dest_cwd"
      debug "cwd normalized $orig_cwd -> $dest_cwd"
    else
      debug "cannot chdir to $dest_cwd; keeping $orig_cwd"
    fi
  fi
fi

export SCCACHE_DIR="$cache_dir"
export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-5G}"
# sccache v0.17.0 does not use SCCACHE_BASEDIRS for rustc; retain this export
# for sccache's non-Rust compatibility. Rust path normalization is performed by
# the STABLE_SRC/STABLE_TGT rewrite above when the stable mounts are available.
export SCCACHE_BASEDIRS="${SCCACHE_BASEDIRS:-$STABLE_SRC:$STABLE_TGT:$repo_root:$target_real}"

# Compile in the client process so a daemon in another mount namespace cannot
# write artifacts through that namespace's /tmp/orbit-workspace bind. Honor an
# explicit operator override (including SCCACHE_CLIENT_SIDE=0 for reproducing
# the shared-daemon missing-artifact failure).
if [[ -z "${SCCACHE_CLIENT_SIDE+x}" ]]; then
  export SCCACHE_CLIENT_SIDE=1
fi
# Private /tmp UDS unless the operator picked a TCP port or socket explicitly.
if [[ -z "${SCCACHE_SERVER_UDS:-}" && -z "${SCCACHE_SERVER_PORT:-}" ]]; then
  export SCCACHE_SERVER_UDS="$DEFAULT_SERVER_UDS"
fi

debug "enabled bin=$cache_bin dir=$cache_dir client_side=${SCCACHE_CLIENT_SIDE-} uds=${SCCACHE_SERVER_UDS-}"
exec "$cache_bin" "$@"
