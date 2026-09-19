---
name: orbit
description: Creates, executes and reviews Orbit tasks; searches task history and docs; records evidence and friction. Use for an assigned task, task authoring, or everyday orbit.task and orbit.search operations. Use orbit-orchestrate for backlog dispatch and run recovery, and orbit-setup for installing or configuring a machine.
---

# Orbit

Work through the authoritative task record and leave verifiable results.
Read only the reference needed for the current action; this is not a reading
checklist. An injected task snapshot is your starting context.

## Connect and act

- With MCP, discover its tools and call `orbit_workspace_list`; copy the
  returned selector, including host qualification when federated. For a CLI-only
  installation, inspect `orbit workspace list/show` on the intended host and use
  that registered workspace. Never substitute another host or shadow store.
- Use registered tools: MCP `orbit_task_show`, or the installed CLI's
  `orbit tool run orbit.task.show --input '{"id":"<task-id>","model":"<agent-family>"}'`.
  Include your agent family in `model` where supported. Use `fields` for compact
  reads. The advertised schema decides which inputs and tools are available.
- Update task state through tools, not files under `.orbit/`. Creating a task
  does not authorize dispatch or completion. In a managed activity, implement
  the assigned work; the pipeline owns delivery and lifecycle transitions.
- Verify artifacts, task/run state, diffs and checks. Agent messages are
  advisory. Do not report skipped validation as passing or merged as deployed.

## Choose the reference

| Need | Read |
|---|---|
| Implement an assigned task | [Task execution](references/task-execution.md) |
| Set dependencies, relations or validation tools | [Task fields](references/task-fields.md) |
| Create or revise a task | [Task authoring](references/task-authoring.md) |
| Review a deliverable | [Task review](references/task-review.md) |
| Find prior tasks, docs or frictions | [Search](references/search.md) |
| Register or retrieve project documentation | [Docs corpus](references/docs-corpus.md) |
| Record a concrete recurring obstacle | [Friction](references/friction.md) |
| Resolve tool transport, permissions or workspace routing | [Tool surface](references/tool-surface.md) |
| Understand Orbit nouns | [Concepts](references/concepts.md) |

For dispatch, backlog supervision and failed runs, use
[orbit-orchestrate](../orbit-orchestrate/SKILL.md). For machine, repository,
MCP, automation or upgrade configuration, use [orbit-setup](../orbit-setup/SKILL.md).

## Lifecycle essentials

Normal delivery is `proposed → backlog → in-progress → review → done`.
Starting proposed, someday or blocked work requires a plan and authorization.
`done` requires review plus durable completion evidence. Terminal work is not
reopened to fix regressions: create a linked repair task. Follow supported
transitions and the assigned workflow; agents have no lifecycle force override.
