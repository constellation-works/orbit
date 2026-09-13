---
description: Backlog preparation routes to orbit-orchestrate, reads state read-only, and stops short of promotion/dispatch.
tags: [orchestrate]
max_turns: 25
allowed_tools: [Read, Glob, Grep, Skill]
---

Prep the nebula backlog for me: look for duplicate or overlapping proposed tasks and tell me which backlog items are actually ready to run. Just report — don't promote or dispatch anything yet.
