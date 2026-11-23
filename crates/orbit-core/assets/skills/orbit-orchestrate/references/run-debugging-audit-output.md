# Interpret run-audit JSON

## Contents

- [Run-audit command output](#run-audit-command-output)
  - [Step indexes, selectors and units](#step-indexes-selectors-and-units)
  - [`orbit run show --json`](#orbit-run-show---json)
  - [`orbit run logs --json`](#orbit-run-logs---json)
  - [`orbit run events --json`](#orbit-run-events---json)
  - [`orbit run trace --json`](#orbit-run-trace---json)

## Run-audit command output

`orbit run show`, `logs`, `events` and `trace` print the same run through four
projections. Each `--json` output is one object, except `logs --follow`, which
is JSONL.

### Step indexes, selectors and units

- In the `orbit run show` summary table, `#` is one-based
  (`step_index + 1`); `step_index` in JSON is zero-based (`steps[]`,
  `provider_processes[]` and the `-s` step record).
- In the `orbit run events` table, `STEP` is the event's textual `step_id`
  (the activity ID, or `-` when absent), not a one-based step number.
- `duration_ms` and `lock_wait_ms` are milliseconds. The table prints
  formatted durations.
- `-s`/`--step` on `show`, `logs` and `events` accepts an activity `step.id`
  from the v2 job YAML. A bare number is read as a zero-based step index. A
  legacy target ID stored on the run also resolves.

### `orbit run show --json`

Without `-s`, the object holds:

- `run`: the job run record. `run.state` is the table's `State:` line.
  `run.agent_invocation` carries the agent invocation's `outcome`, `answer`
  and `progress`. An auto pipeline also carries `run.auto_concurrency`.
- Additional fields:

  | Additional run-show fields |
  |---|
  | `recovery_attempts` (`state` is `unavailable`, `not_attempted` or `recorded`; see [Inspect recovery evidence](run-debugging-recovery-evidence.md#inspect-recovery-evidence-before-falling-back-to-audit-files)), `tag_instructions`, `pull_claim` and `crew_window`, `claimed_leaves`, `refused_settlements`, `pull_occupancy` (`occupied` and `reviewing`), `pull_breaker` (null while closed), `catalog_layers` (each job or activity reference and the layer that resolved it), `drain_summary`, `waiting_on_lock_holders`. |

  Drain, claim and window fields are null or empty for a run that is not a
  pull drain or claimed leaf.
- `pipeline_state`: the run's checkpoint. Null when the checkpoint is
  unreadable; the table then says so.
- `steps`: one entry per step, with `step_index`, `target_id`, `target_type`,
  `state`, `started_at`, `finished_at`, `duration_ms`, `lock_wait_ms`,
  `exit_code`, `error_code`, `error_message`, `outcome` and
  `agent_response_json`. Each entry also has `agent_executions`, one per
  launch, with `provider`, `model`, `crew` and `reasoning_effort`.
  `steps_source` is `record` when the run record holds the steps and `audit`
  when they were rebuilt from the audit trail.
- `root_cause` (first failed leaf, or null) and `additional_root_causes`.
- `provider_processes`: one entry per provider child an agent step spawned:
  `pid`, `provider`, `step_id`, `step_index`, `liveness` (`alive`, `exited`
  or `unknown`), `finished`, `exit_code`, `duration_ms` and
  `stopped_descendants`.
- `script_executions`: every script-step attempt, refusals included.
- `worker_resource_limit`: present only when the resource watchdog recorded
  evidence for the run.

The full MCP `orbit.workflow.run.show` / `orbit.workflow.run.observe` and
HTTP run detail carry the same six pull-drain fields: `pull_claim`,
`claimed_leaves`, `refused_settlements`, `crew_window`, `pull_occupancy` and
`pull_breaker`. Unreadable sections are logged and remain null or empty;
unknown occupancy is null. Recorded-only HTTP detail keeps the stored
breaker but leaves the live sections empty. The bounded MCP view keeps its
existing smaller profile.

With `-s STEP --json` the object is `{"run_id", "job_id", "step",
"step_output"}`. `step` holds the step's record fields and `step_output` its
checkpoint output, or null.

### `orbit run logs --json`

The object is `{"run_id", "job_id", "records"}`. Each record is one provider
CLI invocation, with `step_id`, `provider`, `stdout_blob_ref`,
`stderr_blob_ref`, `stdout` and `stderr`. The streams are the full captured
text, not truncated. Stderr also goes to the terminal's stderr, so stdout
stays parseable.

When no invocation was audited (the run never reached step execution),
`records` is `[]`. The object then carries `worker_log_path`, the pipeline
worker log, or null when none exists.

With `--follow` in a JSON mode, output is JSONL, one record per line:
`{"run_id", "provider", "stream", "text"}`, where `stream` is `stdout` or
`stderr`. The worker-log fallback is `provider: "worker"`, `stream: "stderr"`.
Live lines come from the retained redacted tracing feed, and each completed
invocation's capture fills in what the feed missed. With `--step`, captures
are emitted as each invocation finishes. Follow mode stops at any terminal run
outcome.

Ctrl-C stops observing without cancelling the run. Outside JSON
mode, stdout goes to stdout and stderr to stderr.

### `orbit run events --json`

The object is `{"run_id", "job_id", "events"}`. Each element is a stored v2
audit event as recorded. `step_id` is added when the stored event lacks one
and its owning step is known. `--type` matches the exact `event_type`, such
as `step.started` or `cli.invocation.finished`.

### `orbit run trace --json`

The object is `{"run_id", "job_id", "roots", "orphans"}`. A node is
`{"event", "children"}`, where `event` is an audit event as `run events`
prints it. `roots` are events with no parent. Children follow
`parent_event_id`. An event whose named parent is absent from the run is an
orphan and roots its own tree in `orphans`. An event that would reappear on
its own path is a leaf with no children.

Text output prints the same trees indented, with an `Orphans:` section. A run
with no events prints `No audit events recorded.`, and the JSON keeps empty
`roots` and `orphans`.
