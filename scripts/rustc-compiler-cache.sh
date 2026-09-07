#!/usr/bin/env bash
# rustc wrapper that uses sccache when a host compiler cache is available.
#
# Cargo invokes this as: rustc-compiler-cache.sh <rustc> [args...]
# It must never write to stdout (that stream is rustc's). Missing or unusable
# cache degrades to ordinary rustc. [ORB-11259]
set -euo pipefail

# Tests may override these aliases to avoid sharing host-global /tmp paths.
# Managed Linux sandboxes use the defaults below.
STABLE_SRC="${ORBIT_COMPILER_CACHE_STABLE_SRC:-/tmp/orbit-workspace}"
STABLE_TGT="${ORBIT_COMPILER_CACHE_STABLE_TGT:-/tmp/orbit-build}"

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
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  target_real="${CARGO_TARGET_DIR}"
else
  target_real="${repo_root}/target"
fi

# Managed Linux sandboxes bind the worktree and its target/ at stable /tmp
# paths so sccache keys do not include the jrun worktree prefix.
rewrite_value() {
  local s="$1"
  if [[ -n "$target_real" && ( "$s" == "$target_real" || "$s" == "$target_real/"* ) ]]; then
    printf '%s%s' "$STABLE_TGT" "${s#"$target_real"}"
    return
  fi
  if [[ "$s" == "$repo_root" || "$s" == "$repo_root/"* ]]; then
    printf '%s%s' "$STABLE_SRC" "${s#"$repo_root"}"
    return
  fi
  printf '%s' "$s"
}

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

if same_inode "$STABLE_SRC/Cargo.toml" "$repo_root/Cargo.toml" && same_inode "$STABLE_TGT" "$target_real"; then
  debug "rewriting paths onto $STABLE_SRC and $STABLE_TGT"
  rewritten=()
  for arg in "$@"; do
    rewritten+=("$(rewrite_value "$arg")")
  done
  set -- "${rewritten[@]}"
  # Bash 3.2 lacks array loading, and macOS env does not provide GNU env -0.
  # compgen emits exported names without touching their values, so indirect
  # expansion keeps spaces, equals signs, newlines, and empty values intact.
  while IFS= read -r name; do
    value="${!name}"
    [[ -n "$name" ]] || continue
    new="$(rewrite_value "$value")"
    if [[ "$new" != "$value" ]]; then
      export "${name}=${new}"
    fi
  done < <(compgen -e)
fi

export SCCACHE_DIR="$cache_dir"
export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-5G}"
# sccache v0.17.0 does not use SCCACHE_BASEDIRS for rustc; retain this export
# for sccache's non-Rust compatibility. Rust path normalization is performed by
# the STABLE_SRC/STABLE_TGT rewrite above when the stable mounts are available.
export SCCACHE_BASEDIRS="${SCCACHE_BASEDIRS:-$STABLE_SRC:$STABLE_TGT:$repo_root:$target_real}"
debug "enabled bin=$cache_bin dir=$cache_dir"
exec "$cache_bin" "$@"
