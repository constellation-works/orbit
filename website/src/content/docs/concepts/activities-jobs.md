---
title: Activities and jobs
description: "Activities are reusable execution units, and jobs chain them into workflows."
---

An activity is one reusable unit of execution, and a job chains activities
into a workflow. Both are YAML files, so you can read what an agent is told
to do and change it. [Activity and Job YAML](/reference/activity-job-yaml/)
lists every field.

## Activity

An activity is a YAML file with `schemaVersion: 2`, `kind: Activity`,
`metadata` and a typed `spec`.

| Type | Use |
|------|-----|
| `agent_loop` | Run an agent with an instruction and a tool policy. |
| `deterministic` | Run a registered deterministic action. |

An `agent_loop` activity names no provider or model. The run resolves a
[crew](/concepts/agents/#crews) at dispatch and applies its provider, model
and effort.

The activity's [tool policy](/concepts/agents/#tool-policy) is a baseline for
every task it runs:

- With an allowlist (`tools`), a task's `required_tools` are added to the
  list. One activity running several tasks gets the union.
- A deny list (`tool_disallow_list`) can't be overridden. On the implementing
  activity and on your own activities, a required tool that the list covers
  refuses the run before launch.

See [Agent tool lists](/reference/activity-job-yaml/#agent-tool-lists) for
the full rules, and [Task tool grants](/concepts/tasks/#task-tool-grants) for
the operator exception.

## Job

A job is an ordered list of steps with an `enabled` or `disabled` state,
optional default input and a limit on concurrent runs. It runs when something
invokes it: `orbit run`, a task ship, or a
[routine](/concepts/scheduling/#routine) firing on the scheduler clock.

Each step has exactly one body:

- `target: activity:<name>` runs a catalog activity.
- `spec` inlines an activity.
- `parallel`, `fan_out` with `fan_in`, or `loop` controls other steps.

## Example

A job step that references a reusable activity:

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

To write your own, follow [Write an activity](/how-to/write-activity/).

## Workspace files

Your activities and jobs live under `.orbit/resources/activities/` and
`.orbit/resources/jobs/` in the workspace. Give each a name Orbit doesn't
ship: a workspace file named like a shipped default is ignored.

Older releases seeded copies of the shipped defaults into these directories.
`orbit workspace sync` refreshes the copies you haven't edited and leaves
edited copies and your own files alone.

### If a workspace activity is stale

A workspace activity that Orbit can no longer load, such as a copy of an
activity a release retired or one that declares a removed key, doesn't take
down the workspace:

- Orbit excludes that one file, logs a warning with its path and the remedy,
  and keeps every other activity usable.
- A job that references it fails with the same error.
- `orbit doctor` reports the file, so you can fix or remove it before a
  scheduled job needs it.
