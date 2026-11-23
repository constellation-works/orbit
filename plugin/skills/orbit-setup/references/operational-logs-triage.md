# Operational logs: triage

## Contents

- [Identify the Source](#identify-the-source)
- [Quick Host Triage](#quick-host-triage)
- [Global JSONL Tracing](#global-jsonl-tracing)
- [Audit Event Log](#audit-event-log)
- [One Job Run](#one-job-run)
- [Archive-Pruning Warning](#archive-pruning-warning)
- [Incident Record](#incident-record)

Use this reference for host-level incidents and warnings: an `orbit-sweep`
service failure, a global JSONL tracing warning, a missing log file, or a
question about which evidence to inspect. For a specific failed job run, start
with [run-debugging-triage.md](../../orbit-orchestrate/references/run-debugging-triage.md); the run bundle is normally
more decisive than host logs.

Treat runtime state as evidence. Do not edit files under `~/.orbit/state/`,
`.orbit/state/`, or the system journal to make a warning disappear. Logs can
contain task content and command output, so report only the decisive lines.

## Identify the Source

There are four separate operational evidence sources (plus, on Linux, each
run's [worker scope](operational-logs-containment.md#worker-resource-containment)):

| Source | Contents | First inspection |
| --- | --- | --- |
| OS service log | `orbit-sweep` starts, exits, and scheduler output | Linux: `journalctl --user -u orbit-sweep.service`; macOS: the launchd sweep log |
| Global JSONL trace | Structured tracing emitted by Orbit processes | Ordinarily `~/.orbit/state/logs/orbit.jsonl` and rotated archives; in a managed run with a different explicit `--root` or `ORBIT_ROOT`, the selected root's `state/logs/orbit.jsonl` and archives |
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
Ordinary processes use the home-global `~/.orbit/state/logs/orbit.jsonl` and
`orbit-agent.jsonl` feeds. An Orbit-managed run normally uses its inherited
registry root, but a different explicit `--root` or `ORBIT_ROOT` diverts both
feeds to that selected root's `state/logs/` directory so scratch startup logs
stay out of the inherited registry.

The diversion requires a managed-run
context; unmanaged root overrides keep the home-global feeds. Check the root
used by the managed invocation when locating its logs.

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
and a 100 MiB active-file threshold.

If the directory or active file is absent,
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

To inspect one invocation, list matching events as JSON and take the numeric
`id` from the event object. The JSON list is an array; filters such as
`--since` and `--status` can narrow it to the event you need:

```bash
orbit audit list --json --since 1h --status failure
orbit audit show <event_id> --json
```

Replace `<event_id>` with the selected event's `id`. Confirm that the `id` in
the returned detail matches, then review its command, timestamp, status, exit
code and any error or target fields relevant to the investigation. `show`
returns the full event record, while the default list view is a summary.

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
[run-debugging-triage.md](../../orbit-orchestrate/references/run-debugging-triage.md). It covers the run bundle, v2
audit trail, blobs, and live-process checks in the right order.

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
