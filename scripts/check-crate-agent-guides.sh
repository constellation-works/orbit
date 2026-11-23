#!/usr/bin/env bash
set -euo pipefail

# Agent instruction files [ORB-11046, STD-10].
#
# A guide is one regular `AGENTS.md` per directory, plus a regular `CLAUDE.md`
# whose first line imports it (`@AGENTS.md`), so both provider conventions load
# the same text and the two can never drift (STD-10 R1). This script keeps that shape honest, keeps the
# guides inside their size budget and keeps them from rotting into stale path
# references:
#
#   1. The root and every crate in REQUIRED_GUIDE_CRATES has a non-empty
#      AGENTS.md.
#   2. Every AGENTS.md at the root and under crates/ is a regular file, and
#      each CLAUDE.md beside it is a regular file whose first line is exactly
#      "@AGENTS.md".
#   3. The root AGENTS.md plus its CLAUDE.md is at most 8000 bytes and a
#      crate's at most 4000 (STD-10 R10).
#   4. Every relative Markdown link inside a guide resolves on disk.
#
# Runs in `make ci-fast` (local) and scripts/ci-guardrails.sh (CI).

if ! command -v rg >/dev/null 2>&1; then
  echo "crate-agent-guides: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

REQUIRED_GUIDE_CRATES=(
  orbit-cli
  orbit-cmd
  orbit-common
  orbit-core
  orbit-engine
  orbit-mcp
  orbit-runtime
  orbit-store
  orbit-types
)

fail=0

guides=(AGENTS.md)
for crate in "${REQUIRED_GUIDE_CRATES[@]}"; do
  guide="crates/$crate/AGENTS.md"
  if [[ ! -f "$guide" || -L "$guide" ]]; then
    echo "crate-agent-guides: $guide must exist as a regular file"
    fail=1
  elif [[ ! -s "$guide" ]]; then
    echo "crate-agent-guides: $guide is empty"
    fail=1
  fi
done
if [[ ! -f AGENTS.md || -L AGENTS.md ]]; then
  echo "crate-agent-guides: AGENTS.md must exist as a regular file at the repository root"
  fail=1
elif [[ ! -s AGENTS.md ]]; then
  echo "crate-agent-guides: AGENTS.md is empty; the root guide must hold the shared workspace rules"
  fail=1
fi

# Any AGENTS.md at the root or under crates/ — required or not — is a regular
# file, its CLAUDE.md imports it, and the two fit the size budget.
while IFS= read -r guide; do
  [[ "$guide" == AGENTS.md ]] || guides+=("$guide")
  dir="$(dirname "$guide")"
  if [[ -L "$guide" ]]; then
    echo "crate-agent-guides: $guide is a symlink; AGENTS.md must be the regular file"
    fail=1
    continue
  fi
  limit=4000
  [[ "$dir" == . ]] && limit=8000
  alias_path="$dir/CLAUDE.md"
  size="$(wc -c < "$guide" | tr -d ' ')"
  if [[ -L "$alias_path" || ! -f "$alias_path" ]]; then
    echo "crate-agent-guides: $alias_path must be a regular file whose first line is '@AGENTS.md'"
    fail=1
  else
    first_line="$(head -n 1 "$alias_path")"
    if [[ "$first_line" != "@AGENTS.md" ]]; then
      echo "crate-agent-guides: $alias_path must start with the line '@AGENTS.md'"
      fail=1
    fi
    size=$(( size + $(wc -c < "$alias_path" | tr -d ' ') ))
  fi
  if (( size > limit )); then
    echo "crate-agent-guides: $guide and its CLAUDE.md are $size bytes; the budget is $limit"
    fail=1
  fi
done < <({ echo AGENTS.md; find crates -mindepth 2 -maxdepth 2 -name AGENTS.md; } | sort -u)

# Relative links inside each guide must resolve, so an example module renamed
# out from under a guide fails here instead of misleading the next agent.
for guide in "${guides[@]}"; do
  [[ -f "$guide" ]] || continue
  guide_dir="$(dirname "$guide")"
  while IFS= read -r target; do
    [[ -z "$target" ]] && continue
    case "$target" in
      \#*|http://*|https://*|mailto:*) continue ;;
    esac
    path="${target%%#*}"
    [[ -z "$path" ]] && continue
    if [[ ! -e "$guide_dir/$path" ]]; then
      echo "crate-agent-guides: $guide links to '$target', which does not resolve"
      fail=1
    fi
  done < <(rg -o '\]\(([^)]+)\)' --replace '$1' --no-line-number --no-filename "$guide" || true)
done

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "crate agent guide guard passed"
