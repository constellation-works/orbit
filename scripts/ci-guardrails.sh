#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

fast=false
case "${1:-}" in
  "") ;;
  --fast) fast=true ;;
  *)
    echo "usage: ci-guardrails.sh [--fast]" >&2
    exit 2
    ;;
esac

# Several guardrail scripts shell out to ripgrep; a missing `rg` degrades
# them silently (empty search results read as "clean" or, worse, as false
# violations). Fail fast instead. [ORB-10021]
if ! command -v rg >/dev/null 2>&1; then
  echo "ci-guardrails: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

cargo fmt --all -- --check
if [[ "$fast" == false ]]; then
  # Enumerating workflow tests compiles their targets; keep it out of ci-fast.
  "$repo_root/scripts/check-ci-macos.sh"
  cargo clippy --workspace --all-targets -- -D warnings
  if cargo nextest --version >/dev/null 2>&1; then
    cargo nextest run --no-fail-fast --workspace --lib --bins --tests
  else
    echo "cargo-nextest not found; falling back to cargo test" >&2
    cargo test --no-fail-fast --workspace --lib --bins --tests
  fi
  cargo test --no-fail-fast --workspace --doc
  RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
  # Supply-chain gate: dependency advisories + license allow-list (deny.toml).
  # [ORB-00416] [ORB-11983] Soft-presence like nextest above — CI installs a pinned version;
  # local runs without the tool warn instead of hard-failing.
  if command -v cargo-deny >/dev/null 2>&1; then
    "$repo_root/scripts/cargo-deny.sh" check
  else
    echo "cargo-deny not found; skipping supply-chain gate (install: cargo install cargo-deny --locked)" >&2
  fi
fi

"$repo_root/scripts/generate-doc-indexes.sh" --check
"$repo_root/scripts/check-installer-pubkey.sh"
"$repo_root/scripts/test-installer-security.sh"
"$repo_root/scripts/test-mcp-registry-publish-workflow.sh"
"$repo_root/scripts/check-dependency-direction.sh"
"$repo_root/scripts/test-ci-fast-guards.py"
"$repo_root/scripts/test-codeql-extension-schema.py"
"$repo_root/scripts/check-codeql-extension-schema.py"
"$repo_root/scripts/check-cli-imports.sh"
"$repo_root/scripts/check-terminal-state-guard.sh"
"$repo_root/scripts/check-history-note-size.sh"
"$repo_root/scripts/check-stability.sh"
"$repo_root/scripts/check-artifact-redaction-guardrail.sh"
"$repo_root/scripts/check-ci-failure-reporting.sh"
"$repo_root/scripts/check-public-artifact-ids.py"
"$repo_root/scripts/check-changelog-style.sh"
"$repo_root/scripts/check-error-translation.sh"
"$repo_root/scripts/check-orphan-modules.sh"
"$repo_root/scripts/check-crate-agent-guides.sh"
"$repo_root/scripts/check-embedded-asset-portability.py"
"$repo_root/scripts/test-qa-full-sweep.py" --check
"$repo_root/scripts/sync-activity-assets.sh" --check
"$repo_root/scripts/sync-plugin-skills.sh" --check
"$repo_root/scripts/test-validate-codex-plugin.sh"
"$repo_root/scripts/test-validate-agent-plugin.sh"
"$repo_root/scripts/test-cursor-marketplace-followup.sh"
"$repo_root/scripts/smoke-plugin-install.sh"
"$repo_root/scripts/test-build-budget.sh"
"$repo_root/scripts/test-compiler-cache.sh"
"$repo_root/scripts/test-compiler-cache-namespaces.sh"
"$repo_root/scripts/test-cross-revision-check.sh"
