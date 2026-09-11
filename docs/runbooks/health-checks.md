---
type: runbook
summary: Check Orbit workspace, database, dashboard, log-sink, job-run, and routine-clock health.
tags: [operations, health, doctor, dashboard, routines]
paths: ["crates/orbit-cmd/src/doctor.rs", "crates/orbit-core/src/application/job/run/reconcile.rs"]
related_features: [orbit-core, activity-job, routines]
related_artifacts: [ORB-10005, ORB-10070, ORB-10473, ORB-10501, ORB-10558, ORB-10986, ORB-11791, ORB-12109]
last_validated: 2026-09-11
---

# Check Orbit Health

Use this runbook for local diagnosis, readiness monitoring, or verification after a restore,
database recovery, or upgrade.

## Run `orbit doctor`

`orbit doctor` performs infrastructure checks and one row for each definition-artifact kind.
Every check degrades to a row rather than aborting unless the store itself cannot open.

| Check | What it verifies |
|---|---|
| `config` | layered config parses (`~/.orbit/config.toml` + workspace `config.toml`) |
| `database` | store DB `PRAGMA quick_check` + schema-ledger version versus this binary |
| `disk-space` | free space on the volume holding `.orbit` (warn below 1 GiB or 5%; fail below 256 MiB or 1%) |
| `semantic-index` | stale embedding rows; skipped if never indexed |
| `stale-locks` | `.lock` files under `state/`, `tasks/`, `learnings/`, and `adrs/.locks/` whose recorded holder PID is dead |
| `job-runs` | orphaned `pending` or `running` runs with no live worker process |
| `task-reservations` | active reservations whose owner run or terminal task association proves the reservation stale |
| `task-relations` | unresolved relation/dependency targets that would block a task-index rebuild |
| `orphan-task-stores` | task-store partitions (`~/.orbit/tasks/workspaces/<ws_id>/`) that no workspace binding on this host claims |
| `artifacts-*` | skills, jobs, activities, auto-tasks, and routines on disk: stale, deprecated, residual, catalog-invalid, or a previously reconciled shipped default that is missing |

Example:

```text
$ orbit doctor
│ CHECK            STATUS    DETAILS                                                          │
│ config           ok        valid (~/.orbit/config.toml)                                     │
│ database         ok        quick_check ok; schema version 1 matches this binary             │
│ disk-space       ok        11.2 GiB free of 65.6 GiB (17.1%) on the volume holding …/.orbit │
│ semantic-index   skipped   no semantic embeddings indexed yet                               │
│ stale-locks      warning   1 lock file(s) left by dead holders (the OS already released     │
│                            the flock; safe to delete): …/state/layout.lock                  │
│                            (dead pid 154488, op: layout upgrade, since 2026-07-04T09:25…)   │
│ job-runs         ok        no orphaned job runs                                              │
│ task-reservations ok       no conclusively stale active task reservations                    │
│ task-relations   ok        no unresolved relation/dependency targets                        │
│ orphan-task-stores ok      2 task-store partition(s) scanned, all claimed …                 │
0 failure(s), 1 warning(s).
```

The command exits nonzero only when at least one check is `ERROR`; warnings and skips exit
zero. `--json` emits an array of objects with `check`, `status`, `message`, and `remediation`
fields; statuses are lowercase and `remediation` is `null` for healthy/skipped rows. Human
output prints the same guidance as an `Action:` line. Lock files flagged by `stale-locks`
include holder diagnostics and are safe to delete only after confirming the holder PID is dead.

### Repair stale task reservations

The `task-reservations` check is read-only and intentionally narrower than task-lock listing.
It warns only when Orbit can prove one of these conditions:

- the recorded owner run no longer exists;
- the recorded owner run is terminal;
- the existing run-owner classifier proves a pending/running owner orphaned; or
- an unowned reservation has one or more associated tasks and every one is `done`, Orbit's
  terminal task status.

Fresh reservations, reservations owned by live or inconclusively probed runs, and unowned
reservations with empty, missing, mixed, or non-terminal task associations remain untouched.
Each warning names the `reservation-…` id, its task/run context, the stale reason, and the exact
repair command:

```sh
orbit doctor --fix-stale-task-locks
```

The repair re-reads and reclassifies each candidate immediately before releasing it, uses the
normal task-lock release audit path with the `doctor_stale_task_lock` reason, and is idempotent.
This is distinct from `--fix-stale-locks`, which handles dead-holder filesystem `.lock` files.

There is deliberately no blanket `--fix` or resolve-all option. Configuration repair, database
recovery, job cancellation, graph cleanup, id-allocation retirement, filesystem lock deletion,
task-reservation release, retired activity-backend cleanup, and orphan task-store deletion have
different evidence and safety gates, so each repair remains explicit and safety-scoped.

For definition convergence after installing a new Orbit binary, or to restore a shipped
default that was deleted by hand, use `orbit workspace sync` rather than a doctor repair.
Sync uses managed manifests to create newly shipped definitions, refresh or retire only
provably unedited Orbit-written instances, migrate legacy routine provenance, and preserve
operator content. `orbit workspace sync --check` is the read-only fleet inspection form.
Doctor reports a missing shipped default as an `artifacts-*` error (it does not consult the
warm-open defaults stamp, and it does not rewrite the file); run `orbit init` or
`orbit workspace sync` to restore it. Doctor performs only the specific repair named by an
individual flag; neither command upgrades the binary or pulls a remote repository.

### Reclaim orphaned task stores

`orbit workspace teardown --confirm` deregisters the workspace and deletes both the checkout's
`.orbit/` and the global task-store partition its task state is bound to, retiring that
partition's rows in `~/.orbit/tasks/index.sqlite` in the same step.

A partition directory is named for a **task-registry** workspace id
(`workspace_bindings.workspace_id` in `~/.orbit/tasks/index.sqlite`), minted as `<slug>-<hash>`
for a checkout that binds without an explicit id. That is a different id space from the workspace
catalog's `ws_*` ids in `~/.orbit/workspaces.json`. A task-registry binding claims its partition
only while the binding's recorded `orbit_dir` still exists on disk; a binding to a deleted checkout
is stale and is reported as orphaned. Catalog `ws_*` ids and the synthetic `ws_unbound-data-dir`
partition every `--root <data-dir>` write lands in remain claims, so they are never reported.
`orphan-task-stores` names each orphaned workspace id, its partition path, and its task-bundle count.

An unclaimed partition that **still holds task bundles** and has no stale checkout binding is
reported on its own terms and is never deleted by the repair. On disk it is indistinguishable from
a live checkout's partition whose registry row was lost, and every `orbit` subcommand recreates an
empty `~/.orbit/tasks/index.sqlite` before any check runs — so a lost or restored-without
`index.sqlite` registry makes every checkout other than the one you are standing in look abandoned.
That warning points at recovery instead:

```sh
cd <the checkout that owns the bundles>
orbit task reindex
```

`orbit task reindex` rebuilds the registry rows for one partition from its bundle directories, so
it must be run once per affected checkout. For a partition whose task-registry binding points at a
missing `orbit_dir`, the stale binding is the evidence that the checkout is genuinely gone. The
confirmed repair removes that partition, including its task bundles, and retires the stale registry
rows:

```sh
orbit doctor --fix-orphan-task-stores --confirm
```

Do not use that repair for an unknown populated partition; reindex it first.

An unclaimed partition with **no task bundles** carries nothing to recover. Repair those with:

```sh
orbit doctor --fix-orphan-task-stores --confirm
```

The repair deletes partition directories, so it refuses to run without `--confirm`. It resolves the
claims once, deletes empty partitions with no binding and partitions whose binding is stale, retires
any registry rows naming them, and is idempotent: running it again after a partition is gone is a
no-op.

### Repair retired activity backends

`artifacts-activities` uses the same load and tool-allowlist path as production activity
catalog construction, including workspace-local `.orbit/resources/activities/` files. A
schemaVersion 2 `agent_loop` activity that still declares `spec.backend: http` or
`spec.backend: auto` is a warning that names the file, the rejected field/value, the catalog
parse error, and one opt-in repair:

```sh
orbit doctor --fix-retired-activity-backends
```

The repair deletes only that obsolete `spec.backend` key, leaves unknown backend values and
unrelated malformed activities untouched (and reports them for a manual edit), and is
idempotent across every activity catalog directory in the workspace.

Graph is retired under [Retire and delete Orbit's code-graph subsystem](../design/_archive/orbit-graph/4_decisions.md#retire-and-delete-orbits-code-graph-subsystem) and is not inspected by ordinary health checks. To remove
leftover state explicitly, run `orbit doctor --remove-graph`. This deletes only the current
worktree's `.orbit/graph` and the shared workspace's `.orbit/knowledge/graph`; it is
idempotent when either is absent. Combine it with `--json` for a single JSON result with no
cleanup prose on stdout. Without `--remove-graph`, `orbit doctor` leaves both locations
untouched.

## Probe dashboard health

The loopback-only dashboard (`orbit web serve`, default `127.0.0.1:7878`) exposes liveness
and readiness:

```sh
curl -s localhost:7878/healthz                       # -> "ok"; cheap liveness, always 200
curl -s 'localhost:7878/healthz?detailed=true' | jq  # readiness; HTTP 503 if any check fails
```

Example detailed response:

```json
{
  "status": "ok",
  "workspaces_open": 1,
  "checks": [
    {"name": "sqlite_writable", "status": "ok",   "detail": "store database accepts writes", "workspace": "default"},
    {"name": "log_sink",        "status": "ok",   "detail": "~/.orbit/state/logs/orbit.jsonl accepts appends"}
  ]
}
```

Each detailed check is time-bounded to two seconds and runs per workspace.
`sqlite_writable` executes `BEGIN IMMEDIATE; ROLLBACK` without mutation. Point uptime
monitoring at the detailed form.

## Check the routine clock

Routines are Orbit's scheduling surface. Install or refresh the host clock with
`orbit routine init --install-clock`. Verify the native clock separately from routine due
state:

```sh
orbit routine clock status
orbit routine list
orbit sweep --json
```

On Linux, a healthy enabled status includes an active timer, a finite next systemd trigger,
and an effective cadence. `clock: unhealthy` with an inactive effective cadence means the
timer is enabled but elapsed, unscheduled, inactive, or could not be probed. Inspect the
printed diagnostic, then run `orbit routine clock enable`: it rewrites a stale installed
timer if needed, restarts the timer even when it is already enabled, and returns success
only after verifying a finite next trigger. If that verification fails, inspect the
`systemctl --user status` and `journalctl --user` commands in the error rather than repeating
enable. The generated timer schedules its first sweep from every timer activation and then
recurs from service activation; installation and cadence changes perform the same
post-activation verification.
It does not replay every tick missed during host or manager downtime: on the next sweep,
routine `missed_run: catch_up_once` fires once for a gap while `skip` waits for the next natural
cron slot.

If an `overlap: forbid` routine remains `overlap_in_flight` after a restart, run one explicit
`orbit sweep --json` and inspect the referenced run. Sweep releases a dispatched in-flight
fire immediately only when the recorded owner process is conclusively gone; a live or
unprobeable owner remains protected until terminal or until the routine timeout. Use
`orbit doctor` and the stuck-job-run runbook below when the run itself remains orphaned.

The dashboard also exposes `GET /api/routines`. `qa-sweep` and other auto-task
definitions are ordinary workspace data under `.orbit/auto_tasks/`, processed by the
generic auto-task scheduler routine. The scheduler mints tasks from every due,
enabled definition; it does not itself edit docs, ADRs, learnings, friction records,
or comments.

## Verification and escalation

Record the exact failing check, full details, binary version, config path, workspace, and
relevant environment overrides before diagnosing. Use
[Recover a corrupted database](./database-recovery.md) for integrity failures and
[Recover stuck job runs](./stuck-job-runs.md) for orphaned-run findings.
