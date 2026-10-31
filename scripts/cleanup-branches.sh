#!/usr/bin/env bash

set -euo pipefail

# Ensure we're in the repository root
cd "$(git rev-parse --show-toplevel)"

echo "Pruning unreachable worktrees..."
git worktree prune

echo "Removing worktrees..."
main_path=$(pwd)

git worktree list --porcelain | awk '
    /^worktree / { path = substr($0, 10) }
    /^branch / { branch = substr($0, 8); sub(/^refs\/heads\//, "", branch) }
    /^$/ { print path "\t" branch; path = ""; branch = "" }
    END { if (path != "") print path "\t" branch }
' | while IFS=$'\t' read -r wt_path branch; do
    # Skip the main working tree
    if [[ "$wt_path" == "$main_path" ]]; then
        continue
    fi

    # Skip if branch is main or agent-main (detached worktrees have no branch)
    if [[ "$branch" == "main" || "$branch" == "agent-main" ]]; then
        continue
    fi

    echo "Removing worktree: $wt_path (branch: ${branch:-detached})"
    git worktree remove --force "$wt_path" || true
done

echo "Removing branches..."
git for-each-ref --format '%(refname:short)' refs/heads/ | while read -r branch; do
    if [[ "$branch" != "main" && "$branch" != "agent-main" ]]; then
        echo "Deleting branch: $branch"
        git branch -D "$branch" || true
    fi
done

echo "Cleanup complete."
