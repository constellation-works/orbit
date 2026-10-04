---
title: Policy Format
description: "Reference for Orbit policy YAML and filesystem profiles."
sidebar:
  order: 4
---

## Envelope

```yaml
schemaVersion: 2
kind: Policy
metadata:
  name: default
spec:
  denyRead: []
  denyModify: []
  fsProfiles: {}
```

## Global denies

`denyRead` blocks reads and `denyModify` blocks writes. Deny rules accumulate
globally and apply after the selected filesystem profile is resolved.

```yaml
denyRead:
  - "**/*.env"
denyModify:
  - .orbit/**
  - "**/*.env"
```

## Filesystem profiles

A profile lists the globs an activity may read and modify.

```yaml
fsProfiles:
  reviewer:
    read: [./**]
    modify: []
  implementer:
    read: [./**]
    modify:
      - crates/**
      - docs/**
```

An activity selects a profile with `fsProfile`:

```yaml
spec:
  type: agent_loop
  fsProfile: implementer
```

To test a path against a profile, run `orbit doctor fs-access <profile> <path>`.
For which OS sandbox enforces a profile on each platform, and what happens
where none is available, see [Platform Support](../../concepts/agents/#platform-support).
