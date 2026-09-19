---
description: A filing that comes from a finding (not a direct instruction) searches first, and an open duplicate stops it.
tags: [task, smoke]
max_turns: 15
allowed_tools: [Read, Glob, Grep, Skill]
---

The QA sweep on the Mac mini flagged this — file whatever's needed in the orbit workspace: the launchd sweep unit keeps running a stale orbit binary after a brew upgrade, and the scheduler should detect the version skew and alert. I haven't checked whether anything already covers it.
