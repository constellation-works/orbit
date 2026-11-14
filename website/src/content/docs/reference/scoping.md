---
title: Where state lives
description: "What Orbit keeps per machine and per repository, and how global and workspace definitions merge."
---

Orbit keeps state in two places: one root per machine and one `.orbit/`
directory per repository. Use this page to find a file, or to predict which
definition wins when both places define one.

## Where state lives

| Path | Holds |
|---|---|
| `~/.orbit/` | Machine state: task bundles, `orbit.db` (audit, runs, routines, frictions), the workspace registry, shipped resources, skills and `config.toml`. |
| `<repo>/.orbit/` | Workspace state: identity, local config overrides, auto-tasks, routines, worktrees and logs. Git ignores it. |

Task bundles are not under `.orbit/`. They live at
`~/.orbit/tasks/workspaces/<workspace>/`, next to the audit database
`~/.orbit/orbit.db`. Deleting a checkout's `.orbit/` gives that checkout a clean
slate and leaves its tasks and audit history in place. To back either root up,
see [Back up and move state](/how-to/state-and-backup/).

## What a workspace directory contains

```text
.orbit/
  auto_tasks/        # auto-task definitions
  frictions/         # friction tag taxonomy (tags.yaml)
  resources/         # workspace-local activities/, executors/, jobs/, policies/
  routines/          # routine definitions
  state/
    audit/
    diagnostics/
    job-runs/
    logs/
    worktrees/
    auto-tasks.json  # scheduler cursor
    layout.version
    semantic.db      # lexical search index
  config.toml
  config.yaml
```

## How global and workspace definitions merge

| Artifact | Scope | Rule |
|----------|----------|---------|
| Tasks | Workspace | Each repository has its own backlog. |
| Job runs | Workspace | A run belongs to one workspace. Its record is in the machine database; its logs and worktree are under `.orbit/state/`. |
| Audit | Machine | One event trail for the machine. |
| Activities and jobs | Merged by name | A workspace file adds a new name. A file that reuses a shipped name is ignored, so give your own a new name. |
| Policies | Merged by name | A workspace profile replaces the machine profile of the same name. Deny rules accumulate. |
| Skills | Merged by name | Machine defaults live in `~/.orbit/skills`; a workspace skill of the same name replaces one. |
