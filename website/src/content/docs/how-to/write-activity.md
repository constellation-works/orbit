---
title: Write an Activity
description: "Create a schemaVersion 2 activity file for agent or deterministic execution."
sidebar:
  order: 3
---

## Start with the Header

Every activity uses this envelope:

```yaml
schemaVersion: 2
kind: Activity
metadata:
  name: deterministic_reference
spec:
  type: deterministic
  description: Run a registered deterministic action.
```

## Add Schemas

Use JSON Schema-shaped input and output declarations.

```yaml
input_schema_json:
  type: object
  properties: {}
output_schema_json:
  type: object
  properties:
    status:
      type: string
```

## Choose a Type

For a deterministic activity, name a registered action and pass optional config:

```yaml
type: deterministic
action: example_action
config: {}
```

For an agent loop, declare instruction, tools, and provider:

```yaml
type: agent_loop
instruction: Review the current diff and report risks.
tools:
  - orbit.task.show
  - orbit.search
provider: claude
```

Orbit dispatches every agent loop through the provider's CLI agent; the retired
`backend:` key is covered in [Retired backend
selection](../../reference/config/#retired-backend-selection).

Treat `tools` as the baseline every task using the activity needs. A task may
add exact canonical names through `required_tools`; Orbit deduplicates that
union at dispatch. Declare requirements when creating the task because existing
tasks cannot acquire or replace them. Do not broaden an activity just for one specialized task.
Task requirements affect allowlist inclusion only and do not bypass runtime
capability, policy, sandbox, subprocess, or authentication checks.

## Use It

```bash
orbit activity list
orbit run job path/to/job.yaml --input key=value   # submits and returns a run ID
orbit run job path/to/job.yaml --wait              # block until the run is terminal
```
