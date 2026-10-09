---
type: runbook
summary: Reclaim declared rebuildable outputs from kept terminal run worktrees.
tags: [disk, worktree, gc]
paths: ["crates/orbit-engine/src/executor/automation/vcs/worktree/", "crates/orbit-core/src/application/gc/"]
last_validated: 2026-10-09
---

# Reclaim kept worktree output

Declare rebuildable output in the owner's workspace `.orbit/config.toml`:

```toml
[worktree]
reclaim = ["target", "node_modules", "website/node_modules", ".orbit/tmp/*target*", ".orbit/tmp/codeql-*", ".orbit/tmp/ci-fast-base"]
# Optional: reclaim during admission when the state filesystem is below 30 GiB.
reclaim_below_free_mib = 30720
```

The default list is `["target"]`. An explicit list replaces the earlier layer's
list; `[]` disables reclamation. The free-space threshold is unset by default.
Globs are relative to the worktree root: `*` matches within one component and
`**` as a whole component matches zero or more components, including hidden
directories. Other admitted characters are literal; backslashes, colons and
NUL bytes are refused. Absolute paths, parent components,
empty or dot components, and patterns matching the worktree root fail config
load. Use forward slashes on every platform.

Inspect and apply the same declared policy:

```bash
orbit gc worktrees --reclaim
orbit gc worktrees --reclaim --confirm
```

`--confirm` is governed: it needs the operator or runner capability. From a
shell with no terminal, prefix `ORBIT_OPERATOR=1`, as doctor's printed action
does.

The default and `--dry-run` report paths, matching patterns and file-size bytes
without deletion. Overlapping matches count each removed path once, against the
first matching pattern. `--run` and `--older-than-hours` can narrow the manual
pass. `--target-only` remains an alias for `--reclaim`. Doctor warns when kept
worktrees hold more than 10 GiB of reclaimable output and names this command.

The scheduled `worktree_gc_pipeline` reclaims every retained terminal worktree
on each sweep, including failed, blocked and review candidates. Its age floor
applies to whole-worktree removal; retained terminal output can be reclaimed
immediately. When configured, actual admission checks free space beneath the
state directory and visits oldest terminal worktrees with declared output first,
stopping once the threshold is met. Runs without matching output are skipped
before collection and Git queries, even if free space remains low across
admissions. The next admission checks for newly created output again.
Reclamation is best effort; a failure is reported and later
passes can retry. Active runs, including another active run sharing a checkout,
are protected.

A deletion requires a terminal run, a Git-registered real worktree, and no live
or undecidable recorded worker. Each matched path must resolve strictly inside
the worktree, with no symlink at the path or any ancestor beneath the worktree.
Git must report ignored or untracked content, and no tracked file may lie at or
beneath the match. Git metadata is protected. Symlinks inside a matched directory
are unlinked during recursive removal rather than followed. Run branches,
candidate commits, tracked files and unmatched evidence under `.orbit/tmp` stay.
Resume applies the preserved candidate and rebuilds the removed output. Only
declare caches and build output: evidence matching a declaration is reclaimable.

Collection runs with host authority over agent-written checkouts. It never
runs a cleanup command. `cargo clean` can honor a checkout's `build.target-dir`
pointing outside the checkout; `make clean` and npm scripts can execute
agent-written code. Declared paths let Orbit check confinement and Git content
before deletion, without giving the checkout a host command execution path.
