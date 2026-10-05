---
title: Policies
description: "How Orbit scopes what a run can read and modify: filesystem profiles plus global deny rules."
sidebar:
  order: 5
---

## Definition

A policy scopes the filesystem. A filesystem profile sets what an activity can
read and modify, and global deny rules apply on top.

An activity selects a named profile with `fsProfile`. If it names none, Orbit
resolves an implicit unrestricted profile, and the global denies still apply.

> **Platform support.** Spawned agent CLIs run inside an OS sandbox scoped by
> the resolved `fsProfile`: `sandbox-exec` on macOS, Bubblewrap on Linux. See
> [Platform support](../agents/#platform-support) for what each one enforces.

Tool access is a separate check. A task's `required_tools` (fixed at creation;
see [Transition rules](../tasks/#transition-rules)) extend the activity's
tools, but never bypass caller-role or host-capability checks, tool-specific
policy, filesystem profiles, subprocess allowlists, or external
authentication. Any of these can still deny an included tool at execution
time.

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

The full format is in [Policy Format](../../reference/policy-format/).

## Use

Use narrow profiles for review and summarization. Use broader profiles only
when an agent is expected to edit code.
