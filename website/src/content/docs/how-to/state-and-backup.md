---
title: Back up and move Orbit state
description: "Know which Orbit state is authoritative, take consistent SQLite backups, verify a restore, and move tasks between machines with export and import."
sidebar:
  order: 6
---

Orbit keeps state in two places. This page shows which files you cannot
regenerate, how to copy them safely, how to check a restore, and which command
to use when you move tasks instead of restoring a machine.

## Where state lives

**User/machine state** is under `~/.orbit/`. `--root <dir>` points Orbit at a
different data directory and takes precedence over every other source.
**Workspace state** is under `.orbit/` in each checkout.

Back up and restore these. You can't regenerate them.

- `~/.orbit/config.toml`: global settings and this machine's identity in
  `[machine]` (`id`, `name`, `task_prefix`).
- `~/.orbit/workspaces.json`: the registry of workspaces on this machine, with
  owner and replica roles.
- `~/.orbit/tasks/workspaces/<ws-id>/<task-id>/`: the canonical task bundles.
- `~/.orbit/tasks/index.sqlite`: the task-ID allocator and its issued-ID
  high-water mark, plus workspace bindings. Its index rows can be rebuilt with
  `orbit task reindex`; the allocator and bindings can't.
- `~/.orbit/orbit.db`: audit events, job runs and checkpoints, reservations and
  routine state.
- `~/.orbit/frictions/`: the tag taxonomy and legacy friction record files.
- `~/.orbit/resources/`: only the YAML you wrote and any `.retired-managed/`
  copies. The shipped defaults are reseeded by `orbit init`.
- `<repo>/.orbit/config.yaml` and `config.toml`: workspace identity and optional
  workspace settings.
- `<repo>/.orbit/resources/`: workspace overrides for activities, jobs,
  executors and policies.
- `<repo>/.orbit/state/audit/blobs/`: the audit output that rows in `orbit.db`
  point to. Keep it with `orbit.db` if detailed output matters.
- `<repo>/.orbit/state/worktrees/`: run worktrees, which can hold uncommitted
  candidates from active or failed runs. Keep them until the run is settled.

You can lose and regenerate these: `<repo>/.orbit/state/semantic.db` (rebuilt by
`orbit search reindex`), `~/.orbit/state/logs/`, and `~/.orbit/embed/`.
[The state and backup runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/state-and-backup.md)
has the complete inventory, including retired directories you can delete.

Task publication is not a full backup. It covers task records only. See
[Publish and restore tasks](../task-publication/).

## Take a backup

Orbit's SQLite files (`orbit.db` and `tasks/index.sqlite`) run in WAL mode, so a
committed change can sit in a `-wal` file until SQLite checkpoints it. Copying a
database file while Orbit may write is not a safe backup, even when you also copy
its `-wal` and `-shm` files. A checkpoint between the copies can leave committed
changes out of the copy, and the copy can still pass `quick_check`. Pick one of these.

**Cold copy.** Stop Orbit first: the dashboard, MCP servers, scheduled timers and any
running drain. Then copy the whole directory.

```bash
cp -a ~/.orbit ~/orbit-backup-$(date +%F)
```

A plain file copy of the database files is safe only in this stopped state, or
from one atomic filesystem snapshot of the volume that holds `~/.orbit`, taken at a
single instant.

**Live snapshot of one database.** This works while Orbit runs and writes a
checkpointed file with no sidecars. It uses the `sqlite3` shell, which Orbit does
not install for you.

```bash
sqlite3 ~/.orbit/orbit.db "VACUUM INTO '/backups/orbit.db'"
# or
sqlite3 ~/.orbit/orbit.db ".backup /backups/orbit.db"
```

A live snapshot covers one database. For the whole machine, use the cold copy or an
atomic filesystem snapshot. Keep recovery copies outside `~/.orbit`, because deployment
cleanup (`orbit gc deployments`) treats `orbit.db.bak-*` files there as disposable.

## Restore and verify

Stop local Orbit processes first. This overwrites the current store database, so keep a
copy of it until verification passes.

```bash
cp /backups/orbit.db ~/.orbit/orbit.db
rm -f ~/.orbit/orbit.db-wal ~/.orbit/orbit.db-shm   # stale sidecars from the old file

orbit task reindex        # rebuild task index rows from the bundles
orbit search reindex      # rebuild the task search index
orbit doctor --deep       # scan every database page
```

`orbit doctor --deep` should report `ok` for the `database` row. Then run
`orbit task list` or `orbit task show <id>` on a task you know exists.

If you restored task bundles by copying `~/.orbit/tasks/` rather than
`orbit.db`, run `orbit task reindex` afterwards. Restore `tasks/index.sqlite`
along with the bundles. It alone records the highest task ID this machine has
issued, and a bundle copy without it makes Orbit refuse new task IDs until the
high-water mark is set again. The recovery steps are in the runbook linked above.

## Restore tasks, or move them

Three situations look alike and take different commands.

| You want to | Use |
|---|---|
| Recover the same machine and workspace after a loss, from a [publication](../task-publication/) snapshot. | `orbit task publication restore`. Restore `config.toml` and `workspaces.json` first so the machine and workspace keep their identity. |
| Copy tasks to a different machine or workspace, where the identity differs. | `orbit task export` and `orbit task import`. |
| Keep read-only copies of another machine's tasks current. | `orbit task import --on-conflict owner-wins`, repeatedly. |

Publication restore needs the same authority: the owner checkout, the same logical
workspace and source remote, and the same machine ID. It refuses a destination that
already has tasks and never renumbers. Export and import make no such assumption;
they exist for the case where that identity is gone or deliberately changed.

## Export tasks to an archive

`orbit task export` packs one workspace's task bundles into a `tar.zst`
archive. It does not include audit events, run history, configuration or machine identity.

```bash
orbit task export --output tasks.tar.zst                     # every task in the current workspace
orbit task export --ids BOX-00012,BOX-00040 -o some-tasks.tar.zst  # only these tasks
orbit task export --task-workspace <ws-id> -o other.tar.zst  # another workspace on this machine
```

`--all` is the default when you omit `--ids`. Export fails if the workspace has
no tasks, or if an ID you named isn't registered to it. It writes the archive
beside the destination and replaces any existing file only when the archive is
complete, so a failed export leaves an earlier backup alone. Archives hold only
regular files and directories.

Task text and attachments can hold sensitive detail. Treat the archive like the
task store itself.

## Import tasks

```bash
orbit task import tasks.tar.zst
orbit task import tasks.tar.zst --task-workspace <ws-id> --on-conflict renumber
```

Import checks the archive and every bundle before it changes anything, so a corrupt
or incompatible archive writes nothing. Without `--task-workspace`, tasks go to the
archive's source workspace; if this machine doesn't know that workspace, import
registers it without a checkout. To land them in an existing workspace, pass its
`workspace_id` (from `.orbit/config.yaml` in that checkout).

A task collides when its ID already exists locally with different content. A
task that is already there byte for byte is reported `already-present` under every
policy. `--on-conflict` decides what happens to a collision:

| Value | Behavior |
|---|---|
| `renumber` (default) | Gives the incoming task a new local ID and rewrites relations among the imported tasks. Writes an old-to-new map to `<archive>.idmap.json`. |
| `skip` | Keeps the local task and drops the incoming one (`skipped`). |
| `fail` | Aborts the whole import at the first collision. |
| `owner-wins` | Never renumbers. See below. |

Tasks with free IDs keep them under every policy. A `renumber` import that
had collisions is not repeatable: the local task still differs from the original, so a second
run mints another new ID. Import such an archive once and keep the printed
`.idmap.json` as the record of what landed.

Illustrative output after a task was edited on the machine that minted it:

```text
$ orbit task import tasks.tar.zst
imported into workspace 'ws_proj'
  already-present  BOX-00000
  renumbered  BOX-00001 -> DEV-00002
  id mapping written to tasks.tar.zst.idmap.json
```

### Keep mirrors current with `owner-wins`

Each machine mints IDs under its own task prefix, and the machine whose prefix a task
carries is its only writer. A copy on another machine is a read-only mirror.
`owner-wins` refreshes those mirrors from the owner's export:

```bash
# On the machine that mints BOX- tasks
orbit task export --output box-tasks.tar.zst

# On the machine holding the mirror
orbit task import box-tasks.tar.zst --on-conflict owner-wins
```

For each colliding ID, import decides from the prefix alone. A task under another
machine's prefix is replaced by the incoming copy and reported `updated`. A task under
this machine's prefix is left alone and reported `skipped-local-owned`, so an archive
can't overwrite your own work. Nothing is renumbered, no `.idmap.json` is written, and
running the same archive again reports `already-present`, so it is safe to repeat on a
schedule. Import tasks that others depend on first: a cross-machine `depends_on`
resolves only once its target's mirror has landed.

Only the prefix decides, so `owner-wins` never moves ownership: a mirror stays
read-only on the machine that imported it.

## Check the result

```bash
orbit task list --workspace <name>
orbit doctor
```

`orbit task list` reports bundles missing from the task index. If it does, run
`orbit task reindex` before treating an empty list as lost tasks.
See [Run Orbit across hosts](../multi-host/) for how an ID routes to its owner
and the [CLI reference](../../reference/cli/) for the command list.
