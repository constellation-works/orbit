# Diagnose a live provider process

## Contents

- [Tell a working agent from an abandoned wrapper](#tell-a-working-agent-from-an-abandoned-wrapper)
  - [Verify model routing before reading logs](#verify-model-routing-before-reading-logs)

## Tell a working agent from an abandoned wrapper

For a run that is still `running`, the operator-only `orbit_run` action `show`
(operator authority; denied for agent sessions) carries
`execution_progress`: the activity step that is open and the provider children
it spawned. Only `show` carries it — `orbit_run` action `list` (also
operator-only) pages many runs and does not pay for an audit scan and a
liveness probe per row.

A session with a shell on the owning host reads the
same liveness with `orbit run show <run_id> --json` (`provider_processes`).

Its `state` is `observed` when the run has a v2 audit trail and `unavailable`
when it has none, so an empty projection never has to be read as "nothing is
running". `active_step` names the open `step_id` and `step_index`, and each
`provider_processes.items` entry names the child's `pid`, `provider`, owning
step, and a `liveness` of `alive`, `exited`, or `unknown`.

`liveness` is judged against the identity token recorded with the PID, so a
recycled PID reads `exited` rather than a false `alive`, and a PID recorded in
another PID namespace reads `unknown` rather than a false `exited`. A child with
`finished: true` carries its `exit_code` instead. `limit` and `truncated` say
when a long retry history was bounded.

Within the children that survive the
bounded structural window, open children claim the budget before finished
ones, but a still-open child spawned before that window never reaches the
bound at all and can be absent from `provider_processes.items`;
`provider_processes.truncated` is then `true`.

`execution_progress` itself is served from those bounded windows on every
`orbit_run` show surface. `steps_complete` is `false` when the step-lifecycle
prefix (512 rows) does not include every lifecycle row: `active_step` may then
be null even though a step is open, so a null `active_step` is not by itself
proof that no step is open. `complete` is `false` when any window overflowed:
older provider children, their samples, step positions, or the active step
may be missing.

Read a null `active_step` or a missing child as "no open
step/child" only when the corresponding completeness signal is `true`.

A `running` run whose open step has no `alive` child is the signature of an
abandoned wrapper only when `execution_progress.complete` is `true` (and
`provider_processes.truncated` is `false`); when it is incomplete or
truncated, the missing child or step may predate the window, so that reading
is unavailable from `execution_progress` alone.

`orbit run show <run_id>
--json` reports the same `provider_processes` for the owning host from a
complete replay of the trail, which does distinguish those cases.

With `view: "bounded"`, progress and the log page come from a fixed audit
budget rather than a replay of the whole trail: a prefix of step lifecycle
rows, the newest provider spawn and completion rows, the newest samples since
them, the parents that place them in a step, and the requested log page.
`audit_metadata` gives `rows_read` and its `row_budget`. `logs.total` is still
a full store count, so pagination stays exact.

When `audit_metadata.complete`
is `false`, the history outgrew a budget: children spawned before the window,
their samples, step positions, or the active step may be missing, and
`provider_processes.truncated` is `true`.

`execution_progress.complete` and
`execution_progress.steps_complete` carry the same completeness on both the
bounded and the full `orbit_run` show surfaces; omitting `view` does not turn
`execution_progress` into a full replay — only the CLI's `provider_processes`
above replays the whole trail.

### Verify model routing before reading logs

`orbit run show <run_id> --json` separates three identities: `requested_crew`
is the submitted `crew` input, `resolved_run_crew` is the run-level routing
decision, and `activity_provenance` is durable provider/model evidence for
each agent activity. The latter is the source for actual model routing; it can
be mixed within one run. `actual_status: "not_started"` means no activity has
begun, while `"unavailable"` means it began but no invocation evidence is
available.

Do not infer provider usage or token cost from the requested or
resolved crew, and do not charge deterministic workflow wrapper steps to a
model.

Provider token counters take precedence over usage declared in an answer;
declared usage is only a fallback when provider counters are absent. Codex
prompt-tier evidence belongs to the latest started rollout turn, so a resumed
session does not re-charge earlier turns. Missing rollout or turn-boundary
evidence leaves per-request usage unknown; large invocation totals alone cannot
prove which requests incurred a premium.

An explicit activity `crew` always wins. Otherwise activities with a
`crew_config_key` draw from that pool once per run: the task pilot from
`workflow.purpose_crews.task_pilot`, step-failure and PR conflict recovery from
`workflow.purpose_crews.failure_recovery`, auto-fix repairs from
`workflow.purpose_crews.auto_fix`, final recovery from
`workflow.purpose_crews.final_recovery`. An unset purpose pool falls back to
the pool it replaced (`low` for the pilot and auto-fix, `medium` for recovery).

The run
state's `activity_crew_draws` records each draw, its source and the eligible
members; `orbit run show` prints them as `Activity Crew:` lines, and `--json`
names each draw's `rule`. An empty complexity pool falls back to
`workflow.default_crew` and the source says so. Other activities use the run's
resolved crew.
Check `activity_provenance` after execution to verify the effective route,
especially after changing crew configuration.

Step-scoped variants when the failing step is known: `orbit run show|logs|events <run_id> -s <step_id> --json`.

If these commands fail or omit needed detail, inspect raw payload blobs under `.orbit/state/audit/blobs/` or definition snapshots under `.orbit/state/job-runs/` and mention the fallback in your report.
