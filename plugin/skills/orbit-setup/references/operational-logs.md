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
journalctl --user -u orbit-sweep.service --since '1 hour ago' --no-pager -o short-iso
journalctl --user -u orbit-sweep.service --since '1 hour ago' --no-pager -o short-iso \
  | rg -n -C 4 'WARN|ERROR|failed|panic|No such file'
```

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

## Worker Resource Containment

On Linux with a reachable systemd user manager, every detached pipeline worker
and everything it spawns (agent CLIs, cargo, rustc, test binaries) runs in its
own transient scope, `orbit-worker-<run_id>-<nonce>.scope`, under the user
manager's `app.slice`. One runaway run is throttled or OOM-killed inside that
scope; the dashboard, the sweep clock, SSH, and sibling runs keep working.

The limits live only in the global `~/.orbit/config.toml` `[machine]` table:

| Key | Default | Scope property |
| --- | --- | --- |
| `machine.worker_containment` | `true` | `false` launches workers in the caller's cgroup |
| `machine.worker_memory_high` | `40%` | `MemoryHigh=`: the kernel throttles the run above it |
| `machine.worker_memory_max` | `50%` | `MemoryMax=`: OOM kills stay inside the run |
| `machine.worker_tasks_max` | `4096` | `TasksMax=`: processes plus threads before fork/clone fails |

Memory values take bytes with an optional `K`/`M`/`G`/`T` suffix, a
percentage of physical RAM (resolved by systemd, so the defaults scale with
the host), or `infinity`. Change one with
`orbit config set --global machine.worker_memory_max 12G`; a worker picks up
the value its launching process loaded, so restart a long-lived
`orbit web serve` between runs (never mid-run) to apply it there. Each scope
also sets `OOMPolicy=continue`: the kernel kills the largest process in the run
rather than systemd stopping the whole scope, so the worker survives to record
the cause.

Inspect a live run's scope:

```bash
systemctl --user list-units --type=scope 'orbit-worker-*' --no-pager
unit="$(systemctl --user list-units --type=scope --plain --no-legend 'orbit-worker-<run_id>-*' \
  | awk '{print $1}')"
systemctl --user show -p MemoryHigh -p MemoryMax -p TasksMax -p MemoryCurrent -p TasksCurrent "$unit"
cat "/proc/<worker_pid>/cgroup"   # 0::/…/app.slice/orbit-worker-….scope
scope_dir="/sys/fs/cgroup$(sed -n 's/^0:://p' "/proc/<worker_pid>/cgroup")"
cat "$scope_dir"/{memory.max,pids.max,memory.events,pids.events}
```

A run that fails after its scope hit a limit carries the error code
`worker_resource_limit` in `orbit run show`, naming `memory.max` / `pids.max`
and how many processes were OOM-killed or forks refused. The worker records it
itself; if the worker process died instead, its launcher records it when it is
still watching the child. A run whose launcher had already exited can still
end as `interrupted` / `process_not_found`; then correlate with the kernel log
(`journalctl -k --since '1 hour ago' | rg -i 'oom|orbit-worker'`).

Containment falls back to the old behaviour, with one warning per Orbit
process (`orbit log tail --level warn --target orbit.core.job_run`: "pipeline
workers launch without a bounded systemd scope"), when it is disabled, on
macOS, or when `systemd-run --user --scope` cannot reach a user manager
(containers, sandboxes, sessions without `XDG_RUNTIME_DIR`). Uncontained
workers share the launching service's cgroup. The rendered `orbit-sweep.service`
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
uncontained worker keeps the existing guarantee: it runs in its own `setsid`
session and the sweep unit uses `KillMode=process`. Stopping a worker scope
(`systemctl --user stop orbit-worker-….scope`) kills that run; use
`orbit run cancel` instead.

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
