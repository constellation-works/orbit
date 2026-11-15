#!/usr/bin/env bash
# C compiler stand-in for `cargo check --target aarch64-apple-darwin` on a
# Linux runner. cargo check still runs build scripts, and cc emits Darwin
# flags (`-arch`, `-mmacosx-version-min`) that the host gcc cannot consume.
# For a macOS target, a GNU-family compiler (this name must not contain
# "clang" or "zig", or end in "cl") does not need a Darwin SDK. cargo check
# does not link, so an empty object is enough. This does not compile C;
# Rust `cfg(target_os = "macos")` is what the check type-checks.
# `-?` fails so cc does not treat the stub as an MSVC driver.
# Kept to macOS Bash 3.2 syntax like the other guardrail scripts.
set -euo pipefail

if [[ "${1:-}" == "-E" ]]; then
  exit 0
fi
if [[ "${1:-}" == "-?" ]]; then
  exit 1
fi

out=""
prev=""
for arg in "$@"; do
  if [[ "$prev" == "-o" ]]; then
    out=$arg
  fi
  case "$arg" in
    -o*)
      if [[ "$arg" != "-o" ]]; then
        out=${arg#-o}
      fi
      ;;
  esac
  prev=$arg
done
if [[ -n "$out" && "$out" != "-" ]]; then
  mkdir -p "$(dirname "$out")"
  : >"$out"
fi
exit 0
