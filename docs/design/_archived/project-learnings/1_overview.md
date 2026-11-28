---
title: Project Learnings — Overview
owner: claude
created_date: 2026-11-28
updated_date: 2026-11-28
status: Deprecated
feature: project-learnings
doc_role: overview
type: design
summary: "The native project-learning subsystem is retired; leftover .orbit/learnings/ files are inert and ignored"
tags: [project-learnings]
related_features: [project-learnings, workspace-agent-corpus]
related_artifacts: [ORB-10736, ORB-12718]
---

# Project Learnings — Overview

Retired by [ORB-10736]: the native subsystem has no replacement, and durable project rules now live in reviewed docs and the [workspace agent corpus](../../workspace-agent-corpus/1_overview.md).

Project learnings were a native Orbit resource that stored scoped project rules under
`.orbit/learnings/` and delivered them to agents automatically. [ORB-10736] removed the
subsystem and every surface that depended on it; this archived folder keeps the design as
history. What remains is described in [2_design.md](./2_design.md), and
why it was removed is in [4_decisions.md](./4_decisions.md).

## 1. Motivation

Automatic scope-matched delivery did not justify a separate model, store, projections,
lifecycle API, tool surface, dashboard and curation machinery. Durable guidance now lives
in ordinary reviewed documentation and, for agent activities, the
[workspace agent corpus](../../workspace-agent-corpus/1_overview.md).

## 2. Core Concepts

- **Retired resource.** Orbit has no native learning model, store, lifecycle, delivery
  path, tool, route, dashboard surface, metric or scheduled curation task.
- **Inert leftovers.** Existing `.orbit/learnings/**` files are not read, indexed,
  injected or migrated, and are ignored with the rest of `.orbit/` ([ORB-12718]).

## 3. At a Glance

| Concern | Where | Task |
| --- | --- | --- |
| Removal and its forward migration | [4_decisions.md](./4_decisions.md#remove-the-native-project-learning-subsystem) | [ORB-10736] |
| Leftover files stay ignored | [4_decisions.md](./4_decisions.md#ignore-leftover-orbitlearnings-with-the-rest-of-orbit) | [ORB-12718] |

## Task References

- [ORB-10736] — removed the native project-learning subsystem.
- [ORB-12718] — stopped re-including `.orbit/learnings/` in git.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
