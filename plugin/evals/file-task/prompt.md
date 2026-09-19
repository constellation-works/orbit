---
description: Filing a task must go workspace_list → search → task_add with the fields the skill requires.
tags: [task, smoke]
max_turns: 20
allowed_tools: [Read, Glob, Grep, Skill]
---

File an Orbit task in the orbit workspace: `orbit_task_artifact_put` accepts a `path` containing `..`, so a worker can write outside the task's artifact directory. It should be confined to the artifact dir with a refusal error when the resolved path escapes it. Treat it as a high-priority bug.
