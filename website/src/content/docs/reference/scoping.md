---
title: Scoping Rules
description: "Where Orbit stores and merges tasks, activities, jobs, policies, skills, audit, and run data."
sidebar:
  order: 6
---

## Strategies

Each artifact type follows one of three strategies. **Workspace only** keeps
it per repository. **Global only** keeps one copy on the machine. **Merge by
key** combines global defaults with workspace entries, and the workspace
entry wins for the same key, except that shipped activities and jobs always
keep their names (see the table).

| Artifact | Strategy | Meaning |
|----------|----------|---------|
| Tasks | Workspace only | Each repository has its own backlog and lifecycle state. |
| Activities and jobs | Merge by key | Workspace files add new names. A shipped default keeps its name: a workspace file with the same name is ignored when a run resolves it. |
| Policies | Merge by key | Profiles override by name; global deny rules accumulate. |
| Job runs | Workspace only | Run artifacts stay in the workspace. |
| Skills | Merge by key | Global defaults live in `~/.orbit/skills`; workspace entries override by skill name. |
| Audit | Global only | One authoritative event trail. |

## Where state lives

| Path | Holds |
|---|---|
| `~/.orbit/` | Machine state: task bundles, `orbit.db` (audit, runs, routines, frictions), workspace registry, shipped resources, skills, and `config.toml`. |
| `<repo>/.orbit/` | Workspace identity, local config overrides, auto-tasks, routines, worktrees, and logs. This state is gitignored. |

Deleting workspace state gives that checkout a clean slate; it does not delete
the task bundles or audit database in the global root. For backups, stuck runs,
database recovery, and upgrades, see the
[runbooks](https://github.com/constellation-works/orbit/blob/agent-main/docs/INDEX.md#runbooks).

## Typical workspace state

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
reusable execution defaults as global assets. Override a policy profile or
skill in the workspace when needed; for an activity or job, add one under a
new name, because a shipped name is never replaced.
