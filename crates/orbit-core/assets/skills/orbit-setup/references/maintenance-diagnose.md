# Maintenance: diagnose and repair

Day-2 operations. The [first section](maintenance-worktree-gc.md) is the one that actually bites busy
workspaces; the rest is periodic hygiene.

## Diagnose and repair

```bash
orbit doctor            # config, database, disk, indexes, locks, runs
orbit doctor --json
```

Run it after an upgrade, after an interrupted run, and whenever something behaves
inexplicably. It has targeted repairs, each narrow on purpose:

| Flag | Repairs |
|---|---|
| `--fix-stale-locks` | Lock files whose recorded holder process is dead. |
| `--fix-stale-task-locks` | Task reservations whose owner and task state are conclusively inactive. |
| `--fix-stale-artifacts` | Retires deprecated skills, jobs, activities, auto-tasks, and routines that Orbit itself wrote. Locally modified ones are preserved, not deleted. |
| `--fix-retired-config-keys` | Backs up each edited `config.toml` and removes retired or deprecated keys, first writing each deprecated key's translated value under its replacement key in the same layer while preserving explicit modern values. The exception is a deprecated boolean `machine.worker_containment`: the repair rewrites its value in place (`true` to `"warn"`, `false` to `"off"`, or `"strict"` when `worker_containment_strict = true` is also set) and keeps the key, which stays settable. A second successful run writes nothing. Any legacy after-landing policy refuses before either config is changed or backed up; migrate it manually first. |
| `--fix-automation-pins` | Deletes this Orbit root and workspace's own state-routine attempt pins that no consumer or live run still names. Legacy shared `refs/orbit/automation/<attempt>` pins and other roots' or workspaces' pins are retained and reported. Refuses while a routine sweep runs. |
| `--fix-state-directory-permissions` | Restricts writable Orbit state directories to owner-only access, excluding run worktrees, Cargo target trees, and child symlinks (Unix only). |
| `--fix-orphan-task-stores` | Deletes empty unclaimed task-store partitions, and populated partitions whose bound checkout is confirmed absent, including their task bundles. Unowned or unreachable populated partitions are left alone. Requires `--confirm`. |
| `--remove-graph` | Removes retired graph state from this worktree and the shared workspace. |

For a retired-key repair refused on `operation.review_policy = "after-landing"`,
choose the intended consumer state with `orbit auto-task toggle delivery-code-review on`
or `off`. Set `review.timing` and `review.minutes` explicitly as needed, remove
the legacy policy manually, then rerun the repair. This includes policies masked
by another layer; toggling the consumer alone does not permit automatic removal.

`--fix-stale-artifacts` is how a workspace catches up after an Orbit upgrade
drops a shipped definition. It works by content provenance — changes to
template-owned fields are local edits that Orbit preserves.

For routines, the two settings Orbit's own surfaces change are not edits:
flipping `enabled` (the documented opt-in, and what the dashboard toggle
writes) and deleting the retired `hosts:` key. Comparison parses the routine,
so comments do not count as local modifications either. A routine that differs
from a shipped template only in those settings or comments still counts as
Orbit-written, so an upgraded workspace converges without you moving files by
hand.

Orbit deletes outright
only bytes it can prove it wrote; anything else it copies to
`.retired-managed/routines/` before removing it from the active catalog.
Changing a routine's cadence, target, policy, or description is a real edit
and is preserved and reported; a comment-only edit remains managed.

## Task locks

```bash
orbit task locks list                      # files held by active tasks and reservations
orbit task locks release <reservation_id> --confirm  # operator escape hatch
```

The listing also includes active reservations and distinct task counts. The
Drain readiness groups name the holders blocking a waiting task. No-diff tasks
hold no context locks;
other holders use admission's canonical selectors and unlocked-path rules.
Listing locks also expires old reservations and records their audit events.

Release only after confirming the holding task is genuinely inactive, and only
through this surface — never by editing the store. The full diagnostic sequence
for a reservation blocking a run is in
[common-failures.md](../../orbit-orchestrate/references/common-failures.md).
