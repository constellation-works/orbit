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

# Fail closed: ripgrep exits 2 on a missing search root, which `|| true`
# would swallow into an empty result and a silent pass. Assert the search
# root exists before scanning. (The allowlist is matched against hit paths,
# never scanned, so a stale entry cannot open a bypass and is not asserted.)
if [[ ! -e "$cli_src" ]]; then
  echo "terminal state guard: missing search root: $cli_src" >&2
  exit 1
fi

failed=0
rg_tmp_out="$(mktemp)"
rg_tmp_err="$(mktemp)"
for pattern in "${patterns[@]}"; do
  # Capture ripgrep's status per pattern: 0 carries matches, 1 means no
  # match, anything else is a scan error and must fail closed.
  set +e
  rg --no-heading --line-number "$pattern" --glob '*.rs' "$cli_src" >"$rg_tmp_out" 2>"$rg_tmp_err"
  rg_status=$?
  set -e
  if [[ "$rg_status" -eq 1 ]]; then
    continue
  elif [[ "$rg_status" -ne 0 ]]; then
    echo "terminal state guard: ripgrep failed (exit $rg_status) for pattern $pattern" >&2
    cat "$rg_tmp_err" >&2
    failed=1
    continue
  fi
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
  done <"$rg_tmp_out"
done
rm -f "$rg_tmp_out" "$rg_tmp_err"

if [[ "$failed" -ne 0 ]]; then
  echo "resolve it once in crates/orbit-cli/src/output/sink.rs and read it from there" >&2
  exit 1
fi

echo "orbit-cli terminal state guard passed"
