---
title: Activity and Job YAML
description: "Reference shapes for schemaVersion 2 activity and job assets."
sidebar:
  order: 3
---

## Activity Envelope

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

## Activity Types

| Type | Required fields | Notes |
|------|-----------------|-------|
| `agent_loop` | `instruction`; optional `tools` or `tool_disallow_list`, `provider`, `model`, `wall_clock_timeout_seconds` | `tools` is the activity baseline (allowlist mode). `tool_disallow_list` selects deny mode instead: every registered agent-facing tool except the listed ones; it cannot be combined with a non-empty `tools`. For task-backed dispatch Orbit adds exact `task.required_tools`, deduplicates the union, and rejects invalid requirements before provider launch. Agent execution uses the CLI path only. `backend: cli` still parses and is ignored; `http` and `auto` fail catalog load. `orbit doctor --fix-retired-activity-backends` removes those retired keys. `max_iterations` is inert. |
| `deterministic` | `action`; optional `config` | Runs a registered deterministic action. |

The computed effective list is serialized as `tools` in the CLI execution
envelope and exported as `ORBIT_ACTIVITY_TOOLS`. The task-requested list is
serialized separately as `required_tools`; it is immutable after task creation,
and audit evidence records both lists.

In deny mode the effective list is every callable tool, and the run also
exports `ORBIT_ACTIVITY_TOOL_POLICY=deny`, `ORBIT_ACTIVITY_TOOLS_DENY`, and
`ORBIT_ACTIVITY_NAME`. Disallow entries follow the same name and wildcard
rules as `tools`. A task's `required_tools` cannot re-grant a disallowed tool:
the run is refused before launch. An `agent_loop` activity that declares
neither list still loads, with a deprecation warning; declare explicit `tools`
or a `tool_disallow_list`.
Allowlist inclusion never substitutes for runtime role, capability, policy,
filesystem, subprocess, or authentication checks.

## Job Envelope

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

## Step Bodies

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

Each step may include `when` and `retry`.

```yaml
retry:
  max_attempts: 3
  initial_backoff_ms: 500
  backoff_cap_ms: 5000
  backoff_strategy: exponential
```
