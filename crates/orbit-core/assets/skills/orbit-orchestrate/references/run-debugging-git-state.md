# Inspect Git state for a run failure

## Check Git State For Workflow Failures

```bash
git -C <workspace_path> status --short --branch
git -C <workspace_path> rev-parse --abbrev-ref HEAD
git -C <workspace_path> rev-list --left-right --count <base_ref>...HEAD
git -C <workspace_path> log --oneline --decorate --graph --max-count=12 --all
git -C <workspace_path> ls-remote origin refs/heads/<branch> refs/heads/<base_branch>
```

Use read-only `git merge-tree` to understand conflicts; Git rebase has no
general dry-run mode — don't resolve conflicts unless asked to fix the run, not merely investigate it.
