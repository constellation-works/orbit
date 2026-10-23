#!/usr/bin/env bash
# Runs a command with the lint environment of the folded CI test build.
#
# The workspace test build (`check-ci-macos.sh --workspace-build`) compiles
# workspace members through clippy-driver with the all-targets lint flags of
# `cargo clippy --workspace --all-targets -- -D warnings -A
# clippy::disallowed_methods` (Makefile `ci-lint`). `-A
# clippy::disallowed_methods` keeps test-only channels exempt; production
# clippy still denies them.
#
# Cargo fingerprints RUSTC_WORKSPACE_WRAPPER and the CLIPPY_ARGS value that
# clippy-driver tracks. A later nextest run, doctest build or nextest listing
# reuses those artifacts only under this exact environment; without it Cargo
# recompiles every workspace member. Run each of them through this script.
#
# clippy-driver does not shell-split CLIPPY_ARGS. `cargo clippy` joins each
# argument with this sentinel, and the driver splits on it. A space-separated
# value is one rustc argument and does not apply the flags.
set -euo pipefail

export RUSTC_WORKSPACE_WRAPPER=clippy-driver
export CLIPPY_ARGS='-D__CLIPPY_HACKERY__warnings__CLIPPY_HACKERY__-A__CLIPPY_HACKERY__clippy::disallowed_methods__CLIPPY_HACKERY__'
exec "$@"
