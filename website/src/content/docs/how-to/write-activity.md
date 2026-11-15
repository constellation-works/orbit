---
title: Write an activity
description: "Create an activity file for agent or deterministic execution and run it from a job."
---

An activity is one step a job runs: an agent loop or a registered
deterministic action. Write it as YAML, save it in your checkout, and
reference it from a job. [Activity and Job YAML](/reference/activity-job-yaml/)
lists every field.

## 1. Write the file

An agent activity gives the agent an instruction and its tools:

```yaml
schemaVersion: 2
kind: Activity
metadata:
  name: my_review
spec:
  type: agent_loop
  description: Review the current diff.
  instruction: Review the current diff and report risks.
  tools:
    - orbit.task.show
    - orbit.search
```

It names no provider. The crew the run draws picks the provider, model and
effort, and overrides an inline `provider` or `model`. The retired `backend:`
key is covered in
[Retired backend selection](/reference/config/#retired-backend-selection).

A `deterministic` activity names a registered `action` and passes it optional
`config`. Input and output are JSON Schema. `example_action` is a
placeholder: replace it with a registered action.

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

## 2. Choose an agent's tools

`tools` is the baseline for every task that uses the activity. Don't widen it
for one specialized task. List the extra tool names in that task's
`required_tools` when you create it; they can't be added later. Orbit merges
the two lists at dispatch.

A tool on the list still passes the runtime capability, policy, sandbox,
subprocess and authentication checks.

## 3. Save it and run it

Save the activity under `.orbit/resources/activities/` in your checkout. Give
it a name Orbit doesn't ship: a workspace file named like a shipped activity
is ignored. Reference it from a step in a
[job](/reference/activity-job-yaml/#job-envelope), and save the job under
`.orbit/resources/jobs/`, also with its own name:

```yaml
- id: review
  target: activity:my_review
```

Then run the job by name, or by the path to its YAML:

```bash
orbit job show <job_id>                    # the activity each step runs
orbit run job <job> --input key=value      # submits and prints a run ID
orbit run job <job> --wait                 # waits; nonzero unless the run succeeds
```

`orbit run show <run-id>` names the catalog layer that supplied each job and
activity (`workspace`, `shipped` or `plugin:<ns>`), so you can confirm your
file was used. Shipped references appear only with `--verbose`.
