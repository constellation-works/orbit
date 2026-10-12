---
type: runbook
summary: Reclaim declared rebuildable outputs from kept terminal run worktrees.
tags: [disk, worktree, gc]
paths: ["crates/orbit-engine/src/executor/automation/vcs/worktree/", "crates/orbit-core/src/application/gc/"]
last_validated: 2026-10-12
---

# Reclaim kept worktree output

Declare rebuildable output in the owner's workspace `.orbit/config.toml`:

```toml
[worktree]
reclaim = ["target", "node_modules", "website/node_modules", ".orbit/tmp/*target*", ".orbit/tmp/codeql-*", ".orbit/tmp/ci-fast-base"]
# Optional: reclaim during admission when the state filesystem is below 30 GiB.
reclaim_below_free_mib = 30720
```

The default list is `["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]`:
Cargo output, plus the build directories and base checkouts that agents leave
under run scratch. An explicit list replaces the earlier layer's
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

## Dirty worktrees of settled tasks

A failed claimed leaf keeps its uncommitted candidate in its worktree. The
scheduled `worktree_gc_pipeline` removes such a worktree once the run is
terminal, older than the age floor, has no live or undecidable worker, and
every task it serves is `done`, `rejected` or `archived` (or its claim is
settled), and only when the candidate is recoverable elsewhere. The candidate is
every tracked change and every untracked path Git does not ignore, snapshotted
as one tree through a scratch index. The worktree's own index and files are not
changed. If a staged path differs from this working candidate, GC retains the
checkout with a refusal reason: one tree cannot preserve both versions. This
check applies even when a published ref holds the working candidate. Staged
paths matching the candidate remain rescuable alongside ordinary unstaged
edits. GC checks these recovery sources in order:

1. `carry_ref`: the run's durable carry ref on `origin`
   (`refs/orbit/candidates/<task>/<run>`), when its tree equals the snapshot.
2. `published_branch`: the run's branch on `origin`, when its tree equals the
   snapshot.
3. `rescue_patch`: a patch against the worktree's `HEAD`, written to
   `.orbit/state/rescue/worktree-gc/<run>-<tree>.patch` under the owning
   checkout. GC checks that the patch rebuilds the exact snapshot tree, then
   points `refs/orbit/rescue/worktree-gc/<run>-<tree>` at that HEAD in the
   owning repository, and only then removes the worktree and run branch. The
   ref is what keeps an unpublished HEAD — and files that commit added but the
   patch does not repeat — reachable after the branch is gone and Git expires
   reflogs. The patch header names the ref. Check it out and run
   `git apply` on the patch. A patch is at most 16 MiB, and the store holds
   at most 256 MiB. GC never deletes patch files. Apply or delete old patches
   there; while the store is full, dirty worktrees are kept. A deleting sweep
   drops a base ref whose patch file is gone, after which ordinary Git expiry
   can remove those objects. A store path with a symlinked or non-directory
   component is refused, and GC keeps any existing base refs rather than
   deciding from a redirected directory.

The report's `recovery` names the `source`, its `location` (the ref or patch
file), and the `commit` (the ref's commit, or the commit the patch applies to).
For a patch, check out the `Base-Ref` named in the file — it points at the
recorded commit — and run `git apply` on the patch. GC reclaims declared output, then takes
the snapshot again. If anything changed, it keeps the worktree. Otherwise it
removes the worktree with `git worktree remove --force`. Ignored files go with
it, as with a clean removal. A candidate GC cannot capture exactly (a nested
repository, submodule changes, distinct staged and working versions) or whose patch exceeds a limit stays
`skipped:dirty_rescue_candidate`, and `detail` gives the reason. Manual
`orbit gc worktrees --confirm` and delivery cleanup keep every dirty worktree.

Each sweep reports kept terminal worktrees with `retained_bytes`, the size they
still occupy, and `orbit run show <gc-run>` lists them (`Kept worktree:`) with
the reason, alongside each rescued removal (`Rescued worktree:`). A live run's
worktree is never measured.

Collection runs with host authority over agent-written checkouts. It never
runs a cleanup command. `cargo clean` can honor a checkout's `build.target-dir`
pointing outside the checkout; `make clean` and npm scripts can execute
agent-written code. Declared paths let Orbit check confinement and Git content
before deletion, without giving the checkout a host command execution path.

## Relocated removal leftovers

A full sweep also inventories trash directories left by timed-out removal.
Their `.trash-<name>-<pid>-<timestamp>` name identifies an attempt, but does
not by itself authorize removal. A registered trash checkout is retained as
`skipped:registered_worktree`, regardless of its age or the deleter's liveness.
An unregistered directory whose `.git` link still resolves (including to a
different repository), or whose link cannot be verified, is retained as
`skipped:not_registered_worktree`. Inspect it and use `git worktree repair`
for a moved checkout. Relative gitdir links are resolved from that directory.

Only an unregistered directory with no `.git` link or a well-formed link to
an absent administrative directory is eligible for raw leftover removal.
It is reclaimed once the deleter is dead or the removal timeout has expired;
a live deleter within the timeout retains it as `skipped:deleter_alive`.
Dry-run applies the same safety checks before reporting `would_remove`.
