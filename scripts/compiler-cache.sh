#!/usr/bin/env bash
# Host compiler-cache operator surface [ORB-11259].
#
# Owns $HOME/.orbit/cache/compiler (language-neutral host cache seam).
# Does not share CARGO_TARGET_DIR, does not touch live workers, and does
# not apt-install packages.
#
# Usage:
#   scripts/compiler-cache.sh status
#   scripts/compiler-cache.sh setup [--install]
#   scripts/compiler-cache.sh remove [--yes]
set -euo pipefail

SCCACHE_VERSION="v0.17.0"
DEFAULT_SIZE="5G"

usage() {
  cat <<'EOF'
Usage: scripts/compiler-cache.sh <status|setup|remove> [--install|--yes]

status   Show whether the rustc wrapper would enable sccache, and cache stats.
setup    Create $HOME/.orbit/cache/compiler. With --install, verify a pinned
         sccache archive and install into $HOME/.orbit/cache/bin/.
remove   Delete the compiler cache directory. Refuses unless --yes is passed.
EOF
}

die() {
  printf 'compiler-cache: %s\n' "$*" >&2
  exit 1
}

require_home() {
  [[ -n "${HOME:-}" ]] || die "HOME is unset"
}

cache_root() {
  require_home
  printf '%s\n' "$HOME/.orbit/cache"
}

compiler_dir() {
  printf '%s/compiler\n' "$(cache_root)"
}

cache_bin_dir() {
  printf '%s/bin\n' "$(cache_root)"
}

resolve_sccache() {
  if [[ -n "${ORBIT_COMPILER_CACHE_BIN:-}" && -x "${ORBIT_COMPILER_CACHE_BIN}" ]]; then
    printf '%s\n' "${ORBIT_COMPILER_CACHE_BIN}"
    return 0
  fi
  local bundled
  bundled="$(cache_bin_dir)/sccache"
  if [[ -x "$bundled" ]]; then
    printf '%s\n' "$bundled"
    return 0
  fi
  if command -v sccache >/dev/null 2>&1; then
    command -v sccache
    return 0
  fi
  return 1
}

sccache_asset() {
  local os arch target checksum
  os="$(uname -s)"
  arch="$(uname -m)"
  # SHA-256 digests from the v0.17.0 release assets; update with SCCACHE_VERSION.
  # https://api.github.com/repos/mozilla/sccache/releases/tags/v0.17.0
  case "${os}/${arch}" in
    Linux/x86_64 | Linux/amd64)
      target="x86_64-unknown-linux-musl"
      checksum="67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006"
      ;;
    Linux/aarch64 | Linux/arm64)
      target="aarch64-unknown-linux-musl"
      checksum="821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77"
      ;;
    Darwin/x86_64 | Darwin/amd64)
      target="x86_64-apple-darwin"
      checksum="c2144cafbfe3d22e34ae637f9974ce53613543ac19477fdb287df22ea3668261"
      ;;
    Darwin/arm64 | Darwin/aarch64)
      target="aarch64-apple-darwin"
      checksum="0c560bfba31aef5bdfb4fb3d2677f6e61d71c5c00952f2a83344f47aa31f00f1"
      ;;
    *)
      return 1
      ;;
  esac
  printf 'sccache-%s-%s.tar.gz %s\n' "$SCCACHE_VERSION" "$target" "$checksum"
}

cmd_status() {
  local dir wrapper rustc_wrapper cache_bin
  dir="$(compiler_dir)"
  wrapper="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/scripts/rustc-compiler-cache.sh"
  rustc_wrapper="${RUSTC_WRAPPER:-}"
  printf 'compiler-cache status\n'
  printf '  cache_dir:          %s\n' "$dir"
  printf '  cache_dir_exists:   %s\n' "$([[ -d "$dir" ]] && echo yes || echo no)"
  printf '  cache_dir_writable: %s\n' "$([[ -d "$dir" && -w "$dir" ]] && echo yes || echo no)"
  printf '  wrapper:            %s\n' "$wrapper"
  printf '  RUSTC_WRAPPER:      %s\n' "${rustc_wrapper:-<unset>}"
  printf '  SCCACHE_DIR:        %s\n' "${SCCACHE_DIR:-<unset, wrapper default $dir>}"
  printf '  SCCACHE_CACHE_SIZE: %s\n' "${SCCACHE_CACHE_SIZE:-<unset, wrapper default $DEFAULT_SIZE>}"
  printf '  ORBIT_COMPILER_CACHE=%s\n' "${ORBIT_COMPILER_CACHE:-<unset>}"
  printf '  SCCACHE_CLIENT_SIDE:  %s\n' "${SCCACHE_CLIENT_SIDE:-<unset, wrapper default 1>}"
  printf '  SCCACHE_SERVER_UDS:   %s\n' "${SCCACHE_SERVER_UDS:-<unset, wrapper default /tmp/orbit-sccache.sock>}"
  printf '  SCCACHE_SERVER_PORT:  %s\n' "${SCCACHE_SERVER_PORT:-<unset>}"
  if cache_bin="$(resolve_sccache)"; then
    printf '  sccache:            %s\n' "$cache_bin"
    printf '  sccache_version:    %s\n' "$("$cache_bin" --version 2>/dev/null || echo unknown)"
    if [[ -d "$dir" && -w "$dir" && "${ORBIT_COMPILER_CACHE:-}" != "0" ]]; then
      printf '  effective:          enabled (wrapper will exec sccache)\n'
    else
      printf '  effective:          sccache present but cache not usable; wrapper falls back to rustc\n'
    fi
    SCCACHE_DIR="${SCCACHE_DIR:-$dir}" "$cache_bin" --show-stats 2>/dev/null || true
  else
    printf '  sccache:            not found\n'
    printf '  effective:          disabled (ordinary rustc)\n'
  fi
}

install_sccache() (
  local bindir asset_info asset checksum actual url tmp dest member
  bindir="$(cache_bin_dir)"
  dest="$bindir/sccache"
  asset_info="$(sccache_asset)" || die "no pinned sccache asset for $(uname -s)/$(uname -m)"
  read -r asset checksum <<< "$asset_info"
  url="https://github.com/mozilla/sccache/releases/download/${SCCACHE_VERSION}/${asset}"
  tmp="$(mktemp -d)"
  # A subshell-scoped EXIT trap also runs when errexit aborts curl, hashing or tar.
  trap 'rm -rf "$tmp"' EXIT
  printf 'compiler-cache: downloading %s\n' "$url"
  curl -fsSL "$url" -o "$tmp/$asset"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$asset")"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$asset")"
  else
    die "SHA-256 verification requires sha256sum or shasum"
  fi
  [[ "${actual%% *}" == "$checksum" ]] || die "SHA-256 mismatch for $asset"
  member="${asset%.tar.gz}/sccache"
  tar -xzf "$tmp/$asset" -C "$tmp" "$member"
  [[ -f "$tmp/$member" && ! -L "$tmp/$member" ]] || die "extracted sccache binary missing or not a regular file"
  mkdir -p "$bindir"
  cp "$tmp/$member" "$dest"
  chmod +x "$dest"
  "$dest" --version
)

cmd_setup() {
  local install=0
  local arg
  for arg in "$@"; do
    case "$arg" in
      --install) install=1 ;;
      *) die "unknown setup flag: $arg" ;;
    esac
  done
  local dir
  dir="$(compiler_dir)"
  if ! mkdir -p "$dir" 2>/dev/null; then
    die "cannot create $dir (read-only or missing grant). Run setup on the host, not inside a sandboxed worker. Linux implementer sandboxes write this path only after the Orbit binary with the host-cache grant is deployed."
  fi
  printf 'compiler-cache: created %s\n' "$dir"
  if [[ "$install" -eq 1 ]]; then
    install_sccache
  elif ! resolve_sccache >/dev/null; then
    printf 'compiler-cache: sccache not on PATH. Re-run with --install to download %s,\n' "$SCCACHE_VERSION"
    printf '  or install sccache yourself (user-level; no apt required).\n'
  fi
  cat <<EOF
compiler-cache: setup complete.

Workers detect effectiveness with:
  make compiler-cache-status
  # or: sccache --show-stats   (after a cargo build)

The committed rustc wrapper enables sccache automatically when:
  - sccache is on PATH or at $HOME/.orbit/cache/bin/sccache
  - $dir is writable (Linux implementer sandbox grants ~/.orbit/cache)

Disable without uninstalling: ORBIT_COMPILER_CACHE=0
Bounded retention: SCCACHE_CACHE_SIZE=${SCCACHE_CACHE_SIZE:-$DEFAULT_SIZE} (sccache LRU)
Remove: scripts/compiler-cache.sh remove --yes
EOF
}

cmd_remove() {
  local yes=0
  local arg
  for arg in "$@"; do
    case "$arg" in
      --yes) yes=1 ;;
      *) die "unknown remove flag: $arg" ;;
    esac
  done
  [[ "$yes" -eq 1 ]] || die "refusing to delete $(compiler_dir) without --yes"
  local dir
  dir="$(compiler_dir)"
  if [[ -e "$dir" ]]; then
    rm -rf "$dir"
    printf 'compiler-cache: removed %s\n' "$dir"
  else
    printf 'compiler-cache: %s already absent\n' "$dir"
  fi
}

main() {
  local cmd="${1:-}"
  shift || true
  case "$cmd" in
    status) cmd_status "$@" ;;
    setup) cmd_setup "$@" ;;
    remove) cmd_remove "$@" ;;
    -h | --help | help | "") usage ;;
    *)
      usage >&2
      die "unknown command: $cmd"
      ;;
  esac
}

main "$@"
