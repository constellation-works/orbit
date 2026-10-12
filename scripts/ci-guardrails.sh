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

# The guardrails below import `tomllib` (Python >= 3.11); stop before any gate
# runs rather than die in a traceback partway through.
"$repo_root/scripts/require-python.sh"

# --fast runs formatting and script guardrails, never the Rust test suite.
# `make ci-test-affected` supplies the separate pre-handoff test gate.
cargo fmt --all -- --check
if [[ "$fast" == false ]]; then
  # Enumerating workflow tests compiles their targets; keep it out of ci-fast.
  # --workspace-build lists them from the same `--workspace --lib --bins
  # --tests` artifacts the nextest pass below runs, instead of a per-crate
  # `cargo test -p` build that resolves features differently and recompiles
  # the workspace chain a second time. [DANI-10428]
  "$repo_root/scripts/check-ci-macos.sh" --workspace-build
  # Production must reject unbounded channels. Existing test-only channels
  # remain exempt in the all-targets pass.
  cargo clippy --workspace --lib --bins -- -D warnings -D clippy::disallowed_methods
  cargo clippy --workspace --all-targets -- -D warnings -A clippy::disallowed_methods
  if cargo nextest --version >/dev/null 2>&1; then
    cargo nextest run --no-fail-fast \
      --success-output immediate \
      --workspace --lib --bins --tests
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

# Keep all runtime output within the checkout, including on macOS. Separate
# streams prevent interleaved diagnostics while guards execute concurrently.
mkdir -p "$repo_root/.orbit/tmp"
guard_logs="$(mktemp -d "$repo_root/.orbit/tmp/ci-guardrails.XXXXXX")"
trap 'python3 -c "import shutil, sys; shutil.rmtree(sys.argv[1])" "$guard_logs"' EXIT
export GIT_OPTIONAL_LOCKS=0

run_self_tests=true
if [[ "$fast" == true ]]; then
  # Match ci-test-affected's delivery-base override and merge-base fallback.
  base=""
  if [[ -n "${CI_TEST_BASE:-}" ]]; then
    base="$(git rev-parse --verify "${CI_TEST_BASE}^{commit}" 2>/dev/null || true)"
  else
    for branch in refs/remotes/origin/agent-main refs/heads/agent-main; do
      if git rev-parse --verify --quiet "$branch" >/dev/null; then
        base="$(git merge-base HEAD "$branch" 2>/dev/null || true)"
        break
      fi
    done
  fi
  # Include committed, staged, unstaged, deleted and non-ignored untracked
  # inputs, including both sides of moves. A failed query runs every test.
  if [[ -n "$base" ]] &&
    git diff --name-only --no-renames -z "$base" -- >"$guard_logs/paths" &&
    git ls-files --others --exclude-standard -z >>"$guard_logs/paths"; then
    run_self_tests=false
    while IFS= read -r -d '' path; do
      case "$path" in
        scripts/* | .github/* | Makefile | .cargo/* | Cargo.toml | Cargo.lock | \
          install.sh | RELEASING.md | plugin/* | npm/* | .agents/* | .claude-plugin/*)
          run_self_tests=true
          break
          ;;
      esac
    done <"$guard_logs/paths"
  else
    echo "ci-guardrails: cannot resolve changed inputs; running all self-tests" >&2
  fi
fi

pids=()
names=()
slots=()
next_slot=0
pending=0
failed=0
finish_guards() {
  local index status
  for ((index=0; index<pending; index++)); do
    status=0
    wait "${pids[$index]}" || status=$?
    cat "$guard_logs/${slots[$index]}.out"
    cat "$guard_logs/${slots[$index]}.err" >&2
    if [[ "$status" -ne 0 ]]; then
      echo "ci-guardrails: FAILED ${names[$index]} (exit $status)" >&2
      # Preserve the first failure's exit status for existing callers.
      if [[ "$failed" -eq 0 ]]; then failed="$status"; fi
    fi
  done
  pids=()
  names=()
  slots=()
  pending=0
}

run_guard() {
  local name pid
  name="${1##*/}"
  # The QA sweep's --check is also a live CLI/MCP/web inventory check; it
  # must still run for crates-only changes, unlike fixture-only self-tests.
  if [[ "$name" == test-* && "$name" != test-qa-full-sweep.py ]]; then
    if [[ "$run_self_tests" == false ]]; then
      echo "ci-guardrails: SKIPPED $name (self-test inputs unchanged)"
      return
    fi
    # Self-tests may compile miniature fixtures or test build admission.
    # Run them one at a time, after the independent repository checks.
    finish_guards
  fi
  # bash's time keyword supplies fractional wall time on Linux and bash 3.2.
  # Give each invocation its own output paths, including between batches.
  local slot="$next_slot"
  next_slot=$((next_slot + 1))
  (
    TIMEFORMAT="ci-guardrails: $name %3R s"
    time { "$@"; }
  ) >"$guard_logs/$slot.out" 2>"$guard_logs/$slot.err" &
  pid=$!
  pids+=("$pid")
  names+=("$name")
  slots+=("$slot")
  pending=$((pending + 1))
  if [[ "$pending" -ge 8 || "$name" == test-* && "$name" != test-qa-full-sweep.py ]]; then
    finish_guards
  fi
}

# Start the longer repository scans together, then drain shorter batches.
run_guard "$repo_root/scripts/check-orphan-modules.sh"
run_guard "$repo_root/scripts/check-workflow-action-pins.sh"
run_guard "$repo_root/scripts/test-qa-full-sweep.py" --check
run_guard "$repo_root/scripts/check-error-translation.sh"
run_guard "$repo_root/scripts/check-unused-dependencies.py"
run_guard "$repo_root/scripts/check-doc-links.py"
run_guard "$repo_root/scripts/check-cli-imports.sh"
run_guard "$repo_root/scripts/unit-test-inventory.py" --check "$repo_root/scripts/unit-test-baseline.json"
run_guard "$repo_root/scripts/generate-doc-indexes.sh" --check
run_guard "$repo_root/scripts/check-installer-pubkey.sh"
run_guard "$repo_root/scripts/check-dependency-direction.sh"
run_guard "$repo_root/scripts/check-workflow-yaml.py"
run_guard "$repo_root/scripts/check-test-shell-waits.py"
run_guard "$repo_root/scripts/check-codeql-production-paths.py"
run_guard "$repo_root/scripts/check-codeql-extension-schema.py"
run_guard "$repo_root/scripts/check-terminal-state-guard.sh"
run_guard "$repo_root/scripts/check-durability-syncs.py"
run_guard "$repo_root/scripts/check-windows-portability.py"
run_guard "$repo_root/scripts/check-history-note-size.sh"
run_guard "$repo_root/scripts/check-stability.sh"
run_guard "$repo_root/scripts/check-artifact-redaction-guardrail.sh"
run_guard "$repo_root/scripts/check-web-blocking-handlers.py"
run_guard "$repo_root/scripts/check-ci-failure-reporting.sh"
run_guard "$repo_root/scripts/check-public-artifact-ids.py"
run_guard "$repo_root/scripts/check-changelog-style.sh"
run_guard "$repo_root/scripts/check-crate-agent-guides.sh"
run_guard "$repo_root/scripts/check-embedded-asset-portability.py"
run_guard "$repo_root/scripts/check-desktop-ui.sh"
run_guard "$repo_root/scripts/check-dashboard-vendor.py"
run_guard "$repo_root/scripts/sync-plugin-skills.sh" --check
run_guard "$repo_root/scripts/smoke-plugin-install.sh"
finish_guards

# Fixture self-tests: full CI always runs these, fast mode selects by inputs.
run_guard "$repo_root/scripts/test-installer-security.sh"
run_guard "$repo_root/scripts/test-mcp-registry-publish-workflow.sh"
run_guard "$repo_root/scripts/test-check-unused-dependencies.py"
run_guard "$repo_root/scripts/test-release-workflow.py"
run_guard "$repo_root/scripts/test-release-promotion.py"
run_guard "$repo_root/scripts/test-pr-state-workflows.py"
run_guard "$repo_root/scripts/test-website-workflow.py"
run_guard "$repo_root/scripts/test-ci-fast-guards.py"
run_guard "$repo_root/scripts/test-windows-portability.py"
run_guard "$repo_root/scripts/test-refresh-dashboard-vendor.py"
run_guard "$repo_root/scripts/test-unit-test-inventory.py"
run_guard "$repo_root/scripts/test-ci-test-affected.py"
run_guard "$repo_root/scripts/test-codeql-extension-schema.py"
run_guard "$repo_root/scripts/test-codeql-rust-local.py"
run_guard "$repo_root/scripts/test-codeql-production-paths.py"
run_guard "$repo_root/scripts/test-doc-duty-rotation.py"
run_guard "$repo_root/scripts/test-validate-codex-plugin.sh"
run_guard "$repo_root/scripts/test-validate-agent-plugin.sh"
run_guard "$repo_root/scripts/test-build-budget.sh"
run_guard "$repo_root/scripts/test-make-install.py"
run_guard "$repo_root/scripts/test-compiler-cache.sh"
run_guard "$repo_root/scripts/test-compiler-cache-namespaces.sh"
run_guard "$repo_root/scripts/test-cross-revision-check.sh"
finish_guards
exit "$failed"
