---
title: Activity and Job YAML
description: "Reference shapes for schemaVersion 2 activity and job assets."
sidebar:
  order: 3
---

## Activity envelope

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
rules as `tools`. A task's `required_tools` cannot re-grant a disallowed tool;
the run is refused before launch.

An `agent_loop` activity that declares neither list still loads, with a
deprecation warning. Declare `tools` or `tool_disallow_list`.

Being on the allowlist never replaces runtime role, capability, policy,
filesystem, subprocess, or authentication checks.

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

Inline a full activity spec:

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

## Modifiers

Any step may add `when` and `retry`.

```yaml
retry:
  max_attempts: 3
  initial_backoff_ms: 500
  backoff_cap_ms: 5000
  backoff_strategy: exponential
```
