#!/usr/bin/env bash
# Runs a command with the lint environment of the folded CI test build.
#
# The workspace test build (`check-ci-macos.sh --workspace-build`) compiles
# workspace members through clippy-driver. The crate-root disallowed-method
# denies override the test-only command-line relaxation in production.
# --check and --check-json enforce the warning gate on Cargo's JSON output.
# Plain mode lets nextest reuse the artifacts the checked build produced.
#
# Cargo fingerprints RUSTC_WORKSPACE_WRAPPER and the CLIPPY_ARGS value that
# clippy-driver tracks. A later nextest run or nextest listing
# reuses those artifacts only under this exact environment; without it Cargo
# recompiles every workspace member. Run each of them through this script.
#
# clippy-driver does not shell-split CLIPPY_ARGS. `cargo clippy` joins each
# argument with this sentinel, and the driver splits on it. A space-separated
# value is one rustc argument and does not apply the flags.
set -euo pipefail

export RUSTC_WORKSPACE_WRAPPER=clippy-driver
export CLIPPY_ARGS='-A__CLIPPY_HACKERY__clippy::disallowed_methods__CLIPPY_HACKERY__'
if [[ "${1:-}" == --check || "${1:-}" == --check-json ]]; then
  checker="$(dirname "${BASH_SOURCE[0]}")/check-rust-source-order.py"
  options=()
  if [[ "$1" == --check-json ]]; then options+=(--json); fi
  shift
  # Bash 3.2 (macOS /bin/bash) treats an empty array as unbound under `set -u`.
  exec python3 "$checker" ${options[@]+"${options[@]}"} -- "$@"
fi
if [ "$#" -eq 0 ]; then
  echo "with-test-build-lint: no command given" >&2
  exit 2
fi
exec "$@"
