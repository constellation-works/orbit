#!/usr/bin/env bash
# DESTRUCTIVE: force-removes every worktree except the invoking tree and the
# primary worktree, and force-deletes every local branch except main,
# agent-main and the branches those two kept trees have checked out, with no
# merged/dirty check, confirmation or dry-run. Run only when that
# indiscriminate scope is intended.

set -euo pipefail

# Ensure we're in the invoking tree's root (the primary worktree or a linked one)
cd "$(git rev-parse --show-toplevel)"

echo "Pruning unreachable worktrees..."
git worktree prune

echo "Removing worktrees..."
invoking_path=$(pwd -P)
# The first entry of the worktree list is always the primary worktree.
primary_path=$(git worktree list --porcelain | awk '/^worktree / { print substr($0, 10); exit }')
primary_path=$(cd "$primary_path" 2>/dev/null && pwd -P || printf '%s' "$primary_path")

failures=0
failed_items=()
# Branches checked out in the trees that are kept; git refuses to delete them.
kept_branches=()

while IFS=$'\t' read -r wt_path branch; do
    # Skip the invoking tree and the primary worktree, and keep their branches
    resolved_path=$(cd "$wt_path" 2>/dev/null && pwd -P || printf '%s' "$wt_path")
    if [[ "$resolved_path" == "$invoking_path" || "$resolved_path" == "$primary_path" ]]; then
        if [[ -n "$branch" ]]; then
            kept_branches+=("$branch")
        fi
        continue
    fi

    # Skip if branch is main or agent-main (detached worktrees have no branch)
    if [[ "$branch" == "main" || "$branch" == "agent-main" ]]; then
        continue
    fi

    echo "Removing worktree: $wt_path (branch: ${branch:-detached})"
    if ! git worktree remove --force "$wt_path"; then
        echo "cleanup-branches: failed to remove worktree: $wt_path" >&2
        failed_items+=("worktree: $wt_path")
        failures=$((failures + 1))
    fi
done < <(git worktree list --porcelain | awk '
    /^worktree / { path = substr($0, 10) }
    /^branch / { branch = substr($0, 8); sub(/^refs\/heads\//, "", branch) }
    /^$/ { print path "\t" branch; path = ""; branch = "" }
    END { if (path != "") print path "\t" branch }
')

echo "Removing branches..."
while read -r branch; do
    keep=0
    for kept in main agent-main "${kept_branches[@]}"; do
        if [[ "$branch" == "$kept" ]]; then
            keep=1
            break
        fi
    done
    if [[ "$keep" -eq 0 ]]; then
        echo "Deleting branch: $branch"
        if ! git branch -D "$branch"; then
            echo "cleanup-branches: failed to delete branch: $branch" >&2
            failed_items+=("branch: $branch")
            failures=$((failures + 1))
        fi
    fi
done < <(git for-each-ref --format '%(refname:short)' refs/heads/)

if [[ "$failures" -ne 0 ]]; then
    echo "cleanup-branches: $failures removal(s) failed:" >&2
    for item in "${failed_items[@]}"; do
        echo "  $item" >&2
    done
    exit 1
fi

echo "Cleanup complete."
