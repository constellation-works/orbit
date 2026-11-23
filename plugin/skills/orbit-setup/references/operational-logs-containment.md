# Operational logs: worker resource containment

## Contents

- [Worker Resource Containment](#worker-resource-containment)

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
allow-listed environment.

By default Orbit resolves its `PATH` and allowed
toolchain variables from the owner's interactive login shell, falling back to
a non-interactive login shell when that probe fails. Each probe is bounded to
10 seconds and cached for two minutes. Keep `rg`, `cargo`,
and other required tools on that resolved `PATH`. Failed validation
logs include `Required validation PATH=…`, including failures from nested build
scripts.

Compare that value with the service's `Environment=PATH=…` when a tool
cannot be found. Restart the launching service between runs after changing its
environment, then confirm a new box delivery passes `candidate_validate`.

The probe always tries the interactive login shell first and falls back to a
login-only shell. `workflow.validation_env.path` adds explicit PATH entries
(leading `~/` expands to the owner's home) ahead of the resolved PATH. Only allow-listed
toolchain variables cross the probe; it does not copy the shell's environment.
Agent sessions start with the same `path` entries ahead of their PATH, so a
reviewer runs the `python3` validation runs.

While `path` has an entry, Codex
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
`orbit web serve` between runs (never mid-run) to apply it there.

Each scope
also sets `OOMPolicy=continue`: the kernel kills the largest process in the run
rather than systemd stopping the whole scope, so the worker survives to record
the cause.

Every Linux worker, contained or not, also starts with an `oom_score_adj` of
at least 500, and everything it spawns inherits it. On a host-wide OOM the
kernel therefore kills a worker process before the user manager
(`user@.service` runs at 100), the drain that launched it, or the dashboard.
Without it, a host whose largest process was already killed can lose the user
`systemd` itself, and every worker scope with it (2026-10-12).

Scope units
accept no `OOMScoreAdjust=`, so the worker sets the value on itself before
`systemd-run` execs it; check a live worker with
`cat /proc/<worker_pid>/oom_score_adj`.

`machine.worker_cpu_quota` caps a run's CPU so a stress loop or runaway test
loop in one leaf cannot take every core and starve the other leaves and the
drain's admission control (2026-10-10 load average 89.7 on 32 cores).

It is
off by default; pick a value near cores ÷ drain concurrency, for example
`orbit config set --global machine.worker_cpu_quota 400`. systemd enforces it
only where the user manager delegates the cpu controller (check
`cat /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers`);
otherwise the property is accepted and not enforced. macOS has no systemd
scopes, so the setting does nothing there.

`orbit doctor` (row
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
end as `interrupted` / `process_not_found`.

Its diagnostic's `worker exit:`
clause then says no exit status was observed and what the scope recorded at
claim time still reports: unit result and counters, or that the scope is gone
and the `journalctl --user -u <unit>` command for it. Correlate with the kernel
log (`journalctl -k --since '1 hour ago' | rg -i 'oom|orbit-worker'`).

When a drain and its leaves die together (the user manager was killed), no
cancel is needed. `orbit run history` or `orbit run show` on the follower
marks each run whose worker is gone `interrupted`. It also settles the pull
claims those runs left with the owner: a dead leaf's recorded failure, and the
release of a claim its drain admitted but never launched.

Only claims that no
live drain pulls for and whose leaf is no longer running are settled, so
these reads contact no owner while a drain or leaf is alive. Pass
`--no-reconcile` to read the stored records without either step.

By default, containment falls back to the old behaviour, with one warning per
Orbit process (`orbit log tail --level warn --target orbit.core.job_run`: "pipeline
workers launch without a bounded systemd scope"), when it is disabled or when
`systemd-run --user --scope` cannot reach a user manager on Linux (containers,
sandboxes, sessions without `XDG_RUNTIME_DIR`). macOS instead uses an in-worker watchdog when containment is enabled.

Every
second it checks the tracked process/session set's total physical footprint and
process count against `machine.worker_memory_max` and `machine.worker_tasks_max`.
It records the eight largest consumers, sends descendants SIGTERM, escalates to
SIGKILL after one second, finalizes the run as failed and terminates the worker
last. The worker owns the watchdog, so exiting the submitting CLI does not stop
it. `orbit doctor` and submission notes label this **watchdog**, never kernel
containment.

`off` disables it; memory high-water throttling and CPU quota are
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
cannot run its watchdog.

These are watchdog gaps, not guarantees of kernel
containment. Other platforms without systemd still launch uncontained silently.
With `machine.worker_containment = "strict"` or `--strict-worker-containment` on
`orbit run ship` or `orbit run auto`, an unavailable scope instead refuses the
worker before spawn, including macOS (strict keeps its kernel-only meaning);
`orbit run show` reports `worker_containment_unavailable` and the reason.

The CLI flag is inherited by
the auto coordinator's leaf workers. Uncontained workers share the launching
service's cgroup. The rendered `orbit-sweep.service`
carries `MemoryHigh=70%` and `TasksMax=4096` as a backstop, and re-enabling the
clock (`orbit clock enable`) rewrites an older installed unit.

`orbit-web.service`
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
workers; a separate session does not protect them from cgroup cleanup.

Stopping
a worker scope (`systemctl --user stop orbit-worker-….scope`) kills that run; use
`orbit run cancel <run-id> --confirm` instead.
