# Maintenance: worktree garbage collection

## Contents

- [Worktree garbage collection](#worktree-garbage-collection)
  - [Workspace scratch](#workspace-scratch)

## Worktree garbage collection

Implementation pipelines create worktrees; deterministic collection or filing
jobs need not. With the default seeding policy, a fresh workspace seeds the
`worktree-gc` routine enabled, but automatic reclamation needs the host's Orbit
scheduler clock installed and enabled. Without
that clock, a workspace that ships on a schedule accumulates worktrees until the
disk fills. **Make sure the host clock is running before, not after, scheduling
ship traffic.**

```bash
orbit gc worktrees                          # report only — the default is non-destructive
orbit gc worktrees --estimate-bytes         # dry-run plus a recursive byte estimate
orbit gc worktrees --confirm                # actually remove
orbit gc worktrees --older-than-hours 24    # leave recent runs alone
orbit gc worktrees --run <run_id>           # restrict to one run
orbit gc worktrees --target-only            # report only each worktree's target/ build output
orbit gc worktrees --target-only --confirm  # delete target/ and keep the checkout
```

Collection is conservative: it reaps only worktrees whose associated task has
settled to `done`, `rejected`, or `archived`, or that has been parked in
`someday` for 336 hours (14 days), counted from
the status change. A parked worktree also stays while its worker or claim is
live, while Git holds it locked, or while its run branch has commits that
exist on no published ref and in no rescue patch
(`skipped:unpublished_commits`). A worktree belonging to live work
is never a candidate.

A worktree with uncommitted changes is kept
(`skipped:dirty_rescue_candidate`), except by the scheduled
`worktree_gc_pipeline`. That pipeline removes a settled run's dirty worktree
once the candidate is recoverable: a carry ref or published branch on `origin`
holding exactly it, or a patch it writes to
`.orbit/state/rescue/worktree-gc/` and verifies first. The patch's base commit
is pinned at `refs/orbit/rescue/worktree-gc/<run>-<tree>` until a deleting
sweep sees that the patch file is gone.

`recovery` in the
report names which source. Kept terminal worktrees report `retained_bytes`, and
`orbit run show <gc-run>` lists them with their reasons. Details:
[worktree reclaim runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/worktree-reclaim.md).
Run the report a few times before relying on scheduled reclamation.

The default
seeding policy seeds `worktree-gc` enabled for hourly collection; the host clock
must also be running — [automation-scheduler.md](automation-scheduler.md).

### Workspace scratch

The same `worktree_gc_pipeline` run also prunes the checkout's own
`.orbit/tmp` (`ORBIT_SCRATCH_DIR`), where operators and
`orbit.task.artifact` action `put` callers stage evidence, build output and CodeQL
databases. Artifacts are copied into the task store, so staged files are
disposable once attached.

A top-level entry is removed when its **newest mtime
anywhere inside it** is older than `scratch_older_than_hours` (default `24`);
newer entries are kept, so a file staged moments ago is never at risk.
Override the window for one run:

```bash
orbit run job worktree_gc_pipeline --wait --input scratch_older_than_hours=72
orbit run job worktree_gc_pipeline --wait --input scratch_older_than_hours=0   # everything not in use
```

Set a workspace-specific default by editing `scratch_older_than_hours` in the
workspace copy at `.orbit/resources/jobs/worktree_gc_pipeline.yaml`. To set a
host-wide default, edit the shipped job at
`~/.orbit/resources/jobs/worktree_gc_pipeline.yaml`; local edits there are
preserved instead of refreshed from upstream. A run scoped to one run id
(`target_run_id`) leaves scratch alone.

Symlinks are removed as links and never followed out of `.orbit/tmp`. GC skips,
and reports with a reason, any entry that a live process holds open, uses as its
working directory or executes (read from `/proc` on Linux and `lsof` on macOS —
best effort, so another user's processes are not visible), or whose path a
pending, running or retrying run's record or saved pipeline state names.
Retrying runs keep their scratch while waiting for another attempt.

If the
run inventory or any required saved state cannot be read, the scratch sweep
fails before deleting any entry. A failure removing an entry skips that entry
and the sweep continues.

`orbit run show <run> --json` carries the figures under the `reap` step's
`scratch` object, beside the worktree figures: `bytes_reclaimed`,
`entries_removed`, `entries_skipped` (each listed in `entries` with its
`reason`) and `entries_kept`. To empty the directory by hand regardless of age,
use `orbit gc tmp --confirm`. Without `--confirm` the command only reports,
and it refuses while job runs are active.

On a replica checkout (one that pulls work from an owner on another machine),
new claimed tasks live on the owner; historical tasks minted with this machine's
prefix remain local. A claimed leaf whose handoff the owner accepted and whose
claim this follower settled needs no task answer: the owner holds its delivery, so
its worktree is collected and `detail` names the settled claim.

For other
worktrees, GC reads tasks carrying this machine's prefix from its local store
and asks the owner only for the owner's prefix, learned from existing claim
admissions or a single foreign prefix in the workspace's stored tasks. Claims
identify the owner even when other foreign task mirrors exist; without claims,
multiple foreign prefixes are ambiguous. An unknown or ambiguous prefix retains the worktree with
`skipped:task_prefix_unroutable` and makes no owner call.

Missing local or owner
tasks stay `skipped:task_unresolved`. Owner lookups use the owner's tool
surface, through the claim's own route (the owner must be registered with
`orbit host add`). A transport failure keeps the worktree as
`skipped:owner_unreachable` with the error in `detail`; run GC again once the
owner answers.

`skipped:no_owner_route` means the replica has no route to ask
at all — the owner has no host entry (`orbit host add`) or the checkout is
not a registered workspace — and `detail` says which. A status lookup that
fails without a transport error is reported as `skipped:owner_lookup_failed`
with the reason in `detail`; it does not establish that the owner is down.
The pull drain also
reclaims each settled leaf's `target/` on its next pass, so follower disk
does not depend on this schedule.

Removal first restores owner access to read-only directories inside the
worktree (a test fixture can leave one), so Git can delete the whole tree.

A directory under the worktree root that Git does not list as a worktree is
never removed (`skipped:not_registered_worktree`), unless it is the leftover
of a removal that failed partway: its `.git` link is missing or names an
administrative directory that no longer exists, and it maps to a terminal run
with no live worker whose tasks are settled. GC then removes it as `removed`,
with the reason in `detail`.

For any other such directory, `detail` gives the
remedy: `git worktree repair <path>` if it was moved, otherwise inspect it and
delete it by hand once nothing in it is needed.

`--target-only` (alias of `--reclaim`) reclaims the declared `worktree.reclaim`
paths, by default `target`, `.orbit/tmp/*-target` and `.orbit/tmp/base-wt`.
These hold most of a worktree's size. It deletes only those paths and keeps
the checkout, including uncommitted changes, so a failed or blocked run can
still be rescued. It needs a terminal run with no live recorded worker, not a
settled task.

A running run is never touched, and a path holding anything Git
tracks is kept (`skipped:tracked_content`). The scheduled pipeline does the
same for every kept terminal worktree. `bytes_reclaimed` reports the size of
the removed paths.

`worktree.reclaim_below_free_mib` adds admission-time reclamation: unset by
default, or a non-negative integer MiB (`u64`; `0` never triggers). While free
space beneath the state directory is below it, admission reclaims the declared
`worktree.reclaim` paths from oldest terminal worktrees first, keeping their
checkouts and protecting active runs. Read the effective mark with
`orbit config show --all`.

Inspect eligible output without deletion using
`orbit gc worktrees --reclaim --dry-run --estimate-bytes`; this previews GC
eligibility, not a forced low-space admission. Actual admission reclamation
records a `worktree.reclaim` audit event with reclaimed bytes. See
[Configuration](configuration-settings-workflow.md#common-settings) for the setting and limits.

With `workflow.validation_env.build_output_env` configured, required validation
binds the workspace's output variables to host-owned output under
`.orbit/state/host-validation-targets/`, outside the checkout. Native Linux and macOS
collectors keep the same layout and lineage for historical output; an empty mapping
allocates nothing and never deletes old output. Other platforms retain configured
output for operator maintenance.

Darwin uses readable no-follow directory handles
and `fchmod` on held descriptors; unreadable directories or unprovable identities
refuse reclamation rather than relaxing permissions for measurement. GC attributes
each target to the runs whose recorded worktree path it was built for, and
reclaims it once they are terminal with no live worker and the checkout is gone
(or its kept output is being reclaimed).

A scratch checkout's target (base
rerun, host evidence, recovery) is reclaimed only through its base-cache lock
or lease record, while GC holds that lock; a held lock reports
`skipped:host_target_in_use`. Targets with no such record stay
`skipped:unattributed_host_target`. The existing worktree-GC routine and pipeline
use the same collector.

Platform support does not authorize production deletion,
resume paused routines or change schedules; validate disposable native fixtures
and obtain the required review before rollout. Byte reports measure logical output,
not physical disk headroom.
