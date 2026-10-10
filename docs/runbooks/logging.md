---
type: runbook
summary: Locate, filter, rotate, and retain Orbit process and routine-sweep logs.
tags: [operations, logs, tracing, rotation, routines]
paths: ["crates/orbit-common/src/observability/log_rotation.rs", "crates/orbit-core/src/application/routines/sweep.rs"]
related_features: [auditability, routines]
related_artifacts: [ORB-00423]
last_validated: 2026-10-07
---

# Inspect and Retain Logs

Use this runbook to locate Orbit process logs, filter structured events, or verify that
process and routine-sweep logs remain bounded.

## Global process log

All Orbit processes—the CLI, `orbit web serve`, and the MCP server—append structured tracing
events to two global JSONL feeds with independent byte budgets:

```text
~/.orbit/state/logs/orbit.jsonl        # operational events
~/.orbit/state/logs/orbit-agent.jsonl  # agent stdout/stderr relay
```

`orbit log tail` and the dashboard merge both active feeds. `orbit log tail` can
read another sink with `--path` or `$ORBIT_LOG_PATH`; filenames other than
`orbit.jsonl` read a single file. When neither global feed exists, a one-shot
`orbit log tail` exits non-zero with `orbit log file not found`; `-f` waits for
either feed to appear. Relay targets and `RUST_LOG` filters retain
their existing behavior; the reserved `agent_output` tracing field routes
stdout/stderr records to the independent feed.

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

In follow mode, `orbit log tail -f` drains the open file after a rename, waits
if the active path is temporarily absent, and follows the replacement from
its beginning. If the open file shrinks below the read offset, it restarts
from the beginning. Filters and output mode continue to apply. Incomplete
records wait for a newline; unfinished bytes from an archive or truncated
contents are discarded when switching to the new contents. Writes to an
archive after the follower has switched to the replacement are not followed.

The dashboard log snapshot (`/api/log`) and Errors tab
(`/api/diagnostics/errors`) skip malformed JSON and non-UTF-8 lines, continuing
to show valid records on either side. File access and read errors still fail the
request. The snapshot reads only the active file. The Errors tab also reads
rotated archives that reach into its window, reading each only back to its first
record older than the window (a 60 s margin allows for out-of-order stamps) and not
opening an archive last written before the window. When retention has already pruned
the start of the window, its header shows "covers since" with the oldest
retained instant.

The Tasks dock's Log mode and the bottom status bar put the message before
structured context. Agent relays show the provider event kind (or the stream
name for plain output), its item kind when it has one, and a compact run
identifier first; the raw provider line is omitted when the kind is known.
Targets show their last segment; hover to see the complete target. Home paths use `~`, and managed
worktree paths show the run and relative path; shortened values retain the full
value in their tooltip. The **agent** toggle hides stdout relays independently
of the severity filters and remembers the choice in this browser. Stderr and
orchestration events remain subject to the severity filters. The status bar
follows the same toggle: while stdout relays are hidden it holds the newest
event that is not one, so a drain's agent traffic does not replace an Orbit
warning.

To validate the dashboard with a prepared Playwright module and Chromium, run
the isolated browser fixture (1440 px, snapshot, live and paused relays, paths,
filters, and reload persistence):

```sh
ORBIT_PLAYWRIGHT_MODULE="$PWD/.orbit/tmp/browser/node_modules/playwright/index.mjs" \
ORBIT_LOG_BROWSER_EVIDENCE_DIR="$PWD/.orbit/tmp/log-browser" \
PLAYWRIGHT_BROWSERS_PATH="$PWD/.orbit/tmp/browser/browsers" \
./scripts/build-budget.py -- cargo test -p orbit-web --test http_api \
  log::dashboard_log_message_priority_and_agent_filter -- --ignored --nocapture
```

On minimal Linux images, also supply the prepared browser's
`LD_LIBRARY_PATH` and `FONTCONFIG_FILE` for its private libraries and fonts.

## Rotation and retention

Rotation is size-based. Long-lived commands (`mcp serve`, `mcp listen`, `web serve`,
`sweep`, and `clock tick`) rotate at startup; short-lived commands check the active file on their first
JSONL write and rotate it only if it exceeds the per-file cap. The active file is renamed to
`orbit.jsonl.<UTC-timestamp>`. Archives older than the retention window are deleted, then
the oldest archives are deleted until the total-size cap holds.

Operational defaults are **100 MiB per file, 500 MiB of archives, and 7 days
retention**. Agent relay has its own **50 MiB per file and 200 MiB of archives**,
so transcript volume cannot evict operational events. The same age limit applies
to both feeds; either feed can be pruned sooner when its own size budget fills.
Writers recheck size and reopen after each MiB written, so long-lived producers
also enforce the cap. Relay archives use `orbit-agent.jsonl.<UTC-timestamp>`.
Override the operational size limits and shared age limit in
`~/.orbit/config.toml`:

```toml
[runtime]
log_retention_days = 7
log_max_total_mb = 500
log_max_file_mb = 100
```

Log snapshots return `offset` for operations and `agent_offset` for relay.
Resume `/api/log/stream` with `from` and `agent_from`; SSE `Last-Event-ID`
overrides them and uses `operational:agent` cursors for split feeds. Legacy
numeric IDs still resume the operational cursor.

Plugin-inactive auto-task skips warn once per workspace, definition, plugin, and
seeded plugin version. The warned keys persist in the workspace's
`state/auto-tasks.json`, so later scheduler passes, including each new clock-tick
process, log the skip at DEBUG. A job step skipped because its `when:` guard was
false logs `orbit.job.step_skipped` at INFO; other skips, such as a step a resumed
run already completed, stay at WARN.

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
- Consecutive clock ticks refused by an older executable generation are recorded
  as one line when that generation releases its pin. The line gives UTC start,
  end, last refusal, and refused tick count, and says the executable changed
  under live Orbit processes. The active hold is kept in
  `~/.orbit/.generation-clock-hold.json` so separate tick processes can share it.

On Linux, the sweep unit logs to the journal, which rotates independently.

## Verification

Confirm that the effective active path exists and receives a new expected event. For retention,
compare the active file and archives against the configured per-file, total-size, and age caps;
remember that global JSONL rotation walks archives from long-lived processes (`mcp serve`,
`mcp listen`, `sweep` / `clock tick`, `web serve`) and when the active file exceeds its budget on first write,
while sweep-log rotation runs opportunistically on each pass. Short-lived commands, including
`orbit --help`, do not open the JSONL file.

Related: [Inspect the audit trail](./audit-trail.md) for durable invocation and pipeline events.
