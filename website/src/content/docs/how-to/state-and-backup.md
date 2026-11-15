---
title: Back up and move Orbit state
description: "Back up the Orbit state you cannot regenerate, verify a restore, and move tasks between machines with export and import."
---

Back up the files Orbit cannot regenerate, check a restore, and pick the right
command when you want to move tasks instead of restoring a machine.

## Where state lives

Machine state is under `~/.orbit/`, or the directory you pass as `--root`.
Workspace state is under `.orbit/` in each checkout. Back up these; you
cannot regenerate them.

| Path | Holds |
|---|---|
| `~/.orbit/config.toml` | Global settings and this machine's identity in `[machine]`: `id`, `name`, `task_prefix`. |
| `~/.orbit/workspaces.json` | The workspaces registered on this machine, with owner and replica roles. |
| `~/.orbit/tasks/workspaces/<ws-id>/<task-id>/` | The task bundles. |
| `~/.orbit/tasks/index.sqlite` | The task-ID allocator, its highest issued ID, and workspace bindings. |
| `~/.orbit/orbit.db` | Audit events, job runs and checkpoints, reservations, routine state. |
| `~/.orbit/frictions/` | The friction tag taxonomy and legacy friction records. |
| `~/.orbit/resources/` | The YAML you wrote. `orbit init` reseeds the shipped defaults. |
| `<repo>/.orbit/config.yaml`, `config.toml` | Workspace identity and settings. |
| `<repo>/.orbit/resources/` | Workspace overrides for activities, jobs, executors and policies. |
| `<repo>/.orbit/state/audit/blobs/` | Captured output that rows in `orbit.db` point to. Keep it with `orbit.db`. |
| `<repo>/.orbit/state/worktrees/` | Run worktrees. They can hold uncommitted work from active or failed runs. |

You can lose `<repo>/.orbit/state/semantic.db` (rebuilt by
`orbit search reindex`) and `~/.orbit/state/logs/`. The
[state and backup runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/state-and-backup.md)
has the complete inventory.

[Task publication](/how-to/task-publication/) is not a full backup: it covers
task records only.

## Take a backup

:::caution
Do not copy `orbit.db` or `tasks/index.sqlite` while Orbit is running, even
with their `-wal` and `-shm` files. The copy can miss committed changes and
still pass an integrity check.
:::

**Cold copy.** Stop Orbit first: the dashboard, MCP servers, scheduled timers
and any running drain. Then copy the whole directory. An atomic filesystem
snapshot of the volume that holds `~/.orbit` is equally safe.

```bash
cp -a ~/.orbit ~/orbit-backup-$(date +%F)
```

**Live snapshot of one database.** This works while Orbit runs. It needs the
`sqlite3` shell, which Orbit does not install.

```bash
sqlite3 ~/.orbit/orbit.db "VACUUM INTO '/backups/orbit.db'"
# or
sqlite3 ~/.orbit/orbit.db ".backup /backups/orbit.db"
```

Keep backups outside `~/.orbit`. `orbit gc deployments` treats
`orbit.db.bak*` files there as disposable.

## Restore and verify

Stop Orbit first. The restore overwrites the current database, so keep a copy
of it until verification passes.

```bash
cp /backups/orbit.db ~/.orbit/orbit.db
rm -f ~/.orbit/orbit.db-wal ~/.orbit/orbit.db-shm   # stale sidecars from the old file

orbit task reindex        # rebuild task index rows from the bundles
orbit search reindex      # rebuild the task search index
orbit doctor --deep       # scan every database page
```

`orbit doctor --deep` should report `ok` on the `database` row. Then run
`orbit task show <id>` on a task you know exists.

If you restore task bundles by copying `~/.orbit/tasks/`, restore
`tasks/index.sqlite` with them and run `orbit task reindex`. Without that
file Orbit refuses to issue new task IDs; the runbook above has the recovery.

## Restore tasks, or move them

| You want to | Use |
|---|---|
| Recover the same machine and workspace from a publication snapshot | [`orbit task publication restore`](/how-to/task-publication/#restore) |
| Copy tasks to a different machine or workspace | `orbit task export`, then `orbit task import` |
| Keep read-only copies of another machine's tasks current | `orbit task import --on-conflict owner-wins`, repeated |

Publication restore needs the original machine ID, workspace and source
remote, and an empty task store. Export and import need none of that.

## Export tasks to an archive

`orbit task export` packs one workspace's task bundles into a `tar.zst`
archive. It leaves out audit events, run history, configuration and machine
identity.

```bash
orbit task export --output tasks.tar.zst                  # every task in the current workspace
orbit task export --ids BOX-00012,BOX-00040 -o some.tar.zst  # only these tasks
orbit task export --task-workspace <ws-id> -o other.tar.zst  # another workspace on this machine
```

Export fails if the workspace has no tasks or does not hold an ID you named.
A failed export leaves an existing archive at the same path untouched.

Task text and attachments can hold sensitive detail. Protect the archive as
you protect the task store.

## Import tasks

```bash
orbit task import tasks.tar.zst
orbit task import tasks.tar.zst --task-workspace <ws-id> --on-conflict renumber
```

Import validates the whole archive before it writes anything. Without
`--task-workspace`, tasks go to the archive's source workspace; import
registers that workspace, without a checkout, if this machine does not know
it. To target an existing workspace, pass its `workspace_id` from
`.orbit/config.yaml` in that checkout.

A task collides when its ID already exists locally with different content.
`--on-conflict` decides what happens:

| Value | Behavior |
|---|---|
| `renumber` (default) | Gives the incoming task a new local ID, rewrites relations among the imported tasks, and writes the old-to-new map to `<archive>.idmap.json`. |
| `skip` | Keeps the local task and drops the incoming one. |
| `fail` | Aborts the whole import at the first collision. |
| `owner-wins` | Replaces mirrors, keeps your own tasks, never renumbers. See below. |

Under every policy, a task with a free ID keeps it, and a task that is
already present with identical content is reported `already-present`.

```text
$ orbit task import tasks.tar.zst
imported into workspace 'ws_proj'
  already-present  BOX-00000
  renumbered  BOX-00001 -> DEV-00002
  id mapping written to tasks.tar.zst.idmap.json
```

Import an archive that renumbers only once: a second run mints another new ID
for each collision. Keep the `.idmap.json` as the record of what landed.

### Keep mirrors current with `owner-wins`

The machine whose prefix a task carries is its only writer. A copy on another
machine is a read-only mirror. `owner-wins` refreshes mirrors from the owner's
export:

```bash
# On the machine that mints BOX- tasks
orbit task export --output box-tasks.tar.zst

# On the machine holding the mirror
orbit task import box-tasks.tar.zst --on-conflict owner-wins
```

| Colliding task's prefix | Result |
|---|---|
| Another machine's | Replaced by the incoming copy; reported `updated`. |
| This machine's | Left alone; reported `skipped-local-owned`. |

Running the same archive again reports `already-present`, so you can repeat
the import on a schedule. Import the tasks that others depend on first: a
cross-machine dependency resolves only after its target's mirror has landed.

## Check the result

```bash
orbit task list --workspace <name>
orbit doctor
```

If `orbit task list` reports bundles missing from the index, run
`orbit task reindex` before you treat an empty list as lost tasks.
[Run Orbit across hosts](/how-to/multi-host/) explains how a task ID routes
to its owner.
