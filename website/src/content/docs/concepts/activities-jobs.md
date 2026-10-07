---
title: Activities and Jobs
description: "How Orbit defines reusable execution units (activities) and the workflows that chain them (jobs)."
sidebar:
  order: 3
---

## Activity

An activity is a reusable execution unit. It is a YAML file with
`schemaVersion: 2`, `kind: Activity`, `metadata`, and a typed `spec`.

| Type | Use |
|------|-----|
| `agent_loop` | Run an agent with an instruction and a tool policy. The run's crew picks the provider and model. The retired `backend:` selector is covered in [Retired backend selection](../../reference/config/#retired-backend-selection). |
| `deterministic` | Run a registered deterministic action. |

For a task-backed `agent_loop`, the activity's
[tool policy](../agents/#tool-policy) is a baseline, and the task's
`required_tools` (fixed at creation; see
[Transition rules](../tasks/#transition-rules)) extend it:

- An allowlist (`tools`) gains the task's requirements, deduplicated. A task
  with no requirements gets the list unchanged.
- A disallow list (`tool_disallow_list`) can't be overridden. A requirement it
  covers is refused before launch.

Unknown, inactive, malformed, wildcard, and non-agent-facing requirements are
also refused before launch. When one agent activity runs several tasks, it
gets the union of their requirements. The effective list is recorded in the
CLI envelope, `ORBIT_ACTIVITY_TOOLS`, and the audit trail.

## Job

A job is a workflow: ordered steps, plus an `enabled` or `disabled` state,
optional default input, and a concurrency limit. A job runs when something
invokes it: `orbit run`, a task ship, or a [routine](../scheduling/#routine)
firing on the host scheduler clock.

Each step has exactly one body:

- `target: activity:<name>` runs a catalog activity
- `spec: ...` inlines an activity spec
- `parallel`
- `fan_out` with `fan_in`
- `loop`

## Why both exist

Activities make execution reusable. Jobs make orchestration explicit. Agent
behavior stays in YAML you can inspect, not hidden in code.

**Example:** a job step that references a reusable activity.

```yaml
# .orbit/resources/activities/analyze_code.yaml
schemaVersion: 2
kind: Activity
metadata:
  name: analyze_code
spec:
  type: agent_loop
  description: Analyze the provided code.
  instruction: "Analyze the provided code."
  tools:
    - orbit.task.show

---
# .orbit/resources/jobs/review_pr.yaml
schemaVersion: 2
kind: Job
metadata:
  name: review_pr
spec:
  state: enabled
  kind: workflow
  steps:
    - id: analysis
      target: activity:analyze_code
```

The activity names no provider or model. The run resolves a
[crew](../agents/#crews) at dispatch and applies its provider, model, and
effort.
