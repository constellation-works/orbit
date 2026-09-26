#!/usr/bin/env bash
set -euo pipefail

# The output sink is the only place in orbit-cli that asks whether stdout is a
# terminal, how wide it is, or whether color is permitted
# (docs/design/terminal-interface/specs/output-modes.md §1, ADR-0306). A
# command that re-derives any of it renders differently from the sink that
# resolved the invocation, which is the drift the sink exists to remove.
#
# There is no longer a grandfathered exception: command/log/tail.rs used to
# colorize a streamed tail from a local `is_terminal()` check. It now receives
# the resolved sink through `Execute::execute` and uses `sink.color_allowed()`.
# Nothing may be added to the allowlist without that being a decision.

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! command -v rg >/dev/null 2>&1; then
  echo "check-terminal-state-guard: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

cli_src="crates/orbit-cli/src"

allowed=(
  "$cli_src/output/sink.rs"
  "$cli_src/output/tests/sink.rs"
  # Asserts the gate the sink resolves, so it names the variables on purpose.
  "$cli_src/output/tests/gating.rs"
)

# `IsTerminal` covers `x.is_terminal()` too: the trait must be imported by name
# to call it. A bare `is_terminal()` is therefore not matched on purpose —
# `JobRunState::is_terminal` is an unrelated domain predicate.
patterns=(
  '\bIsTerminal\b'
  '\bTIOCGWINSZ\b'
  '\bNO_COLOR\b'
  '\bCLICOLOR'
  '"COLUMNS"'
)

failed=0
for pattern in "${patterns[@]}"; do
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    file="${hit%%:*}"
    # Naming a variable in prose is not querying it. Doc comments have to be
    # able to say which variables the sink reads and why a call site stopped
    # reading them, or the rule cannot be explained where it is enforced.
    code="${hit#*:*:}"
    if [[ "${code#"${code%%[![:space:]]*}"}" == //* ]]; then
      continue
    fi
    permitted=0
    for allow in "${allowed[@]}"; do
      if [[ "$file" == "$allow" ]]; then
        permitted=1
        break
      fi
    done
    if [[ "$permitted" -eq 0 ]]; then
      echo "terminal state queried outside the output sink: $hit"
      failed=1
    fi
  done < <(rg --no-heading --line-number "$pattern" --glob '*.rs' "$cli_src" || true)
done

if [[ "$failed" -ne 0 ]]; then
  echo "resolve it once in crates/orbit-cli/src/output/sink.rs and read it from there" >&2
  exit 1
fi

echo "orbit-cli terminal state guard passed"
