---
title: Policies
description: "How Orbit uses filesystem profiles and global deny rules to scope execution."
sidebar:
  order: 5
---

## Definition

Policy is a filesystem-scoping surface. It controls what an activity can read or modify, then applies global deny rules on top.

Activity tool inclusion is a separate admission boundary. For task-backed agent
loops, `task.required_tools` extends the activity's baseline tool allowlist; it
is immutable after task creation and does not bypass caller-role or host-capability checks, tool-specific policy,
filesystem profiles, subprocess allowlists, or external authentication. Any of
those checks may still deny an included tool at execution time.

An activity can select a named profile with `fsProfile`. If it omits the field, Orbit resolves an implicit unrestricted profile before global denies are applied.

> **Platform support.** Spawned agent CLIs use a platform-specific OS boundary where supported: macOS uses `sandbox-exec`, and Linux uses trusted `/usr/bin/bwrap` after a namespace-and-mount capability probe. The Linux boundary enforces writes from the resolved `fsProfile` while leaving host filesystem reads and host network access available; read rules and network-egress policy remain delegated. Linux dispatch fails closed when `/usr/bin/bwrap` is unavailable or the probe fails, unless the executor explicitly sets `allow_fallback: true`, which runs without Linux write confinement. On Windows and other unsupported platforms, the policy still applies as in-process FS guards for Orbit's HTTP-tool builtins, but no OS-level backend wraps the spawned agent subprocess.

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
