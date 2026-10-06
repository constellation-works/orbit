#!/bin/sh
# Build the static Bubblewrap that Orbit bundles for Linux hosts whose own
# bwrap is missing or lacks --bind-fd (docs/runbooks/linux-sandbox.md).
#
# Usage: scripts/build-bundled-bwrap.sh <output-dir>
#
# The release workflow runs this natively on x86_64 and aarch64 inside the
# digest-pinned Alpine image named in .github/workflows/release.yml, so the
# result is a musl static executable with no runtime library dependency. Both
# sources are pinned by version and SHA-256. A fixed SOURCE_DATE_EPOCH, a
# build-directory prefix map and a stripped link without a build ID keep the
# bytes independent of where and when the build ran; the .buildinfo file
# records the toolchain that produced them.
#
# Writes into <output-dir>:
#   orbit-bwrap-<arch>-linux            the static executable
#   orbit-bwrap-<arch>-linux.buildinfo  pinned inputs, toolchain, digest
#   bubblewrap-<version>.tar.xz         the exact Bubblewrap source built
#   libcap-<version>.tar.xz             the exact libcap source linked in
#   orbit-bwrap-NOTICE.txt              licences of everything linked in
#   orbit-bwrap-build.sh                this recipe
#
# Bubblewrap is LGPL-2.0-or-later; libcap is BSD-3-Clause or GPL-2.0-only.
# The release publishes both source tarballs and this recipe alongside the
# binaries.

set -eu

# Keep BWRAP_VERSION in step with BUNDLED_BWRAP_VERSION in
# crates/orbit-exec/src/linux_sandbox/wrapper.rs. --bind-fd arrived in 0.8.0.
BWRAP_VERSION=0.12.0
BWRAP_SHA256=9760d007363e3abba7c747489910f9f82d9fca53ba3bd3282e396fa3c97a3314
BWRAP_URL="https://github.com/containers/bubblewrap/releases/download/v${BWRAP_VERSION}/bubblewrap-${BWRAP_VERSION}.tar.xz"
LIBCAP_VERSION=2.78
LIBCAP_SHA256=0d621e562fd932ccf67b9660fb018e468a683d7b827541df27813228c996bb11
LIBCAP_URL="https://cdn.kernel.org/pub/linux/libs/security/linux-privs/libcap2/libcap-${LIBCAP_VERSION}.tar.xz"

# A fixed epoch for any tool that stamps a date; the build reads no clock.
SOURCE_DATE_EPOCH=1747008000
export SOURCE_DATE_EPOCH

fail() {
  printf 'build-bundled-bwrap: %s\n' "$*" >&2
  exit 1
}

out="${1:-}"
[ -n "$out" ] || fail "usage: build-bundled-bwrap.sh <output-dir>"
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "no bundled Bubblewrap is published for $(uname -m)" ;;
esac
for tool in cc curl make meson ninja pkg-config readelf sha256sum strip tar xz; do
  command -v "$tool" >/dev/null 2>&1 || fail "required tool not found: $tool"
done

mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT HUP INT TERM

fetch() {
  url="$1"
  sha="$2"
  name="${url##*/}"
  cached="$out/$name"
  if [ -f "$cached" ]; then
    if printf '%s  %s\n' "$sha" "$cached" | sha256sum -c - >/dev/null 2>&1; then
      tar -xJf "$cached" -C "$work"
      return
    fi
    rm -f "$cached"
  fi

  download="$work/$name"
  if ! curl -fsSL "$url" -o "$download"; then
    fail "download failed for $name"
  fi
  printf '%s  %s\n' "$sha" "$download" | sha256sum -c - >/dev/null \
    || fail "checksum mismatch for downloaded $name; expected $sha"
  mv "$download" "$cached"
  tar -xJf "$cached" -C "$work"
}

fetch "$BWRAP_URL" "$BWRAP_SHA256"
fetch "$LIBCAP_URL" "$LIBCAP_SHA256"

cflags="-O2 -ffile-prefix-map=$work=."
stage="$work/stage"

# libcap: the static library and its pkg-config file only — no shared
# objects, Go bindings or PAM module.
make -C "$work/libcap-${LIBCAP_VERSION}/libcap" \
  SHARED=no GOLANG=no PAM_CAP=no USE_GPERF=no \
  CC=cc BUILD_CC=cc COPTS="$cflags" \
  prefix="$stage" lib=lib \
  install-static-cap

PKG_CONFIG_PATH="$stage/lib/pkgconfig" \
  meson setup "$work/build" "$work/bubblewrap-${BWRAP_VERSION}" \
  --buildtype=release \
  --default-library=static \
  -Dprefer_static=true \
  -Dc_args="$cflags" \
  -Dc_link_args="-static -Wl,--build-id=none" \
  -Dselinux=disabled \
  -Dman=disabled \
  -Dtests=false \
  -Dbash_completion=disabled \
  -Dzsh_completion=disabled
ninja -C "$work/build" bwrap

binary="$out/orbit-bwrap-${arch}-linux"
cp "$work/build/bwrap" "$binary"
strip --strip-all "$binary"
chmod 0755 "$binary"

# Refuse a result Orbit would refuse, or that would not run everywhere.
if readelf -d "$binary" 2>/dev/null | grep -q NEEDED; then
  fail "$binary is dynamically linked"
fi
"$binary" --version | grep -qx "bubblewrap ${BWRAP_VERSION}" \
  || fail "$binary does not report bubblewrap ${BWRAP_VERSION}"
"$binary" --help | grep -q -- '--bind-fd' \
  || fail "$binary does not support --bind-fd"

{
  printf 'bubblewrap %s sha256 %s\n' "$BWRAP_VERSION" "$BWRAP_SHA256"
  printf 'libcap %s sha256 %s\n' "$LIBCAP_VERSION" "$LIBCAP_SHA256"
  printf 'SOURCE_DATE_EPOCH %s\n' "$SOURCE_DATE_EPOCH"
  printf 'cc %s\n' "$(cc --version | head -n 1)"
  printf 'meson %s\n' "$(meson --version)"
  printf 'ninja %s\n' "$(ninja --version)"
  if command -v apk >/dev/null 2>&1; then
    printf 'alpine %s\n' "$(cat /etc/alpine-release)"
    apk info -v 2>/dev/null | sort | sed 's/^/apk /'
  fi
  printf 'orbit-bwrap-%s-linux sha256 %s\n' "$arch" "$(sha256sum "$binary" | cut -d ' ' -f 1)"
} > "$binary.buildinfo"

{
  printf 'The orbit-bwrap-*-linux executables are Bubblewrap %s\n' "$BWRAP_VERSION"
  printf '(https://github.com/containers/bubblewrap), statically linked with\n'
  printf 'libcap %s and the C library of the build image named in their\n' "$LIBCAP_VERSION"
  printf '.buildinfo. The exact sources are published with this release as\n'
  printf 'bubblewrap-%s.tar.xz and libcap-%s.tar.xz, and\n' "$BWRAP_VERSION" "$LIBCAP_VERSION"
  printf 'orbit-bwrap-build.sh rebuilds the executables from them.\n'
  printf '\n==== Bubblewrap: LGPL-2.0-or-later ====\n\n'
  cat "$work/bubblewrap-${BWRAP_VERSION}/COPYING"
  printf '\n==== libcap: BSD-3-Clause OR GPL-2.0-only ====\n\n'
  cat "$work/libcap-${LIBCAP_VERSION}/License"
  printf '\n==== C library ====\n\n'
  if command -v apk >/dev/null 2>&1; then
    printf 'musl libc %s (MIT): https://git.musl-libc.org/cgit/musl/tree/COPYRIGHT\n' \
      "$(apk info -v musl 2>/dev/null | head -n 1)"
  else
    printf '%s\n' "$(cc -print-file-name=libc.a)"
  fi
} > "$out/orbit-bwrap-NOTICE.txt"
cp "$0" "$out/orbit-bwrap-build.sh"

printf 'built %s\n' "$binary"
