---
description: Filing a task must go workspace_list → search → task_add with the fields the skill requires.
tags: [task, smoke]
max_turns: 20
allowed_tools: [Read, Glob, Grep, Skill]
---

File an Orbit task in the orbit workspace: `orbit_command_exec` currently accepts any absolute cwd, so a worker can run commands outside its worktree. It should be confined to the run's worktree or a registered workspace root, with a refusal error when the cwd is outside. Treat it as a high-priority bug.
