---
title: Scoping Rules
description: "Where Orbit stores and merges tasks, activities, jobs, policies, skills, audit, and run data."
sidebar:
  order: 6
---

## Strategies

Each artifact type follows one of three strategies. **Workspace only** keeps
it per repository. **Global only** keeps one copy on the machine. **Merge by
key** combines global defaults with workspace overrides, and the workspace
entry wins for the same key.

| Artifact | Strategy | Meaning |
|----------|----------|---------|
| Tasks | Workspace only | Each repository has its own backlog and lifecycle state. |
| Activities and jobs | Merge by key | Workspace definitions override global defaults by name. |
| Policies | Merge by key | Profiles override by name; global deny rules accumulate. |
| Job runs | Workspace only | Run artifacts stay in the workspace. |
| Skills | Merge by key | Global defaults live in `~/.orbit/skills`; workspace entries override by skill name. |
| Audit | Global only | One authoritative event trail. |

## Typical workspace state

```text
.orbit/
  auto_tasks/        # auto-task definitions
  frictions/         # friction tag taxonomy (tags.yaml)
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

## Rule of thumb

Keep anything that describes this repository's work in the workspace. Keep
reusable execution defaults as global assets, and override them in the
workspace when needed.
