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
orbit gc worktrees --estimate-bytes         # dry-run plus a recursive byte estimate
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

For routines, the two settings Orbit's own surfaces change are not edits:
flipping `enabled` (the documented opt-in, and what the dashboard toggle
writes) and deleting the retired `hosts:` key. A routine that differs from a
shipped template only in those still counts as Orbit-written, so an upgraded
workspace converges without you moving files by hand. Orbit deletes outright
only bytes it can prove it wrote; anything else it copies to
`.retired-managed/routines/` before removing it from the active catalog.
Changing a routine's cadence, target, policy, or description — or adding a
comment — is a real edit and is preserved and reported.

## Task locks

```bash
orbit task locks list                      # files held by active tasks and reservations
orbit task locks release <reservation_id>  # operator escape hatch
```

Release only after confirming the holding task is genuinely inactive, and only
through this surface — never by editing the store. The full diagnostic sequence
for a reservation blocking a run is in
[common-failures.md](../../orbit-orchestrate/references/common-failures.md).

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

## Persistent MCP clients during upgrades

`orbit update --contract --json` describes candidate protocol support without
opening state. The updater requires this protocol before replacing an executable;
a missing/incompatible candidate is refused, including pre-fix downgrades.

Use `orbit update --preflight --json` against the configured executable and
same authorities before a wrapper changes the installation. Exit 0 reports
`schema_version: 1`, `admitted: true`, `reservation: false`,
`contract: executable-generation-v1`, and the `admission_roots` it locked;
exit 1 refuses admission on stderr and names the refusing authority.
`--root`, then `ORBIT_ROOT`, otherwise isolated `HOME=` / the host-global
root selects the first authority, reported as `global_root` (scratch init in
a read-only `~/.orbit` sandbox stays unblocked). When an override names
something other than the host-global root, the host-global root is locked
*as well*: the replaced executable is the running host binary, which no root
override moves, and clients started without an override pin the host-global
root. A root override therefore isolates state, not host-binary replacement —
a live client on either authority refuses the upgrade, and so does an
authority whose `.generation.lock` this process cannot write (a read-only
`~/.orbit`, say): the update could never record the candidate there, so it is
refused before anything is staged rather than after the binary is replaced.
`orbit update` admits against that same set for the invocation; a green
preflight is not evidence for an update that would resolve different roots. It opens no runtime or
stores and may create coordination lock files. It is an observation, not a
reservation. `orbit update` reacquires and holds admission through
replacement, then pins the candidate in every locked authority through
convergence. External installers must quiesce clients; a standalone preflight
is not race-free. `orbit update --check` checks releases, not running-client
compatibility.

Participating CLI/MCP processes pin their executable generation for their entire
lifetime. An update refuses while any is live, and a different executable cannot
auto-migrate underneath them. This covers stdio/operator, TCP listener,
federated local, destination SSH and managed processes without changing their
authority. Quiesce via the owning client/operator and retry; Orbit does not kill
sessions, hand off connections, reclaim claims or replay mutations. For a lost
reply, inspect the durable operation/audit before any retry. Never delete the
root's `.generation.lock` or `.generation-admission.lock` to force admission.

Existing pre-fix processes do not hold these locks: the first installation needs
explicit quiescence and reconnection to the same configured authority. A desktop
restart alone does not prove an unmanaged backend exited. No shadow stores or
ad-hoc MCP servers are part of this contract. Additive-newer compatibility permits
some unaudited CLI reads; MCP tool calls, including workspace discovery, require
durable audit writes and cannot use that read-only fallback.

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

The global JSONL trace at `~/.orbit/state/logs/orbit.jsonl` rotates from
long-lived processes (`orbit mcp serve`, `orbit sweep`, `orbit web serve`)
and when the active file exceeds its budget (one `metadata()` check on first
write). Short-lived commands, including `orbit --help`, do not open the file
or walk the log directory. Defaults: seven days of archives, a 500 MiB
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
[operational-logs.md](operational-logs.md).

## Search indexes

```bash
orbit semantic stats                       # companion and index status
orbit semantic index --kind tasks|docs|all
orbit docs index                           # doc corpus embeddings
```

Both are idempotent and safe to re-run. Reindex after bulk imports, large doc
moves, or a restore. → [search.md](../../orbit/references/search.md)

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
ID mapping. To keep a read-only mirror of another host's tasks in step, use
`--on-conflict owner-wins`: it replaces only bundles whose task-ID prefix belongs
to that host, never touches locally minted IDs, and is safe to re-run. Import is a different operation from publication's strict
same-authority, identical-retry recovery; do not substitute it to bypass a
publication ownership or divergence error.
