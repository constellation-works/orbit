---
title: Policies
description: "How Orbit uses filesystem profiles and global deny rules to scope execution."
sidebar:
  order: 5
---

## Definition

Policy is a filesystem-scoping surface. It controls what an activity can read or modify, then applies global deny rules on top.

Activity tool inclusion is a separate admission boundary: a task's
`required_tools` (fixed at creation, see
[Transition rules](../tasks/#transition-rules)) extends the activity's baseline
allowlist but does not bypass caller-role or host-capability checks,
tool-specific policy, filesystem profiles, subprocess allowlists, or external
authentication, any of which may still deny an included tool at execution time.

An activity can select a named profile with `fsProfile`. If it omits the field, Orbit resolves an implicit unrestricted profile before global denies are applied.

> **Platform support.** Spawned agent CLIs run under an OS boundary scoped by the resolved `fsProfile` — `sandbox-exec` on macOS, Bubblewrap on Linux, none on Windows; see [Platform Support](../agents/#platform-support).

## Shape

```yaml
schemaVersion: 2
kind: Policy
metadata:
  name: default
spec:
  denyRead:
    - "**/*.env"
  denyModify:
    - .orbit/**
    - "**/*.env"
  fsProfiles:
    reviewer:
      read: [./**]
      modify: []
```

## Use

Use narrow profiles for review and summarization. Use broader profiles only when
an agent is expected to edit code.
