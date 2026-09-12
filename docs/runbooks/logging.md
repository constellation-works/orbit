---
type: runbook
summary: Locate, filter, rotate, and retain Orbit process and routine-sweep logs.
tags: [operations, logs, tracing, rotation, routines]
paths: ["crates/orbit-common/src/observability/log_rotation.rs", "crates/orbit-core/src/application/routines/sweep.rs"]
related_features: [auditability, routines]
related_artifacts: [ORB-00423]
last_validated: 2026-09-12
---

# Inspect and Retain Logs

Use this runbook to locate Orbit process logs, filter structured events, or verify that
process and routine-sweep logs remain bounded.

## Global process log

All Orbit processes—the CLI, `orbit web serve`, and the MCP server—append structured tracing
events to one global JSONL sink:

```text
~/.orbit/state/logs/orbit.jsonl        # default writer and reader path
```

`orbit log tail` can read another sink with `--path` or `$ORBIT_LOG_PATH`.

One JSON object is written per line:
`{"timestamp", "level", "target", "fields": {..., "message"}}`. Secret-looking values
(environment variables matching `TOKEN`, `SECRET`, `PASSWORD`, or `API_KEY`;
`Authorization` or `x-api-key` headers; and `sk-…` keys) are redacted before reaching the
sink.

Before diagnosing a missing or unexpected log, confirm the actual binary, reader path
(`--path` or `$ORBIT_LOG_PATH`), `RUST_LOG`, root, and config file used by the process.

## Read and filter logs

```sh
orbit log tail -n 100                        # four-column view of recent events
orbit log tail -f --level warn               # follow, warnings and up
orbit log tail --target orbit.policy --since 1h
orbit log tail --json                        # raw JSONL lines

# jq directly on the sink:
jq -r 'select(.level=="ERROR")
       | "\(.timestamp) \(.target) \(.fields.message)"' ~/.orbit/state/logs/orbit.jsonl
```

`RUST_LOG` controls the tracing filter for any Orbit process. For example,
`RUST_LOG=debug orbit task list` uses standard `EnvFilter` syntax.

## Rotation and retention

Rotation is size-based and checked once at process start. When the active file exceeds the
per-file cap, it is renamed to `orbit.jsonl.<UTC-timestamp>`. Archives older than the
retention window are deleted, then the oldest archives are deleted until the total-size cap
holds.

Defaults are **100 MB per file, 500 MB total, and 7 days retention**. Override them in
`~/.orbit/config.toml`:

```toml
[runtime]
log_retention_days = 7
log_max_total_mb = 500
log_max_file_mb = 100
```

Rotation is implemented in `crates/orbit-common/src/observability/log_rotation.rs`.

## Routine sweep log on macOS

The `com.orbit.sweep` launchd agent, installed by
`orbit routine init --install-clock`, redirects `orbit sweep` stdout and stderr to a
separate file because it is not the JSONL tracing sink:

```text
~/.orbit/logs/sweep.log
```

Two behaviors keep it bounded on an always-on host [ORB-00423]:

- `orbit sweep` prints only fires, retries, baselines, errors, and a one-line heartbeat when
  nothing was due; `--verbose` restores one row per routine.
- Each pass opportunistically rolls and prunes `sweep.log` through the same rotation machinery
  and `[runtime]` caps as the JSONL sink, producing `sweep.log.<UTC-timestamp>` archives.

On Linux, the sweep unit logs to the journal, which rotates independently.

## Verification

Confirm that the effective active path exists and receives a new expected event. For retention,
compare the active file and archives against the configured per-file, total-size, and age caps;
remember that the global JSONL rotation check occurs at process start, while sweep rotation runs
opportunistically on each pass.

Related: [Inspect the audit trail](./audit-trail.md) for durable invocation and pipeline events.
