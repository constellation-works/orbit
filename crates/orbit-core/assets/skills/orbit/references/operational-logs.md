# Checking Orbit Operational Logs

Use this reference for host-level incidents and warnings: an `orbit-sweep`
service failure, a global JSONL tracing warning, a missing log file, or a
question about which evidence to inspect. For a specific failed job run, start
with [run-debugging.md](run-debugging.md); the run bundle is normally
more decisive than host logs.

Treat runtime state as evidence. Do not edit files under `~/.orbit/state/`,
`.orbit/state/`, or the system journal to make a warning disappear. Logs can
contain task content and command output, so report only the decisive lines.

## Identify the Source

There are four separate operational evidence sources:

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

Rotation is opportunistic at process initialization. Defaults are seven days of
archives, a 500 MiB archive budget, and a 100 MiB active-file threshold. If the
directory or active file is absent, first determine whether any process should
have created it; a missing optional log is not evidence of lost job history.

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
[run-debugging.md](run-debugging.md). It covers the run bundle, v2
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
| Warning appears during a normal application start, not `orbit-sweep` | Correlate the process command and run `orbit log tail --level warn --since 1h` | Treat it as global JSONL rotation; inspect the active path, archives, limits, and any concurrent removal of the directory. |

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
