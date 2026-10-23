---
title: Policy format
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

`spec.description` is an optional free-text label. A rule is a glob relative to
the workspace root and cannot leave it: `~` and `..` are refused.

## Global denies

`denyRead` blocks reads and `denyModify` blocks writes. Deny rules accumulate
globally and apply after the selected filesystem profile is resolved.

```yaml
denyRead:
  - "**/*.env"
denyModify:
  - .orbit/**
  - "!.orbit/tmp/**"
  - "**/*.env"
```

A `denyModify` entry that starts with `!` is an exception. It names an exact
path or a `<path>/**` subtree strictly inside an earlier deny, and reopens it
only for profiles that already allow writing there. In the example,
`.orbit/tmp/**` is writable for a profile that grants it, and the rest of
`.orbit/` stays blocked. `denyRead` takes no exceptions.

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
      - "!docs/drafts/**"
```

Rules run in order and the last rule that matches a path decides it, so a rule
starting with `!` carves a path out of an earlier grant: `implementer` cannot
modify `docs/drafts/`. Every `modify` rule must be covered by a `read` rule,
and a profile rule may not repeat a global deny entry.

Orbit's default policy defines `implementer`, `reviewer`, `docs_writer` and
`pure_compute`. The profile named `unrestricted`, which an activity gets when
it sets no `fsProfile`, always exists: it reads and modifies everything the
global denies leave open. A workspace policy of the same name merges over the
host's: deny rules accumulate, and a workspace profile replaces a host profile
of the same name. See [Scoping rules](../scoping/).

An activity selects a profile with `fsProfile`:

```yaml
spec:
  type: agent_loop
  fsProfile: implementer
```

To test a path against a profile, run `orbit doctor fs-access <profile> <path>`.
For which OS sandbox enforces a profile on each platform, and what happens
where none is available, see [Platform Support](../../concepts/agents/#platform-support).
