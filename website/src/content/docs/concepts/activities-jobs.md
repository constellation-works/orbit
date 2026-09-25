---
title: Activities and Jobs
description: "How Orbit represents executable work units and workflow orchestration."
sidebar:
  order: 3
---

## Activity

An activity is a reusable execution unit. Schema v2 activities declare `schemaVersion: 2`, `kind: Activity`, metadata, and a typed `spec`.

Supported activity types:

| Type | Use |
|------|-----|
| `agent_loop` | Run an agent with an instruction and tool allowlist; the run's crew selects provider and model. The retired `backend:` selector is covered in [Retired backend selection](../../reference/config/#retired-backend-selection). |
| `deterministic` | Run a registered deterministic action. |

For a task-backed `agent_loop`, the activity's `tools` are a baseline. Orbit
adds the task's `required_tools` — fixed at creation, see
[Transition rules](../tasks/#transition-rules) — and deduplicates the union
before provider launch. A task with no requirements receives the baseline
unchanged. Unknown, inactive, malformed, wildcard, and non-agent-facing
requirements fail admission before launch. If one agent activity selects multiple tasks, their requirements
are all included in the same union. The effective list is included in the CLI envelope,
`ORBIT_ACTIVITY_TOOLS`, and audit evidence.

## Job

A job is a workflow. It has schedule state, optional default input, concurrency limits, and ordered steps. A job runs when something invokes it — `orbit run`, a task ship, or a [routine](../scheduling/#routine) firing on the sweep clock.

Step bodies can reference an activity, inline an activity spec, or compose control flow:

- `target: activity:<name>`
- `spec: ...`
- `parallel`
- `fan_out` and `fan_in`
- `loop`

## Why Both Exist

Activities make execution behavior reusable. Jobs make orchestration explicit. This keeps the dispatch surface inspectable and avoids hiding agent behavior inside code.

**Example:** A job step referencing a reusable activity.

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
