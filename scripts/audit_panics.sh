#!/usr/bin/env bash
# Inventory `.unwrap()` / `.expect(...)` / `panic!(...)` / `unreachable!(...)` /
# `unimplemented!(...)` / `todo!(...)` sites across the workspace.
#
# Buckets:
#   total         — every match in workspace member `src/**/*.rs`
#   in_cfg_test   — same, restricted to lines inside `#[cfg(test)]` blocks
#                   (or files whose path looks test-only)
#   non_test      — every match outside those scopes
#   blast_radius  — non-test matches in execution-critical crates
#                   (orbit-engine, orbit-agent, orbit-tools, orbit-exec) src/,
#                   excluding `_tests.rs`, `/tests/`, `test_support`, `/fixtures/`.
#
# Usage:
#   scripts/audit_panics.sh           # human-readable table
#   scripts/audit_panics.sh --json    # machine-readable JSON
#
# Members and package names come from Cargo.toml (Python >= 3.11).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

"$repo_root/scripts/require-python.sh"
exec python3 "$repo_root/scripts/audit_panics.py" "$@"
