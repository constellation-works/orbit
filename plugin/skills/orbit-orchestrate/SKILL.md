---
name: orbit-orchestrate
description: Prepare and supervise an Orbit backlog — discover useful work, deduplicate tasks, run task-pilot, promote ready work, supervise authorized delivery, and route CI/QA findings into repairs. Use orbit instead for one assigned implementation task.
---

# Orbit Orchestrate

Use Orbit's task and run state to keep authorized work moving. This skill
teaches the operating loop; the companion [orbit skill](../orbit/SKILL.md)
teaches the commands and tool contracts. A managed leaf worker executes its
assigned task and cannot become an orchestrator or dispatch other runs.

## Operating loop

Inspect evidence → search open and closed work → author a bounded task → pilot
→ verify applied context → promote → dispatch → supervise → feed findings back.

- Establish the owning host and authoritative workspace selector first. Use
  that selector on durable MCP operations; never substitute a local store.
- Follow the user's current scope, crew limits, and authorization. Preparation
  can happen before execution starts. Once promotion is authorized and pilot
  has applied valid context, promote promptly; do not invent another waiting
  stage. An active drain can immediately claim a task placed in `backlog`.
- **Completion is opt-in.** Default shipping ends in `review`. When the user
  authorizes delivery through `done`, use `orbit run ship <task-id> --complete`
  or `orbit run auto --for <duration> --complete`. The latter covers work
  admitted throughout that window, including newly prepared tasks. It does
  not itself authorize promotion or a later window.
- Under an authorized continuous-completion policy, post-merge review, QA, and
  CI produce repair tasks; do not insert an unrequested pre-merge review gate.
  Repository protections still apply.
- Attribute the work you orchestrate. `orchestrator` records the crew that
  prepared and supervised a task; it is not the execution crew and grants no
  authority. Set it while the task is `proposed` or `backlog`, either per call
  or once for the session with `orbit mcp serve --orchestrator <crew>`. That
  flag is configuration, not proof of who is calling: a connection outlives a
  model switch, so override it per call or restart the session when the
  orchestrating crew changes.
- Agent reports are advisory. Verify persisted task changes, run outcomes,
  merges, tests, and deployed behavior independently. Do not make activity
  success depend on enforcing the shape of an agent's reported output.
- Preserve user interventions. A changed status or crew is not automatically
  a defect to undo. A stop instruction ends your supervision/new dispatches;
  distinguish that from cancelling workers already running.

## References

| Reference | Read it for |
|---|---|
| [loop.md](references/loop.md) | Discovery, task quality, crew selection, orchestrator attribution, zero-input pilot, immediate promotion, and observation. |
| [authorization.md](references/authorization.md) | Executable `--complete` examples, concurrency and crew limits, window boundaries, and handoff metrics. |
| [recovery.md](references/recovery.md) | CI deduplication, triage, failed completion, operational repair tasks, and deployment verification. |
| [walkthroughs.md](references/walkthroughs.md) | Decisions for missing context, duplicates, locks, unavailable authority, provider limits, and window expiry. |

For unfamiliar Orbit vocabulary, start with [concepts.md](../orbit/references/concepts.md).
For MCP versus CLI routing, read [tool-surface.md](../orbit/references/tool-surface.md).
