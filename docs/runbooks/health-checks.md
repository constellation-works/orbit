---
type: runbook
summary: Check Orbit workspace, database, dashboard, log-sink, job-run, and routine-clock health.
tags: [operations, health, doctor, dashboard, routines]
paths: ["crates/orbit-cmd/src/doctor/mod.rs", "crates/orbit-core/src/application/job/run/reconcile.rs"]
related_features: [orbit-core, activity-job, routines]
related_artifacts: [ORB-10005, ORB-10070, ORB-10473, ORB-10501, ORB-10558, ORB-10986, ORB-11791, ORB-12109, ORB-12223, ORB-12244]
last_validated: 2026-09-20
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
| `tracked-orbit-files` | git still tracks files under `.orbit/`; `.orbit/` is per-user state. Remedy: `git rm -r --cached .orbit` |
| `empty-task-stubs` | empty `ORB-*` directories under those partitions, or ones that hold only `.task.yaml.lock` (aborted creates). Data-bearing dirs missing `task.yaml` are not stubs; `orbit task reindex` still clears this row |
| `unresolved-task-bundles` | `ORB-*` directories missing `task.yaml` that still hold bundle content (`events.jsonl`, `artifacts/`, …). Retained task data: restore `task.yaml` or move the directory aside; `orbit task reindex` will not delete them |
| `artifacts-*` | skills, jobs, activities, auto-tasks, and routines on disk: stale, deprecated, residual, catalog-invalid, or a previously reconciled shipped default that is missing |
| `clock-unit` | the installed launchd/systemd sweep unit invokes this Orbit binary (path and `--version`); skipped when no unit is installed |

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

`orbit workspace teardown <workspace> --confirm` requires an explicit registered name,
`ws_*` id, or absolute checkout path — it never infers the target from cwd. It prints the
resolved catalog name and id, checkout root, and task-store partition path, then deregisters
the workspace and deletes both the checkout's `.orbit/` and the global task-store partition
its task state is bound to, retiring that partition's rows in `~/.orbit/tasks/index.sqlite`
in the same step. The summary names the catalog workspace the deleted partition belonged to.

A partition directory is named for a **task-store partition id**
(`workspace_bindings.workspace_id` in `~/.orbit/tasks/index.sqlite`), minted as `<slug>-<hash>`
for a checkout that binds without an explicit id. That is a different namespace from the
workspace-registry id in `~/.orbit/workspaces.json`: `orbit workspace init` passes its catalog
`ws_*` id in as the partition id, so those workspaces spell both the same, while a legacy
`<slug>-<hash>` partition and the synthetic `ws_unbound-data-dir` partition have no catalog row at
all. Partition ownership is therefore always resolved through the task registry. A task-registry checkout claim is live while its recorded `repo_root`
is present on disk; a catalog checkout supplies the same per-checkout evidence when `workspace init`
has only a path-free task-registry registration. The shared external Orbit root (`--root
<data-dir>`) is not used as checkout evidence because several checkouts may share it. A missing
checkout is reported as stale, and an unreadable path is reported as unreachable. Checkoutless
catalog entries and the synthetic `ws_unbound-data-dir` partition every `--root <data-dir>` write
lands in remain claims. `orphan-task-stores` names each
reported workspace id, its partition path, and its task-bundle count.

**Evidence rule for deleting task bundles.** A partition that holds task bundles is deleted by the
repair only when its bound checkout is *confirmed gone*: `repo_root` stats as absent, and the
nearest ancestor directory that does exist is readable and therefore able to testify that the path
below it is missing. Any other filesystem answer — `EACCES` from an unsearchable parent, `EIO` or
`ENOTCONN` from a dropped mount, a path that resolves through a non-directory — classifies the
binding as **unreachable**: the checkout may be intact behind the failure, so the partition is
retained, reported with the failing path and error, and never deleted by
`--fix-orphan-task-stores`. A partition with **no task bundles** carries nothing to recover, so the
older rule still applies to it: any binding that is not a live claim reclaims the directory.

An unclaimed partition that **still holds task bundles** and has no checkout binding at all is
likewise never deleted by the repair. On disk it is indistinguishable from a live checkout's
partition whose registry row was lost, and every `orbit` subcommand recreates an empty
`~/.orbit/tasks/index.sqlite` before any check runs — so a lost or restored-without `index.sqlite`
registry makes every checkout other than the one you are standing in look abandoned. That warning
points at recovery instead:

```sh
cd <the checkout that owns the bundles>
orbit task reindex
```

`orbit task reindex` rebuilds the registry rows for one partition from its bundle directories, so
it must be run once per affected checkout.

For an **unreachable** partition, restore access first — remount the volume, repair the directory
permissions that hide the checkout — and re-run `orbit doctor`. A reachable checkout becomes a live
claim again and the row clears itself; a checkout that is genuinely gone once its parent is
readable becomes a confirmed-stale binding, which the repair can then remove.

For a partition whose binding is confirmed stale, the missing `repo_root` is the evidence that the
checkout is gone. The confirmed repair removes that partition, including its task bundles, and
retires the stale registry rows — as it does for every empty unclaimed partition.

**Recovery ordering for a deleted checkout.** Doctor can already classify the partition as stale
while the workspace is still in the catalog (the catalog checkout's `repo_root` is the evidence).
You may then either run the repair immediately, or first deregister the dead workspace:

```sh
ORBIT_OPERATOR=1 orbit workspace remove <workspace-name-or-id-or-absolute-checkout-path>
```

`workspace remove` changes only the catalog; it does not delete `.orbit` or task bundles. It
**retains** the task-registry workspace binding and copies the catalog checkout into that
registry so the leftover partition stays stale rather than flipping back to a claimed imported
archive. A populated leftover is reported with its path, bundle count, and the reclaim command
below. After removal, `orbit doctor` still warns on that partition; it must not report `ok`.

```sh
orbit doctor --fix-orphan-task-stores --confirm
```

Either order works: repair while the catalog still lists the checkout, or `workspace remove`
then the same repair. Skipping the repair after `workspace remove` leaves the bundles on disk
and unreachable through workspace selectors.

Do not use that repair for an unknown or unreachable populated partition; reindex or restore it
first. One residual limitation: a volume unmounted from a mountpoint that is itself still present
and readable reports its checkout as absent, and the partition is then treated as confirmed stale.
Remount before running the repair on a host with removable or network-mounted checkouts.

The repair deletes partition directories, so it refuses to run without `--confirm`. It resolves the
claims once, deletes empty unclaimed partitions and populated partitions whose checkout is
confirmed gone, retires any registry rows naming them, and is idempotent: running it again after a partition is gone is a
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

Graph is retired under the "Retire and delete Orbit's code-graph subsystem" decision ([ORB-10491]) and is not inspected by ordinary health checks. To remove
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

## Check the host clock

Routines are Orbit's scheduling surface. Install or refresh the host clock with
`orbit routine init --install-clock`. Verify the native clock separately from routine due
state:

```sh
orbit clock status
orbit routine list
orbit clock tick --json
orbit doctor
```

`orbit clock status` prints the unit's program path and the version that program
reports next to `platform:`. A version that does not match this binary is flagged on the
same line as `mismatch: running <version> at <path>`. A unit that still invokes the
compatibility alias `orbit sweep` is reported as stale. `orbit clock status --format json`
emits the same report as one JSON object: the clock's `state` (`enabled`, `paused`, or
`unhealthy`), cadence, platform, and health fields, plus a `program` object with the unit's
program path, version, and comparison `verdict`. `orbit doctor`'s `clock-unit` row is the
same comparison in check form:

- **ok** — the unit invokes this binary (canonical path and version match)
- **warning** — the unit's program path differs but `--version` matches (two installs; the
  clock can drift), or the named program is missing/unrunnable
- **ERROR** — the unit's program reports a different version than this binary. The row names
  the unit file, both program paths, and both versions. Rewrite the unit with
  `orbit clock repair`, or repoint the package-manager install the unit
  names so it is this version
- **skipped** — no launchd plist or systemd user service is installed

`orbit clock repair` is the repair for every one of those rows. It rewrites the installed unit
to this binary and re-registers it with launchd/systemd, prints what it changed, and exits
non-zero when the manager would not reload the rewritten unit. A unit that already names this
binary is left alone, and a paused clock is corrected on disk without being resumed — use
`orbit clock enable` to resume it.

A failed reload is remembered in `~/.orbit/clock.reload-pending`: re-running `orbit clock
repair` (or `orbit update`) retries the `launchctl load` / `systemctl --user restart` even
though the unit file already names this binary, reports `reloaded` once the manager accepts
it, and keeps exiting non-zero until then. `orbit clock enable` and `orbit clock disable` clear
the pending retry — the operator's explicit choice wins over a repair that is still catching up.

Operators do not have to reach for it after an ordinary upgrade: `orbit update` runs
`orbit clock repair` as its last convergence step, so a unit orphaned by an install at a new
path is repaired in the same command that moved the binary. The path that still needs a
manual run is a binary installed by something other than `orbit update` (`brew`, `cargo
install`, a checkout build) while the unit names the old one.

A hand-run `orbit sweep` / `orbit clock tick` from a binary the unit does not name prints one
warning line to stderr naming the unit, the program it runs, and this binary. The scheduled
pass never prints it — it *is* the program the unit names — so seeing it means unattended
sweeps are running a different build than the one you just invoked.

`orbit update --check` reports the binary on `PATH` / the cargo install; it does not inspect
what the clock unit invokes. After an upgrade, run `orbit doctor` or `orbit clock
status` before assuming unattended sweeps are running the new binary.

A sweep that discovers workspaces but fails to open every one of them exits non-zero and
prints a single `sweep.no_workspace_loaded` row naming this binary's version and the first
load error. Partial load errors still print one `load error […]` line per workspace and
exit 0. An unconfigured host (no registered workspaces) remains a clean no-op.

On Linux, a healthy enabled status includes an active timer, a finite next systemd trigger,
and an effective cadence. `clock: unhealthy` with an inactive effective cadence means the
timer is enabled but elapsed, unscheduled, inactive, or could not be probed. Inspect the
printed diagnostic, then run `orbit clock enable`: it rewrites stale installed service and
timer files if needed, restarts the timer even when it is already enabled, and returns success
only after verifying a finite next trigger. (`orbit clock enable` resumes a paused clock;
`orbit clock repair` only repoints a unit whose program moved, and never resumes one.) If that verification fails, inspect the
`systemctl --user status` and `journalctl --user` commands in the error rather than repeating
enable. The generated timer schedules its first sweep from every timer activation and then
recurs from service activation; installation and cadence changes perform the same
post-activation verification.

On macOS, launchd keeps reporting a loaded agent as loaded long after its program stops
working, so `clock: enabled` is not on its own evidence that sweeps are firing. An enabled
launchd clock is `unhealthy` with an inactive effective cadence when the plist names a
program that cannot report a version (the same detection behind `orbit doctor`'s
`clock-unit` row), when `launchctl print gui/<uid>/com.orbit.sweep` reports a non-zero
`last exit code` for the most recent run, when that dump lists `penalty box` under
`properties`, or when the dump cannot be read at all. Each case prints the reason and the
same recovery: `orbit clock enable` rewrites the unit to this binary and reloads it. The
dashboard's Host Sweep Clock card shows the same states — a degraded clock is badged
`missed`, not `healthy`, even though the launchd service itself is still enabled.
It does not replay every tick missed during host or manager downtime: on the next sweep,
routine `missed_run: catch_up_once` fires once for a gap while `skip` waits for the next natural
cron slot.

If an `overlap: forbid` routine remains `overlap_in_flight` after a restart, run one explicit
`orbit clock tick --json` and inspect the referenced run. The tick releases a dispatched in-flight
fire immediately only when the recorded owner process is conclusively gone; a live or
unprobeable owner remains protected until terminal or until the routine timeout. Use
`orbit doctor` and the stuck-job-run runbook below when the run itself remains orphaned.

The dashboard also exposes `GET /api/routines`. `qa-sweep` and other auto-task
definitions are ordinary workspace data under `.orbit/auto_tasks/`, evaluated in-process
by the host tick. It mints tasks from every due, enabled definition and creates no
scheduler run; it does not itself edit docs, design records, learnings, friction records,
or comments.

## Verification and escalation

Record the exact failing check, full details, binary version, config path, workspace, and
relevant environment overrides before diagnosing. Use
[Recover a corrupted database](./database-recovery.md) for integrity failures and
[Recover stuck job runs](./stuck-job-runs.md) for orphaned-run findings.
