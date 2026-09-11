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
| `agent_loop` | Run an agent with an instruction, provider, and tool allowlist. Agent execution uses the CLI path only; `spec.backend: http` and `auto` fail catalog load. Remove a leftover backend with `orbit doctor --fix-retired-activity-backends`. |
| `deterministic` | Run a registered deterministic action. |

For a task-backed `agent_loop`, the activity's `tools` are a baseline. Orbit
adds the task's exact `required_tools` and deduplicates the union before provider
launch. The list is immutable after task creation. A task with no requirements
receives the baseline unchanged. Unknown,
inactive, malformed, wildcard, and non-agent-facing requirements fail admission
before launch. If one agent activity selects multiple tasks, their requirements
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
# .orbit/activities/analyze_code.yaml
schemaVersion: 2
kind: Activity
name: analyze_code
spec:
  type: agent_loop
  provider: gemini
  model: gemini-3.1-pro
  instruction: "Analyze the provided code."

---
# .orbit/jobs/review_pr.yaml
schemaVersion: 2
kind: Job
name: review_pr
steps:
  - id: analysis
    target: activity:analyze_code
```
