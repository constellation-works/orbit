#!/usr/bin/env bash
set -euo pipefail

# Crate-level agent guides [ORB-11046].
#
# A crate guide is one canonical `CLAUDE.md` plus an `AGENTS.md` *relative*
# symlink to it, so both provider conventions load the same text and the two
# can never drift. This script keeps that shape honest and keeps the guides
# from rotting into stale path references:
#
#   1. Every crate in REQUIRED_GUIDE_CRATES has a non-empty CLAUDE.md.
#   2. Every crates/*/AGENTS.md is a symlink whose literal target is exactly
#      "CLAUDE.md" (relative, sibling) and resolves to a regular file.
#   3. Every relative Markdown link inside a crate guide resolves on disk.
#
# Runs in `make ci-fast` (local) and scripts/ci-guardrails.sh (CI).

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

for crate in "${REQUIRED_GUIDE_CRATES[@]}"; do
  guide="crates/$crate/CLAUDE.md"
  link="crates/$crate/AGENTS.md"

  if [[ ! -f "$guide" ]]; then
    echo "crate-agent-guides: missing $guide"
    fail=1
    continue
  fi
  if [[ ! -s "$guide" ]]; then
    echo "crate-agent-guides: $guide is empty"
    fail=1
  fi
  if [[ ! -L "$link" ]]; then
    echo "crate-agent-guides: $link must exist and be a symlink to CLAUDE.md"
    fail=1
  fi
done

# Any AGENTS.md under crates/ — required or not — must be the sibling symlink.
while IFS= read -r link; do
  target="$(readlink "$link" 2>/dev/null || true)"
  if [[ -z "$target" ]]; then
    echo "crate-agent-guides: $link is a regular file; it must be a symlink to the sibling CLAUDE.md"
    fail=1
    continue
  fi
  if [[ "$target" != "CLAUDE.md" ]]; then
    echo "crate-agent-guides: $link points at '$target'; it must point at the sibling 'CLAUDE.md'"
    fail=1
    continue
  fi
  if [[ ! -f "$link" ]]; then
    echo "crate-agent-guides: $link does not resolve to a readable file"
    fail=1
  fi
done < <(find crates -mindepth 2 -maxdepth 2 -name AGENTS.md | sort)

# Relative links inside each guide must resolve, so an example module renamed
# out from under a guide fails here instead of misleading the next agent.
while IFS= read -r guide; do
  crate_dir="$(dirname "$guide")"
  while IFS= read -r target; do
    [[ -z "$target" ]] && continue
    case "$target" in
      \#*|http://*|https://*|mailto:*) continue ;;
    esac
    path="${target%%#*}"
    [[ -z "$path" ]] && continue
    if [[ ! -e "$crate_dir/$path" ]]; then
      echo "crate-agent-guides: $guide links to '$target', which does not resolve"
      fail=1
    fi
  done < <(rg -o '\]\(([^)]+)\)' --replace '$1' --no-line-number --no-filename "$guide" || true)
done < <(find crates -mindepth 2 -maxdepth 2 -name CLAUDE.md | sort)

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "crate agent guide guard passed"
