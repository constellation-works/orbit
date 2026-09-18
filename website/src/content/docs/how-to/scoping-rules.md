---
title: Choose Scopes
description: "Select the right state scope and filesystem profile for Orbit assets and execution."
sidebar:
  order: 4
---

## Artifact Scope

Tasks and job runs are workspace-only; activities, jobs, policies, and skills
merge global defaults with workspace overrides by key; audit is global-only.
The full table is in [Scoping Rules](../../reference/scoping/).

Use workspace-local state for work tied to a repository. Use global state for shared defaults and the audit trail; skills use global defaults with optional workspace overrides by skill name.

## Filesystem Scope

Use `fsProfile` to select what an activity may read and modify.

```yaml
spec:
  type: agent_loop
  fsProfile: reviewer
```

Then define the profile in policy:

```yaml
fsProfiles:
  reviewer:
    read: [./**]
    modify: []
```

Global `denyRead` and `denyModify` rules still apply.
