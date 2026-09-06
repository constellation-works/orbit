# Keeping a workspace healthy

Day-2 operations. The first section is the one that actually bites busy
workspaces; the rest is periodic hygiene.

## Worktree garbage collection

Implementation pipelines create worktrees; deterministic collection or filing
jobs need not. Nothing reclaims them automatically unless
you arrange it, so a workspace that ships on a schedule accumulates worktrees
until the disk fills. **Enable this before, not after, scheduling ship traffic.**

```bash
orbit gc worktrees                          # report only — the default is non-destructive
orbit gc worktrees --confirm                # actually remove
orbit gc worktrees --older-than-hours 24    # leave recent runs alone
orbit gc worktrees --run <run_id>           # restrict to one run
```

Collection is conservative: it reaps only worktrees whose associated task has
settled to `done`, `rejected`, or `archived`. A worktree belonging to live work
is never a candidate. Run the report a few times before automating it, then
enable the `worktree-gc` routine for hourly reclamation —
[automation.md](automation.md).

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
| `--fix-retired-activity-backends` | Removes known retired `spec.backend` values from agent-loop activities. |
| `--remove-graph` | Removes retired graph state from this worktree and the shared workspace. |

`--fix-stale-artifacts` is how a workspace catches up after an Orbit upgrade
drops a shipped definition. It works by content provenance — if you edited a
seeded file, Orbit assumes you meant it and leaves it in place.

## Task locks

```bash
orbit task locks list                      # files held by active tasks and reservations
orbit task locks release <reservation_id>  # operator escape hatch
```

Release only after confirming the holding task is genuinely inactive, and only
through this surface — never by editing the store. The full diagnostic sequence
for a reservation blocking a run is in
[common-failures.md](../common-failures.md).

## Managed resources and upgrades

```bash
orbit workspace sync --check --json   # no writes; exit 3 means pending changes
orbit workspace sync --json           # converge managed defaults
orbit skill list
orbit skill doctor
orbit skill link                      # repair supported user skill symlinks
```

Run sync from an initialized, registered checkout after upgrading the binary.
It reconciles both host-global and workspace-local managed assets, including
skill references, jobs, activities, routines, and auto-task definitions.
Provenance distinguishes untouched shipped content from local edits. Read
`preserved` and `binding_drift` outcomes; a successful sync does not mean a
customized override was overwritten. Never fabricate or edit the managed-asset
manifest to force replacement. Review custom overrides against the installed
catalog and update them deliberately.

Sync updates managed files, not workspace ownership or task publication. The
plugin skill bundle updates through its plugin distribution; global skill
symlinks and a plugin installation are distinct delivery paths.

## Database and layout upgrades

```bash
orbit migrate               # inspect pending migrations without applying
orbit migrate --confirm     # apply
```

Both ledgers — the SQLite schema and the workspace layout — auto-apply when a
runtime opens, so most upgrades need nothing. The bare command and `--dry-run`
are the only way to *see* what is pending without applying it, which is what you
want before upgrading a machine that matters.

## Audit events

```bash
orbit audit list --since 1h --status failure
orbit audit stats --since 7d
orbit audit export --json > audit.json
orbit audit prune --older-than 90d --confirm
```

The audit store is persistent invocation metadata: who called what, when, and
whether it was denied. It grows without bound until pruned. Prune requires
`--confirm`; export first if the history matters.

## Logs

The global JSONL trace at `~/.orbit/state/logs/orbit.jsonl` rotates
opportunistically at process start. Defaults: seven days of archives, a 500 MiB
total budget, a 100 MiB active-file threshold. Tune in `config.toml`:

```toml
[runtime]
log_retention_days = 7
log_max_total_mb   = 500
log_max_file_mb    = 100
```

Values are validated at load — zero is rejected, and `log_max_file_mb` may not
exceed `log_max_total_mb`.

```bash
orbit log tail -n 120
orbit log tail --level warn --since 1h
```

For diagnosing a host-level incident rather than tuning retention, see
[operational-logs.md](../operational-logs.md).

## Search indexes

```bash
orbit semantic stats                       # companion and index status
orbit semantic index --kind tasks|docs|all
orbit docs index                           # doc corpus embeddings
```

Both are idempotent and safe to re-run. Reindex after bulk imports, large doc
moves, or a restore. → [search.md](../search.md)

## What is evidence and must not be edited

Files under `.orbit/state/job-runs/`, `.orbit/state/audit/`, and
`~/.orbit/state/` are the record of what happened. Never edit them to make a
warning disappear or a run look successful. If a run is wrong, fix the cause and
re-run; the failed run stays as history.

For a validated task snapshot and same-authority recovery, use
[publication.md](publication.md). It does not back up logs, credentials, or
scheduler state.

Task bundles can also be moved deliberately — `orbit task export` and
`orbit task import` handle portable archives, and `orbit task reindex` rebuilds
the registry index from on-disk bundles after a restore.

For archive migration, `orbit task export --output <archive.tar.zst> --ids <id>,<id>`
selects tasks (omitting IDs exports all). `orbit task import <archive.tar.zst>`
defaults to renumbering collisions and rewriting references; choose
`--on-conflict fail` or `skip` deliberately when appropriate. Inspect the returned
ID mapping. Import is a different operation from publication's strict
same-authority, identical-retry recovery; do not substitute it to bypass a
publication ownership or divergence error.
