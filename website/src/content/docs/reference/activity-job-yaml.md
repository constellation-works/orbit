---
title: Activity and job YAML
description: "Reference shapes for schemaVersion 2 activity and job assets."
sidebar:
  order: 3
---

## Activity envelope

The examples illustrate schema shapes. `example_action` is a placeholder for
a registered action; `assess_a` and `assess_b` stand for catalog activities
you define. Replace those names before executing a job.

```yaml
schemaVersion: 2
kind: Activity
metadata:
  name: example_activity
spec:
  type: deterministic
  description: Run a registered deterministic action.
  action: example_action
  input_schema_json:
    type: object
    properties: {}
  output_schema_json:
    type: object
    properties:
      status:
        type: string
```

## Activity types

| Type | Required fields | Notes |
|------|-----------------|-------|
| `agent_loop` | `instruction`; optional `tools` or `tool_disallow_list`, `provider`, `model`, `wall_clock_timeout_seconds` | Runs the provider's CLI agent. Tool lists and retired fields are covered below. |
| `deterministic` | `action`; optional `config` | Runs a registered deterministic action. |

### Agent tool lists

`tools` is the activity's baseline allowlist. Declaring `tool_disallow_list`,
even as `[]`, selects deny mode instead: every registered agent-facing tool
except the listed ones. `tool_disallow_list` cannot be combined with a
non-empty `tools`.

For task-backed dispatch, Orbit adds the task's exact `required_tools` to the
baseline, removes duplicates, and rejects invalid requirements before the
provider launches. The resulting effective list is serialized as `tools` in
the CLI execution envelope and exported as `ORBIT_ACTIVITY_TOOLS`. The task's
requested list is serialized separately as `required_tools`. It cannot change
after the task is created, and audit evidence records both lists.

In deny mode the effective list is every callable tool, and the run also
exports `ORBIT_ACTIVITY_TOOL_POLICY=deny`, `ORBIT_ACTIVITY_TOOLS_DENY`, and
`ORBIT_ACTIVITY_NAME`. Disallow entries follow the same name and wildcard
rules as `tools`. A task's `required_tools` cannot re-grant a disallowed tool.
On `agent_implement`, and on any activity outside the shipped non-implementer
set, a covered requirement refuses the run before launch. `final_recovery`,
`step_failure_recovery`, `pr_conflict_recovery`, `agent_review_repair`,
`review_reconciliation_review`, `task_pilot` and `agent_invoke` drop that
requirement from the requested list and record a note; it stays out of the
callable set. The shipped `agent_explore` and `verify_finding` activities use
the same non-implementer rule.

An `agent_loop` activity that declares neither list still loads, with a
deprecation warning. Declare `tools` or `tool_disallow_list`.

Being on the allowlist never replaces runtime role, capability, policy,
filesystem, subprocess, or authentication checks.

Optional `steering_instruction` and `explore_instruction` fields add guidance
only when mid-run steering or read-only exploration is enabled and available.
See [Agent execution](../../concepts/agents/#runtime-path)
for the corresponding workspace settings.

### Retired agent fields

Agent execution uses the CLI path only. `backend: cli` still parses and is
ignored; `backend: http` and `backend: auto` fail catalog load, and
`orbit doctor --fix-retired-activity-backends` removes them. See
[Retired backend selection](../config/#retired-backend-selection).
`max_iterations` is accepted and has no effect: only
`wall_clock_timeout_seconds` bounds an invocation.

## Job envelope

```yaml
schemaVersion: 2
kind: Job
metadata:
  name: example_job
spec:
  state: enabled
  max_active_runs: 1
  kind: workflow
  steps:
    - id: run_action
      target: activity:deterministic_reference
```

## Step bodies

Every step has an `id` and one body.

Reference an activity:

```yaml
- id: assess
  target: activity:agent_assess_diff
```

Inline a type-specific activity spec:

```yaml
- id: run_action
  spec:
    type: deterministic
    action: example_action
    config: {}
```

Run branches in parallel:

```yaml
- id: parallel_assessment
  parallel:
    join: { mode: all }
    branches:
      - id: branch_a
        target: activity:assess_a
      - id: branch_b
        target: activity:assess_b
```

`parallel.join` and `fan_in.join` use the same success requirements:

- `all` succeeds when every branch or worker succeeds, including an empty block.
- `any` requires at least one successful branch or worker, so an empty block fails.
- `quorum` requires at least `n` successes. `n` must be positive and cannot exceed
  a parallel block's declared branch count. Fan-out item counts are resolved at
  runtime, so a positive quorum fails when too few workers succeed, including
  when there are no items.

An empty fan-out records `[]` under its step id and optional `fan_in.collect`
alias, and emits dispatch and join audit events with zero workers. A failed
join stops subsequent steps.

Fan-in keeps one output slot per item in dispatch order. Failed workers have
`null` output slots; successful siblings remain available when `any` or
`quorum` admits the join. Set `fan_in.errors: worker_errors` to publish a
separate array at `steps.worker_errors.output` in the same order: successful
workers have `null` errors and failed workers have their dispatch error or
unsuccessful outcome's message. A worker panic records its panic diagnostic.
An empty block publishes `[]`. The errors array is retained in successful
compound checkpoints and restored on resume. Choose an errors key distinct
from the step's id and collect alias.

The shipped finding verification job passes these errors into apply as
`partition_errors`. A failed partition stays unverified with `verifier_failed`
and its crew and diagnostic; a successful verifier that omits a task's verdict
still reports `missing_verdict`. Native pinned-source inspection supports
Codex and Claude, so verifiers are drawn only from crews of those providers;
a tier without one passes to the next tier and then the default crew. A
verifier that still fails, for any reason, keeps its refusal alongside
successful partitions' verdicts.

## Modifiers

Any step may add `when` and `retry`.

```yaml
retry:
  max_attempts: 3
  initial_backoff_ms: 500
  backoff_cap_ms: 5000
  backoff_strategy: exponential
```
