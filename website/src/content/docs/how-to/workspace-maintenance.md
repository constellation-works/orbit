---
title: Maintain and remove workspaces
description: "Inspect a workspace, sync shipped defaults after an upgrade, apply migrations, rebind a moved checkout or Git remote, and remove or tear down a workspace."
---

Keep a registered workspace current as Orbit and your repository change:
check its binding, pick up new defaults after an upgrade, relocate a checkout or
follow a moved Git remote, and retire it when you are done.

## Inspect bindings and ship mode

```bash
orbit workspace show     # this checkout
orbit workspace list     # registered workspaces; --all adds replica checkouts
```

`show` prints the workspace name and `ws_*` ID, the checkout path, its role
(`owner` or `replica`), its ship mode (`pr` or `local`) and the base branch.

### Change the ship mode

A workspace delivers a finished task by opening a pull request (`pr`) or by
merging into the local base branch (`local`).

```bash
orbit workspace ship-mode          # print the current mode
orbit workspace ship-mode local    # change it
orbit --workspace my-project workspace ship-mode pr
```

Changing the mode reports the previous one and touches no configuration file.

### Reassert a checkout's role

In a [multi-machine setup](/how-to/distributed-drain/), one checkout is the
`owner` and the others are `replica` checkouts.

```bash
orbit workspace role my-project owner
orbit workspace role my-project replica --owner hm_abcdef0123456789
```

A replica requires `--owner` with the owner's machine ID. An owner rejects
it. Orbit validates the declaration before it saves.

## Synchronize shipped defaults

A new Orbit release can ship updated routines, executors, policies and prompt
templates. After you [upgrade](/getting-started/install/#stay-current), run
sync once on each host:

```bash
orbit workspace sync --all
```

Sync brings every workspace registered on this host, replicas included, up to
the installed binary's defaults. It keeps managed files you have edited and
reports them as preserved. Without `--all`, it syncs the current checkout
only. `--all` works from any directory and cannot be combined with
`--workspace`.

To see what would change without writing anything:

```bash
orbit workspace sync --check          # exit 3 when changes are pending
orbit workspace sync --check --all
```

With `--all`, the output has one line per workspace:

| Status | Meaning |
|---|---|
| `synced` | Changes were applied; the line lists them. |
| `pending` | With `--check` only: a sync would apply changes. |
| `already current` | Nothing to apply. |
| `skipped` | Left untouched, with the reason: a missing checkout or `.orbit/` directory, a path that is not a Git checkout, a data root shared with an earlier workspace that synced, or a live drain or run. |
| `failed` | Sync raised an error; the line carries it. |

One failure does not stop the rest. The command exits 1 if any workspace
failed, and 3 under `--check` when changes are pending and none failed.
`--json` prints the same records under `workspaces`, with a `summary` of
counts.

### If a workspace is skipped for a live run

`orbit workspace sync --all` does not rewrite a workspace while a drain or run
is using it, and no flag overrides that. When the drain or run ends, run
`orbit workspace sync --all` again. `--check` only reads, so it never skips
for a live run.

Single-checkout `orbit workspace sync` does not check for live runs: it
rewrites the workspace even while a drain or run is executing its managed
definitions, so run it only once the workspace is idle.

### If `.orbit/` files are tracked by Git

Sync maintains an `.orbit/` block in the repository's `.gitignore`, but it
never runs `git` for you. If `orbit doctor` reports tracked `.orbit/` files,
run `git rm -r --cached .orbit`.

`orbit doctor` also raises a `workspace-convergence` warning that names any
workspace behind the installed binary's defaults.

## Inspect and apply migrations

Orbit versions two things: the SQLite store schema and the workspace layout
in `.orbit/`. Pending migrations apply automatically when an Orbit command
opens the workspace, so you rarely run them yourself.

```bash
orbit migrate              # list pending migrations; nonzero exit if any
orbit migrate --confirm    # apply them
```

```text
COMPONENT         CURRENT  SUPPORTED
workspace layout  9        9
store schema      86       86

workspace: /path/to/repo/.orbit (orbit 0.45.1)

Workspace is up to date.
```

### If a newer Orbit migrated the workspace

When every newer migration is additive, an older binary opens the workspace
read-only: read commands work and writes are refused. Otherwise commands
refuse to open it. Upgrade Orbit in both cases.

### Adopt a workspace that recorded no authority

A workspace records the home directory or `--root` its clients run under,
and layout migrations run only under that authority. A workspace last opened
by an older Orbit, or a data directory used as its own `--root`, may have
recorded none. Every pending layout migration then refuses with
`it has recorded no generation authority`.

Stop every Orbit process that uses the workspace: MCP sessions, the
dashboard, drains and the clock. Then name the authority those clients use:

```bash
orbit migrate --adopt-authority ~/.orbit
orbit --root /srv/orbit migrate --adopt-authority /srv/orbit   # a data directory used as its own root
```

Adoption refuses, and changes nothing, while a long-lived client of the
workspace is still running or when the workspace already records a different
authority. Afterwards, `orbit migrate --confirm` applies the pending
migration.

### Repair historical crew attribution

Analytics that group by crew read the crew stored on each task. Tasks
admitted before Orbit recorded the crew their delivery selected carry a
stale or missing value. Repair them once, on the owner's checkout, after
upgrading:

```bash
orbit migrate --backfill-task-crews
orbit migrate --backfill-task-crews --workspace my-project
```

The command applies pending migrations first and has no preview. For each
task, it copies the crew from the task's latest delivery: the latest local
run, or a newer settlement from a pull drain. It leaves a task alone when:

- it has no delivery to copy from;
- its latest local run was cancelled before implementing, and no settlement
  is newer;
- its stored crew already matches;
- you assigned its crew explicitly after the delivery ran.

```text
task crew backfill in workspace ws_0123456789abcdef: 2 changed of 9 tasks with a delivery crew selection
  ABC-1: (none) -> night-shift
  ABC-2: day-crew -> night-shift
```

`--json` carries the same report under `task_crew_backfill`. Each change
appends a `crew_assigned` entry to the task's history; there is no other
undo. Running it again changes nothing. Deliveries recorded only on another
host are not repaired from here.

To see what a task records before you run it, use `orbit task show <id>`
(the `Execution Crew` row) and `orbit run history --task <id>`.

`--backfill-diff-stats` is a separate repair: it records landed line counts
for done tasks, up to `--limit` per run. The backfill flags cannot be
combined with each other or with `--dry-run`.

### Repair historical dispatch effort

Agent scorecards group work by model and reasoning effort. Invocations that
finished before Orbit recorded the effort show as `@ effort unknown`.
Recover it from each invocation's audited launch command:

```bash
orbit migrate --backfill-dispatch-effort --workspace my-project
```

Run it on each host, against that host's own store, after upgrading. It
applies pending migrations first, and running it again changes nothing. The
report counts what it recorded, by effort, and what it left unknown.

An invocation stays `effort unknown` when no launch matches it or its
matching launches disagree. Effort is never guessed from crew
configuration; only a unit dispatched under an explicit `<crew>@<effort>`
label shows that effort. A launch with no effort flag is recorded as the
provider default.

Runs that another machine executed for a [pull drain](/how-to/distributed-drain/)
are not in the owner's store. On each follower, after
`--backfill-dispatch-effort` there, send them to the owner:

```bash
orbit migrate --backfill-claimed-effort --workspace my-project
```

The owner records an effort only for claims that follower ran and never
replaces one it already holds.

## Rebind a moved checkout

When you move an existing repository checkout to a new directory on this
machine, rebind the workspace to point Orbit's host-local registration to
the new location.

This command updates only the local checkout binding on the current host. It
never moves files on disk, modifies Git configuration or remote URLs on your
forge, or changes another host. To update the registered repository URL after a
forge rename or transfer, use [Rebind a moved source Git remote](#rebind-a-moved-source-git-remote).

### Prerequisites

Before rebinding, ensure the checkout and host meet these requirements:

- **Move files yourself.** Relocating the checkout directory on disk is an
  operator action. Move the directory only after completing the other
  prerequisites.
- **Stop activity.** Stop workspace runs and release any active task claims or
  path reservations.
- **Remove managed worktrees.** Finish work in any retained Orbit-managed
  worktrees under `.orbit/state/worktrees/` and remove them before moving the
  primary checkout.
- **Matching Git origin and identity.** The moved directory must be a primary
  Git checkout whose `origin` remote matches the registered source remote. It
  must retain its checkout-local `.orbit/` identity directory (not a symlink
  or an external data root) matching the registered workspace.
- **Existing host store.** The rebind operation requires an existing host store
  (`~/.orbit/orbit.db`) outside the moved directory. If moving a newly
  initialized checkout before running any tasks, run a runtime command such as
  `orbit tool run orbit.task.list --input '{}'` first to initialize the store.
- **Old directory removed.** The old checkout path must no longer exist.

### Rebind walkthrough

Always supply `--workspace` with the registered workspace name or ID, because
Orbit cannot resolve the workspace from the moved path before rebinding.

```bash
# 1. Stop active runs and move the repository directory.
mv /path/to/old-repo /path/to/new-repo

# 2. Validate without changing the registry.
orbit --workspace my-project workspace rebind --path /path/to/new-repo --dry-run

# 3. Apply the new checkout binding.
orbit --workspace my-project workspace rebind --path /path/to/new-repo

# 4. Verify the registered checkout path.
orbit --workspace my-project workspace show
```

Passing `--dry-run --json` outputs the `old` and `new` `repo_root` and
`orbit_dir` paths and reports whether the binding would succeed without writing
to the registry.

### Refusals and recovery

Orbit validates preconditions before writing changes and refuses the rebind if:

- `--workspace` is omitted;
- the host store (`orbit.db`) is missing (refused even under `--dry-run`);
- the target is not a primary Git checkout root or its `origin` remote does not
  match the registered source remote;
- the target lacks a local `.orbit/` directory, uses a symlink for `.orbit/`,
  or its `.orbit/` identity does not match the registered workspace;
- the old checkout directory still exists;
- retained Orbit-managed worktrees remain registered in Git or on disk;
- live runs, active task claims, or path reservations remain unsettled; or
- the checkout uses an external Orbit data root instead of a checkout-local
  `.orbit/` directory.

If a rebind is interrupted before completing, Orbit refuses runtime operations
on the workspace until recovery finishes. Repeat the exact same command to
complete the rebind safely:

```bash
orbit --workspace my-project workspace rebind --path /path/to/new-repo
```

## Rebind a moved source Git remote

When you transfer or rename the repository on your forge, update the source
identity Orbit registered, so pull request delivery and task publication
keep working. Orbit accepts a portable Git URL: no credentials, local paths
or remote aliases. To update local paths after relocating a checkout on disk,
see [Rebind a moved checkout](#rebind-a-moved-checkout) above.

:::caution[Remove a publication binding first]
If the workspace uses [task publication](/how-to/task-publication/), the
rebind refuses, even with `--dry-run`, while a publication binding exists.
Save the output of `orbit workspace publication show --json`, then run
`orbit workspace publication remove --confirm`. After the source rebind, run
`orbit workspace publication rebind` with a new publication ID and an empty
branch. Orbit never rewrites old snapshots.
:::

```bash
# 1. Save the current remote in case you need to roll back.
orbit workspace source-remote show --json

# 2. Validate without changing the registry.
orbit workspace source-remote rebind --remote git@github.com:example/new-repo.git --dry-run --json

# 3. Apply.
orbit workspace source-remote rebind --remote git@github.com:example/new-repo.git

# 4. Orbit does not touch your Git configuration. Update origin yourself, then verify.
git remote set-url origin git@github.com:example/new-repo.git
orbit workspace source-remote show
```

To roll back, rebind to the URL you saved in step 1.

## Remove or tear down a workspace

| | `orbit workspace remove <workspace>` | `orbit workspace teardown <workspace> --confirm` |
|---|---|---|
| Registration | Removed | Removed |
| `.orbit/` in the checkout | Kept | Deleted |
| Tasks in `~/.orbit/tasks/` | Kept | Deleted |
| Orbit's skill links in `.agents/skills/` and `.claude/skills/` | Kept | Deleted |
| Confirmation | None; acts immediately | Requires `--confirm` |

`<workspace>` is a name, a `ws_*` ID or an absolute checkout path.

### Deregister with remove

```bash
orbit workspace remove my-project
```

Use `remove` to unregister a checkout and keep its task history and
settings. If task bundles remain, the command prints the path of the
task-store partition it left. Reclaim it later with
`orbit doctor --fix-orphan-task-stores --confirm`.

### Delete artifacts with teardown

```bash
orbit workspace teardown my-project             # print the plan; change nothing
orbit workspace teardown my-project --confirm   # delete
```

Without `--confirm`, teardown prints the workspace, repository root, `.orbit`
directory and task stores it would delete, then exits.

Teardown refuses when your current directory belongs to a different
registered checkout. It also refuses the global `~/.orbit/` directory. If it
is interrupted, run the same command again.

To keep the tasks, [export them](/how-to/state-and-backup/#export-tasks-to-an-archive)
before you tear down.
