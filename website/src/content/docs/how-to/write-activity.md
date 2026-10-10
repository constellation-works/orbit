---
title: Write an Activity
description: "Create a schemaVersion 2 activity file for agent or deterministic execution."
sidebar:
  order: 3
---

An activity is one step a job runs: an agent loop or a registered
deterministic action. You write it as YAML, save it in your checkout, and
reference it from a job. [Activity and Job YAML](../../reference/activity-job-yaml/)
lists every field.

## 1. Write the file

Every activity uses this envelope. Input and output are JSON Schema.

```yaml
schemaVersion: 2
kind: Activity
metadata:
  name: my_check
spec:
  type: deterministic
  description: Run a registered deterministic action.
  action: example_action
  config: {}
  input_schema_json:
    type: object
    properties: {}
  output_schema_json:
    type: object
    properties:
      status:
        type: string
```

A `deterministic` activity names a registered `action` and passes it optional
`config`. An `agent_loop` activity gives the agent an instruction, its tools,
and a provider instead:

```yaml
spec:
  type: agent_loop
  description: Review the current diff.
  instruction: Review the current diff and report risks.
  tools:
    - orbit.task.show
    - orbit.search
  provider: claude
```

Orbit runs every agent loop through the provider's CLI agent. The retired
`backend:` key is covered in
[Retired backend selection](../../reference/config/#retired-backend-selection).

## 2. Choose an agent's tools

`tools` is the baseline for every task that uses the activity. Don't widen it
for one specialized task. List the extra exact canonical tool names in that
task's `required_tools` when you create it; they can't be added later. Orbit
merges the two lists at dispatch.

Adding a tool only puts it on the allowlist. It does not bypass runtime
capability, policy, sandbox, subprocess, or authentication checks.

## 3. Save it and run it

Save the activity under `.orbit/resources/activities/` in your checkout, with
a name Orbit doesn't ship: a shipped activity keeps its name, and a workspace
file with the same name is ignored. Reference it from a step in a
[job](../../reference/activity-job-yaml/#job-envelope), and save the job under
`.orbit/resources/jobs/`, also with a name of its own:

```yaml
- id: check
  target: activity:my_check
```

Then run the job by name, or by the path to its YAML:

```bash
orbit job show <job_id>                    # the activity each step runs
orbit run job <job> --input key=value      # submits and prints a run ID
# Wait for completion and return nonzero unless the run succeeds.
orbit run job <job> --wait
```

`orbit run show <run-id>` names the catalog layer that supplied each activity
(`workspace`, `shipped`, or `plugin:<ns>`), so you can confirm your file was
used.
