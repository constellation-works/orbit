# Checking Orbit Operational Logs

Use this reference for host-level incidents and warnings: an `orbit-sweep`
service failure, a global JSONL tracing warning, a missing log file, or a
question about which evidence to inspect. For a specific failed job run, start
with [run-debugging.md](../../orbit-orchestrate/references/run-debugging.md); the run bundle is normally
more decisive than host logs.

Treat runtime state as evidence. Do not edit files under `~/.orbit/state/`,
`.orbit/state/`, or the system journal to make a warning disappear. Logs can
contain task content and command output, so report only the decisive lines.

## Identify the Source

There are four separate operational evidence sources (plus, on Linux, each
run's [worker scope](#worker-resource-containment)):

| Source | Contents | First inspection |
| --- | --- | --- |
| OS service log | `orbit-sweep` starts, exits, and scheduler output | Linux: `journalctl --user -u orbit-sweep.service`; macOS: the launchd sweep log |
| Global JSONL trace | Structured tracing emitted by Orbit processes | `~/.orbit/state/logs/orbit.jsonl` and rotated archives |
| Audit event store | Persistent CLI invocation metadata | `orbit audit list --since 1h --status failure` |
| Job-run evidence | One pipeline's state, events, stdout/stderr, and blobs | `orbit run show|events|trace|logs <run_id>` |

`orbit-sweep` is a short-lived service. A journal warning does not by itself
mean the scheduler failed: check its exit status and the following sweep line.

## Quick Host Triage

Set the global root once so every command inspects the same installation:

```bash
orbit_root="$HOME/.orbit"
orbit --version
date -u +'%Y-%m-%dT%H:%M:%SZ'
```

On Linux, inspect the user service and its recent journal:

```bash
systemctl --user status orbit-sweep.timer orbit-sweep.service --no-pager
systemctl --user cat orbit-sweep.service
systemctl --user show orbit-sweep.service -p ActiveState -p Result \
  -p ExecMainStartTimestampMonotonic -p TimeoutStartUSec -p KillMode
journalctl --user -u orbit-sweep.service --since '1 hour ago' --no-pager -o short-iso
journalctl --user -u orbit-sweep.service --since '1 hour ago' --no-pager -o short-iso \
  | rg -n -C 4 'WARN|ERROR|failed|panic|No such file'
```

`orbit clock status` and the doctor's `clock-unit` row flag a tick still running
after its five-minute budget, a failed service start, or missing recovery
settings. The doctor row also flags a systemd timer that is disabled or inactive,
and on macOS a launchd agent that is not loaded or a `sweep.log` older than the
tick interval. `orbit clock enable` arms it again. `orbit clock status` still
reports an operator pause as `paused`.

An exhausted cooperative budget exits nonzero and names deferred
workspaces in the tick report (`deadline_exceeded`, `skipped_workspaces` in
JSON). A synchronous operation that never returns hits systemd's ten-minute
startup timeout, which kills the service group and lets the timer recover.
The service should report a finite `TimeoutStartUSec` and `KillMode=mixed`;
`orbit clock repair` rewrites older units and preserves a paused clock.
Fetch timeouts kill the command's process group, including `git-remote-http`,
and lock waits consume the source pass's 30-second budget.

On macOS, the clock installer redirects sweep stdout/stderr to a file:

```bash
launchctl print "gui/$(id -u)/com.orbit.sweep"
tail -n 160 "$orbit_root/logs/sweep.log"
```

If the service is absent, verify the configuration without firing a routine:

```bash
orbit routine list
orbit sweep --dry-run
```

## Global JSONL Tracing

The JSONL tracing sink is distinct from the sweep-service log. Its active file
and rename-based archives are disposable; job/run stores are not.

```bash
# Preferred public reader: recent events, warnings, or one tracing target.
orbit log tail -n 120
orbit log tail --level warn --since 1h
orbit log tail --target orbit.logging.rotation --since 1h

# Inspect archive files and the raw sink only when filesystem detail is needed.
find "$orbit_root/state/logs" -maxdepth 1 -type f -exec ls -lh {} \;
sed -n '/^\[runtime\]/,/^\[/p' "$orbit_root/config.toml"
```

Rotation walks archives from long-lived processes (`orbit mcp serve`,
`orbit sweep`, `orbit web serve`) and when the active file exceeds its budget
on first JSONL write. Short-lived commands, including `orbit --help`, do not
open the file. Defaults are seven days of archives, a 500 MiB archive budget,
and a 100 MiB active-file threshold. If the directory or active file is absent,
first determine whether any process should have created it; a missing optional
log is not evidence of lost job history.

## Audit Event Log

`orbit audit` queries the persistent CLI-invocation audit store. It is separate
from the JSONL trace and is useful for host-wide command failures, denials, and
command history:

```bash
orbit audit list --since 1h --status failure
orbit audit list --json --limit 100
orbit audit stats --since 7d
```

Use `orbit log tail` for trace events, `orbit audit` for persistent invocation
metadata, and the run commands below for a single pipeline's detailed audit
trail.

## One Job Run

When a warning names a `jrun-*` id, inspect that run before drawing a
connection to host logs:

```bash
orbit run show <run_id> --json
orbit run events <run_id> --json
orbit run trace <run_id>
orbit run logs <run_id> --json
```

For a failed, cancelled, or stuck run, continue with
[run-debugging.md](../../orbit-orchestrate/references/run-debugging.md). It covers the run bundle, v2
audit trail, blobs, and live-process checks in the right order.

## Delivery Profiles and Admission Diagnostics

Inspect these read-only surfaces when work waits despite a free worker slot:

```bash
orbit run profiles --json
orbit run profiles --accuracy --json
orbit run readiness --json
orbit config keys --json
orbit config show
```

Profiles summarize per-delivery CPU and memory by workspace, work class and
complexity on this host; `auto-task:<template>` profiles omit complexity.
They use each key's last `profile_window` completed deliveries, whatever their
age, under the newest crew model and Orbit version, excluding resumed
or retried deliveries. Sparse keys fall back to broader profiles; inspect
sample counts and spread before drawing conclusions. `BUILD_FLAG` identifies
no-diff work using more than one core for two consecutive minutes, with its
deliveries listed; templates that build by design, such as QA sweeps, are exempt.
It is diagnostic evidence, not proof of a defect.

`--accuracy` compares forecasts recorded in `shadow` or `enforce` with measured
host load 5, 10, 20 and 30 minutes ahead, and reports shadow admission differences.
Error is forecast minus measured; negative errors are underestimates. Unmeasured
points are reported separately and cannot establish accuracy. Forecast modes
and settings are in [configuration.md](configuration.md#common-settings).
Forecast fit, the fixed worker-slot ceiling, and sustained resource throttling
are separate admission checks; diagnostics authorize no policy change.

Readiness names `host_shutdown_scheduled` with the scheduled time and mode.
New unattended admissions hold within `machine.host_shutdown_hold_minutes`
(default 180); later schedules are still visible while admissions continue.
Running work is left alone by the hold. Report the schedule and configured
window; preserve the hold and operator capacity policy rather than raising
concurrency, changing forecast thresholds, or cancelling a shutdown automatically.

## Provider failure holds

A local run can stop because its provider failed rather than the work. The
step error then carries a typed marker followed by `provider=<name>`. Only
the provider CLI's own stderr, terminal error, or failure frames set a
marker; an agent transcript or tool output that merely mentions a rate limit
does not. Treat this as read-only diagnosis: the hold lifts by itself, and
the next run resumes the kept candidate. Do not clear state files, unblock
or replay the task, file a duplicate, or dispatch repeatedly to force it
through.

| Marker | Meaning | Run outcome | Excluded crews | Base backoff |
| --- | --- | --- | --- | --- |
| `[provider_capacity]` | The selected model was at capacity. | `held` (`provider_capacity`) | The run's crew, or, when the step ran on another provider or account, only the named account's crew (`account=<crew>`), or the provider's crews without their own credentials for a default-login failure. | 15 minutes |
| `[provider_unavailable]` | Unusable on this host, such as a failed login. | `failed`; the task is still held | The run's crew, or, when the step ran on another provider or account, only the named account's crew (`account=<crew>`), or the provider's crews without their own credentials for a default-login failure. | 30 minutes |
| `[provider_refusal]` | The provider's content policy refused the task. | `failed`; the task is still held | Every crew of the refusing provider. | 24 hours |
| `[provider_limit]` | The account hit a usage limit. | `held` (`provider_limit`) | Every crew of the failing account; only crews whose model contains the named model when the failure names one. | Until the reported reset (below) |

Capacity, unavailability, and refusal backoffs double for each earlier hold
the task received in the last 24 hours, up to 24 hours. A usage-limit hold
lasts until the provider's reset, clamped between 5 minutes and 7 days;
with no readable reset it starts at 30 minutes and doubles per earlier
usage-limit hold on that provider account in the last 24 hours, up to
6 hours.

A crew that sets its own credentials is its own account: the marker then
carries `account=<crew>` after the provider, and a limit, capacity, or
unavailability failure excludes that crew alone. A failure without one is
the provider's default login and excludes only the provider's crews without
their own credentials. A refusal is the provider's content policy and still
excludes every crew of that provider. An unavailability hold also names the
host's machine: a failed login is that host's, so its crews are excluded
only there, while crews an earlier still-standing capacity, limit, or
refusal hold excluded stay excluded on every machine. A claimed run on
another host that a provider refused releases its claim the same way, and
the owner records the hold from its own crews.

Until the release time, the drain gives the task to a crew the hold does
not exclude: the task's own crew or pool first, then the other enabled
members of its purpose or type pool, then its complexity pool, then the
workspace default. The draw source names the hold. With no crew left, the
task waits and `orbit run readiness` reports it with `reason:
"provider_backoff"` and the release time. A run started with an explicit
crew input ignores the hold. The hold ends at the release time or at any
later status change; do not move the task just to lift it. The next run
resumes the committed candidate: a held run commits it on the run's branch
and pushes it to the candidate ref on the origin without opening a branch
or a pull request, so there is nothing to re-run or rebuild by hand.

Inspect the hold through these read-only surfaces:

```bash
orbit run show <run-id> --json
orbit task show <task-id>
orbit run readiness <task-id> --json
```

`orbit run show` names the marker (`provider=<name>`, with
`account=<crew>` for a crew account), the diagnostic code, and whether the
run ended `held` or `failed`. `orbit task show` leaves the task in
`backlog` under a `provider_failure_hold` history event whose note names
the class, provider, excluded crews, release time, and run. `orbit run
readiness` reports the waiting task with `reason: "provider_backoff"` and a
detail naming that run, class, excluded crews, and release time; eligibility
commands alone never clear it. Check `orbit run readiness --help`,
`orbit run show --help`, and `orbit task show --help` on the installed host
for the exact output shape before scripting around it.

Separate the hold from two lookalikes. A host outage from an
unavailability failure is recorded for this host only: until its backoff
lapses the local drain draws no task onto that crew, not only the held one,
and moving the held task through another status does not lift that. Sign
the provider in again on that host; tasks draw the crew once the outage
lapses. An unavailability hold placed on another host excludes only the
crews carried from an earlier still-standing capacity, limit, or refusal
hold; those stay excluded on every machine. A provider usage-budget wait
instead reports `reason:
"provider_limit"` with a `Provider limits:` line naming each gated window,
its reset, and the skipped crews; `orbit doctor` warns on the provider's
`provider-limits` row. Nothing failed and nothing needs repair there: the
wait lifts by itself after the reset. Thresholds and budgets are in
[configuration.md](configuration.md#common-settings). When the scheduled
task pilot withholds its dispatch, its reason names the provider or crew,
when the backoff ends, the failing run, and the skipped crews; the next
tick after the hold lifts dispatches again with no operator action.

## Worker Resource Containment

On Linux with a reachable systemd user manager, every detached pipeline worker
and everything it spawns (agent CLIs, cargo, rustc, test binaries) runs in its
own transient scope, `orbit-worker-<run_id>-<nonce>.scope`, under the user
manager's `app.slice`. One runaway run is throttled or OOM-killed inside that
scope; the dashboard, the sweep clock, SSH, and sibling runs keep working.

Scope launches carry the launching process's allow-listed child variables
(including `PATH`) and deliberate worker environment edits in the child
environment. `systemd-run` receives name-only `--setenv=NAME` arguments and
copies each value from that environment, so credentials are not placed on the
command line. Required validation runs `/bin/sh` with a cleared,
allow-listed environment. By default Orbit resolves its `PATH` and allowed
toolchain variables from the owner's interactive login shell, falling back to
a non-interactive login shell when that probe fails. Each probe is bounded to
10 seconds and cached for two minutes. Keep `rg`, `cargo`,
and other required tools on that resolved `PATH`. Failed validation
logs include `Required validation PATH=…`, including failures from nested build
scripts. Compare that value with the service's `Environment=PATH=…` when a tool
cannot be found. Restart the launching service between runs after changing its
environment, then confirm a new box delivery passes `candidate_validate`.

The config schema exposes `workflow.validation_env.shell_probe`: `interactive`
(default), `login` to select only the non-interactive probe, or `off` to
disable shell probing. `workflow.validation_env.path` adds explicit PATH entries
(leading `~/` expands to the owner's home); `workflow.validation_env.path_mode`
is `prepend` by default or `replace` to use only those entries. Only allow-listed
toolchain variables cross the probe; it does not copy the shell's environment.
Agent sessions start with the same `path` entries ahead of their PATH, so a
reviewer runs the `python3` validation runs. While `path` has an entry, Codex
is started with `allow_login_shell=false`, so its tool commands skip the login
profiles where macOS `path_helper` would demote those entries; list every
toolchain directory agents need in `path`.

The limits live only in the global `~/.orbit/config.toml` `[machine]` table:

| Key | Default | Scope property |
| --- | --- | --- |
| `machine.worker_containment` | `warn` | `off` launches workers in the caller's cgroup; `warn` falls back to it with a warning when a systemd user scope is unavailable; `strict` refuses the worker launch instead |
| `machine.worker_memory_high` | `40%` | `MemoryHigh=`: the kernel throttles the run above it |
| `machine.worker_memory_max` | `50%` | `MemoryMax=`: OOM kills stay inside the run |
| `machine.worker_tasks_max` | `4096` | `TasksMax=`: processes plus threads before fork/clone fails |
| `machine.worker_cpu_quota` | unset (`0`) | `CPUQuota=`: percent of one core (`400` = four cores); `0` or unset sets no CPU limit |

Memory values take bytes with an optional `K`/`M`/`G`/`T` suffix, a
percentage of physical RAM (resolved by systemd, so the defaults scale with
the host), or `infinity`. Change one with
`orbit config set --global machine.worker_memory_max 12G`; a worker picks up
the value its launching process loaded, so restart a long-lived
`orbit web serve` between runs (never mid-run) to apply it there. Each scope
also sets `OOMPolicy=continue`: the kernel kills the largest process in the run
rather than systemd stopping the whole scope, so the worker survives to record
the cause.

Every Linux worker, contained or not, also starts with an `oom_score_adj` of
at least 500, and everything it spawns inherits it. On a host-wide OOM the
kernel therefore kills a worker process before the user manager
(`user@.service` runs at 100), the drain that launched it, or the dashboard.
Without it, a host whose largest process was already killed can lose the user
`systemd` itself, and every worker scope with it (2026-10-12). Scope units
accept no `OOMScoreAdjust=`, so the worker sets the value on itself before
`systemd-run` execs it; check a live worker with
`cat /proc/<worker_pid>/oom_score_adj`.

`machine.worker_cpu_quota` caps a run's CPU so a stress loop or runaway test
loop in one leaf cannot take every core and starve the other leaves and the
drain's admission control (2026-10-10 load average 89.7 on 32 cores). It is
off by default; pick a value near cores ÷ drain concurrency, for example
`orbit config set --global machine.worker_cpu_quota 400`. systemd enforces it
only where the user manager delegates the cpu controller (check
`cat /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers`);
otherwise the property is accepted and not enforced. macOS has no systemd
scopes, so the setting does nothing there. `orbit doctor` (row
`worker-containment`) shows the configured limits.

Inspect a live run's scope:

```bash
systemctl --user list-units --type=scope 'orbit-worker-*' --no-pager
unit="$(systemctl --user list-units --type=scope --plain --no-legend 'orbit-worker-<run_id>-*' \
  | awk '{print $1}')"
systemctl --user show -p MemoryHigh -p MemoryMax -p TasksMax -p CPUQuotaPerSecUSec -p MemoryCurrent -p TasksCurrent "$unit"
cat "/proc/<worker_pid>/cgroup"   # 0::/…/app.slice/orbit-worker-….scope
scope_dir="/sys/fs/cgroup$(sed -n 's/^0:://p' "/proc/<worker_pid>/cgroup")"
cat "$scope_dir"/{memory.max,pids.max,memory.events,pids.events}
```

A run that fails after its scope hit a limit carries the error code
`worker_resource_limit` in `orbit run show`, naming `memory.max` / `pids.max`
and how many processes were OOM-killed or forks refused. The worker records it
itself; if the worker process died instead, its launcher records it when it is
still watching the child. A run whose launcher had already exited can still
end as `interrupted` / `process_not_found`. Its diagnostic's `worker exit:`
clause then says no exit status was observed and what the scope recorded at
claim time still reports: unit result and counters, or that the scope is gone
and the `journalctl --user -u <unit>` command for it. Correlate with the kernel
log (`journalctl -k --since '1 hour ago' | rg -i 'oom|orbit-worker'`).

When a drain and its leaves die together (the user manager was killed), no
cancel is needed. `orbit run history` or `orbit run show` on the follower
marks each run whose worker is gone `interrupted`. It also settles the pull
claims those runs left with the owner: a dead leaf's recorded failure, and the
release of a claim its drain admitted but never launched. Only claims that no
live drain pulls for and whose leaf is no longer running are settled, so
these reads contact no owner while a drain or leaf is alive. Pass
`--no-reconcile` to read the stored records without either step.

By default, containment falls back to the old behaviour, with one warning per
Orbit process (`orbit log tail --level warn --target orbit.core.job_run`: "pipeline
workers launch without a bounded systemd scope"), when it is disabled or when
`systemd-run --user --scope` cannot reach a user manager on Linux (containers,
sandboxes, sessions without `XDG_RUNTIME_DIR`). macOS instead uses an in-worker watchdog when containment is enabled. Every
second it checks the tracked process/session set's total physical footprint and
process count against `machine.worker_memory_max` and `machine.worker_tasks_max`.
It records the eight largest consumers, sends descendants SIGTERM, escalates to
SIGKILL after one second, finalizes the run as failed and terminates the worker
last. The worker owns the watchdog, so exiting the submitting CLI does not stop
it. `orbit doctor` and submission notes label this **watchdog**, never kernel
containment. `off` disables it; memory high-water throttling and CPU quota are
not implemented on macOS. Percent memory limits use physical RAM; `infinity`
disables only the memory ceiling. The five-second profile sampler is unchanged.

A watchdog breach carries `worker_resource_limit` in `orbit run show`, with
command names, PIDs and memory bytes for top consumers. The resource evidence is
kept with the run: the same bounded snapshot with start keys, CPU rates (absent
at the first reading), totals, limits and observation time, one record per run
until that run is deleted. `orbit run show --json` returns it under
`worker_resource_limit`. Arguments and environment are not collected.

Polling can overshoot between checks. Unreadable snapshots skip a check. After
a breach, persistent identity-binding or signal errors keep the watchdog
retrying and the run active until it can verify that tracked children stopped;
finalization can be delayed indefinitely. A process that calls `setsid` after
leaving the tree, and any children it starts, escape tracking; a session that
starts and loses its leader between checks can also be missed. A killed worker
cannot run its watchdog. These are watchdog gaps, not guarantees of kernel
containment. Other platforms without systemd still launch uncontained silently.
With `machine.worker_containment = "strict"` or `--strict-worker-containment` on
`orbit run ship` or `orbit run auto`, an unavailable scope instead refuses the
worker before spawn, including macOS (strict keeps its kernel-only meaning);
`orbit run show` reports `worker_containment_unavailable` and the reason. The CLI flag is inherited by
the auto coordinator's leaf workers. Uncontained workers share the launching
service's cgroup. The rendered `orbit-sweep.service`
carries `MemoryHigh=70%` and `TasksMax=4096` as a backstop, and re-enabling the
clock (`orbit clock enable`) rewrites an older installed unit. `orbit-web.service`
is operator-installed: bound it with a user drop-in such as
`~/.config/systemd/user/orbit-web.service.d/limits.conf` (`[Service]`
`MemoryHigh=`, `MemoryMax=`, `TasksMax=`), then `systemctl --user
daemon-reload`; apply it to the running unit with `systemctl --user
set-property orbit-web.service …` rather than a restart, which would interrupt
in-flight uncontained runs.

Restarts: a contained worker lives outside the `orbit-web.service` and
`orbit-sweep.service` cgroups, so restarting either never signals it. An
uncontained worker runs in its own `setsid` session but remains in the launching
service's cgroup. The sweep unit uses `KillMode=mixed` and
`TimeoutStartSec=10min`, so service stops or that timeout can kill uncontained
workers; a separate session does not protect them from cgroup cleanup. Stopping
a worker scope (`systemctl --user stop orbit-worker-….scope`) kills that run; use
`orbit run cancel <run-id> --confirm` instead.

## Archive-Pruning Warning

The text `failed to prune JSONL log archives` is emitted by the shared rotation
helper. It can therefore describe the macOS sweep log as well as the global
JSONL file. Identify the invoking process and platform before assuming that
`orbit.jsonl` is missing.

| Symptom | Confirm | Interpretation and safe next step |
| --- | --- | --- |
| Linux logs the warning once per `orbit-sweep` minute, then a normal sweep result with status `0` | `systemctl --user cat orbit-sweep.service`; `test -d "$orbit_root/logs"` | Linux writes sweep output to the journal, while `$orbit_root/logs/sweep.log` is a macOS target. An unconditional prune of that missing parent produces a harmless, noisy ENOENT. Record the version and file a fix to skip sweep-log rotation on Linux or make a missing parent a no-op. Do not create a dummy directory merely to suppress the warning. |
| A process cannot open or prune `$orbit_root/state/logs/orbit.jsonl` | `ls -ld "$orbit_root/state" "$orbit_root/state/logs"`; check the process `HOME` | Confirm the same user initialized the global root and that the path is readable/writable. Repair permissions or initialization only with explicit approval. |
| Archive deletion reports permission, read-only filesystem, or I/O errors | `find "$orbit_root/state/logs" -maxdepth 1 -type f -exec ls -l {} \;`; `df -h "$orbit_root"` | Retention may no longer bound disk use. Capture the exact path/error and address capacity or ownership through normal host operations. |
| Warning appears from `orbit mcp serve`, `orbit sweep`, `orbit web serve`, or a process that is writing JSONL, not a short-lived `--help` | Correlate the process command and run `orbit log tail --level warn --since 1h` | Treat it as global JSONL rotation; inspect the active path, archives, limits, and any concurrent removal of the directory. |

The first pattern is non-fatal: it affects an unused Linux sweep-log rotation
target. The scheduler, job-run records, audit events, and journal remain valid
evidence.

## Incident Record

Keep a report short and reproducible:

```markdown
Observed: <UTC timestamp, host, Orbit version>
Source: <journal | launchd sweep log | global JSONL | audit event store | run bundle>
Scope: <single process | every sweep | one run id>
Impact: <none | scheduler delayed | run failed | retention not enforced>
Evidence: <service exit status, decisive error, relevant path>
Cause: <confirmed cause or clearly labelled hypothesis>
Next step: <specific safe remediation or code change>
```

Separate a host-log warning from a job failure. A warning followed by a
successful sweep is not the root cause of an unrelated pipeline failure.
