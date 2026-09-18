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

## Global Denies

`denyRead` blocks reads. `denyModify` blocks writes. These rules accumulate globally and apply after the selected filesystem profile is resolved.

```yaml
denyRead:
  - "**/*.env"
denyModify:
  - .orbit/**
  - "**/*.env"
```

## Filesystem Profiles

Profiles describe allowed read and modify globs.

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

An activity selects a profile with `fsProfile`.

```yaml
spec:
  type: agent_loop
  fsProfile: implementer
```

Which OS-level sandbox enforces the profile on each platform, and what happens where none is available, is covered in [Platform Support](../../concepts/agents/#platform-support).
