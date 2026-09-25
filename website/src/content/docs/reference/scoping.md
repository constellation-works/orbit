---
title: Scoping Rules
description: "Where Orbit stores and merges tasks, activities, jobs, policies, skills, audit, and run data."
sidebar:
  order: 6
---

## Strategies

| Artifact | Strategy | Meaning |
|----------|----------|---------|
| Tasks | WorkspaceOnly | Per-repository backlog and lifecycle state. |
| Activities and jobs | MergeByKey | Global defaults merge with workspace overrides by key. |
| Policies | MergeByKey | Profiles override by name; global deny rules accumulate. |
| Job runs | WorkspaceOnly | Run artifacts stay local to the workspace. |
| Skills | MergeByKey | Global defaults in `~/.orbit/skills`; workspace entries override by skill name. |
| Audit | GlobalOnly | One authoritative event trail. |

## Typical Workspace State

```text
.orbit/
  auto_tasks/        # auto-task definitions
  frictions/         # friction records, tags.yaml
  knowledge/
  resources/         # activities/, executors/, jobs/, policies/ overrides
  routines/          # routine definitions
  state/
    audit/
    diagnostics/
    job-runs/
    logs/
    scoreboard/
    worktrees/
    auto-tasks.json  # scheduler cursor
    layout.version
    semantic.db      # lexical search index (historical file name)
  config.toml
  config.yaml
```

Task bundles are not under `.orbit/`. They live in the global root at
`~/.orbit/tasks/workspaces/<workspace>/`, next to the audit database
`~/.orbit/orbit.db`.

## Rule of Thumb

If an artifact describes this repository's work, keep it workspace-local. If it describes reusable execution defaults, use mergeable global assets and override them in the workspace when needed.
