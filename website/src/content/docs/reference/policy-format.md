---
title: Policies
description: "Bound what a run can read and modify with filesystem profiles and global deny rules."
---

A policy bounds the filesystem for a run. A profile lists the paths an
activity may read and modify, and global deny rules apply on top of every
profile. Spawned agent CLIs run inside an OS sandbox scoped by the resolved
profile; see [Platform support](/concepts/agents/#platform-support) for what
each platform enforces.

Give review and summarization activities a read-only profile. Grant write
access only where an agent is expected to edit.

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

`spec.description` is an optional label. A rule is a glob relative to the
workspace root and cannot leave it: `~` and `..` are refused.

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

Rules run in order and the last rule that matches a path decides it. A rule
that starts with `!` carves a path out of an earlier grant, so `implementer`
cannot modify `docs/drafts/`. Every `modify` rule must be covered by a `read`
rule, and a profile rule may not repeat a global deny entry.

An activity selects a profile with `fsProfile`:

```yaml
spec:
  type: agent_loop
  fsProfile: implementer
```

Orbit's default policy defines `implementer`, `reviewer`, `docs_writer` and
`pure_compute`. An activity that names no profile gets `unrestricted`: read and
modify access to everything the global denies leave open.

To test a path against a profile, run `orbit doctor fs-access <profile> <path>`.

## Global denies

`denyRead` blocks reads and `denyModify` blocks writes, whatever profile is
selected.

```yaml
denyRead:
  - "**/*.env"
denyModify:
  - .orbit/**
  - "!.orbit/tmp/**"
  - "**/*.env"
```

A `denyModify` entry that starts with `!` is an exception. It names an exact
path or a `<path>/**` subtree inside an earlier deny, and reopens it only for
profiles that already allow writing there. Here `.orbit/tmp/**` is writable for
a profile that grants it, and the rest of `.orbit/` stays blocked. `denyRead`
takes no exceptions.

## Machine and workspace policies

A workspace policy merges over the machine policy of the same name: deny rules
accumulate, and a workspace profile replaces a machine profile of the same
name. A workspace `denyModify` exception must stay inside an exception the
machine policy already allows. See [Where state lives](/reference/scoping/).

## What a policy does not cover

Tool access is a separate check. A task's `required_tools` extend the tools an
activity offers, but a filesystem profile, a caller's role or a tool's own
policy can still deny a call. See
[Task tool grants](/concepts/tasks/#task-tool-grants).
