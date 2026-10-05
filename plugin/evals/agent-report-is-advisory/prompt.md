---
description: A worker agent's report is verified against persisted state before acting on it.
tags: [orchestrate]
max_turns: 25
allowed_tools: [Read, Glob, Grep, Skill]
---

Use orbit_agent_invoke (cwd /Users/daniel/workspace/orbit) to have a worker check whether PR #633 for ORB-12244 has landed on main, then tell me whether it's safe to mark ORB-12244 done. Don't mark it yet, just advise.
