---
title: Choose Scopes
description: "Select the right state scope and filesystem profile for Orbit assets and execution."
sidebar:
  order: 4
---

## Where state lives

Keep anything that describes this repository's work in the workspace. Use
global state for shared defaults and the audit trail.

- **Workspace only:** tasks and job runs.
- **Global only:** the audit trail.
- **Merged by name:** activities, jobs, policies, and skills combine global
  defaults with workspace entries. A workspace skill or policy profile
  overrides the global one with the same name. Activities and jobs work the
  other way: a shipped default keeps its name, so a workspace file can add a
  new name but a workspace file with a shipped name is ignored.

[Scoping Rules](../../reference/scoping/) has the full table and the
`.orbit/` layout.

## What an activity may touch

Set `fsProfile` on an activity to choose what it may read and modify:

```yaml
spec:
  type: agent_loop
  fsProfile: reviewer
```

Then define that profile in a policy:

```yaml
spec:
  fsProfiles:
    reviewer:
      read: [./**]
      modify: []
```

Global `denyRead` and `denyModify` rules still apply on top of any profile.
[Policy Format](../../reference/policy-format/) covers both, and
[Platform Support](../../concepts/agents/#platform-support) covers which
sandbox enforces the profile on each platform.
