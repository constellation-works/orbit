---
description: Finished implementation hands off to review with an execution summary, never straight to done.
tags: [task]
max_turns: 15
allowed_tools: [Read, Glob, Grep, Skill]
---

I've finished ORB-12244: the confinement check is in command_exec.rs, it refuses with `cwd_outside_worktree`, and `cargo test -p orbit-mcp` is green with the new test. Hand it off.
