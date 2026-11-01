---
name: orbit
description: Creates, executes and reviews Orbit tasks; searches task history and friction records; records evidence. Use for an assigned task or task ID, task authoring, `orbit.task`/`orbit.search` MCP tools, `orbit tool run`, task records under `.orbit/`, or friction. Use orbit-orchestrate for `orbit run` dispatch, drains and failed `jrun-*` runs, and orbit-setup for installing or configuring a machine.
---

# Orbit

Work through the authoritative task record and leave verifiable results.
Read only the reference needed for the current action; this is not a reading
checklist. An injected task snapshot is your starting context.

## Connect and act

- With MCP, discover its tools. In a managed activity, use the injected task
  and inherited workspace binding; `orbit.workspace.list` may be outside the
  activity allowlist. In an unbound session, call `orbit_workspace_list` and
  copy the returned selector, including host qualification when federated.
  For a CLI-only installation, inspect `orbit workspace list/show` on the
  intended host and use that registered workspace. Never substitute another
  host or shadow store.
- Use registered tools: MCP `orbit_task_show`, or the installed CLI's
  `orbit tool run orbit.task.show --input '{"id":"<task-id>","model":"<agent-family>"}'`.
  Include your agent family in `model` where supported. Use `fields` for compact
  reads. The advertised schema decides which inputs and tools are available.
- Update task state through tools, not files under `.orbit/`. Creating a task
  does not authorize dispatch or completion. In a managed activity, implement
  the assigned work; the pipeline owns delivery and lifecycle transitions.
- Verify artifacts, task/run state, diffs and checks. Agent messages are
  advisory. Do not report skipped validation as passing or merged as deployed.
- To check a UI change in a browser, use the host's headless browser kit:
  when it is verified, your environment carries `PLAYWRIGHT_MODULE`,
  `PLAYWRIGHT_BROWSERS_PATH` and `ORBIT_BROWSER_KIT_ENV`. Source
  `"$ORBIT_BROWSER_KIT_ENV"` in the shell that runs the check
  ([Task execution](references/task-execution.md#checking-a-ui-in-a-browser)).
- Use the installed `orbit` binary, never `cargo run -- ...`. Bare
  `orbit task ...` subcommands are the human surface and skip agent
  provenance; agents call `orbit tool run orbit.task.*`. Task IDs come from
  `orbit.task.add`; never invent one. Only a human can force an off-table
  transition (`orbit task update --force` or the dashboard); `orbit.task.update`
  and MCP have no `force`.

## Desktop navigation

When the connected client advertises `orbit_ui_open` / `orbit_ui_inspect`, use
those read-only entrypoints to open the control center or a selected task/run.
Discover workspaces first and copy the exact returned selector; pass `workspace`
and public `id` together, with `kind: run` for a run. Keep ordinary task tools
available when the client cannot render the UI.

The panel can capture proposed tasks, edit allowed fields, comment and record
reviews through separately guarded desktop tools. A click or conversation
reference grants no operator authority. Reread authoritative task state before
acting on a copied reference. Keep the same request ID/payload when a write's
outcome is unknown; only a definite refusal permits a corrected fresh request.
Native support depends on the actual desktop build and connection; an installed
plugin or passing protocol test alone does not prove rendering/context delivery.

## Choose the reference

| Need | Read |
|---|---|
| Implement an assigned task | [Task execution](references/task-execution.md) |
| Prepare configured agent-filled pull request sections | [Agent-filled PR sections](references/task-execution.md#agent-filled-pr-sections) |
| Set dependencies, relations or validation tools | [Task fields](references/task-fields.md) |
| Create or revise a task | [Task authoring](references/task-authoring.md) |
| Review a delivered diff | [Task review](references/task-review.md) |
| Find prior tasks or frictions | [Search](references/search.md) |
| Run and interpret task-flow analytics | [Task-flow analytics](references/concepts.md#task-flow-analytics) |
| Record a concrete recurring obstacle | [Friction](references/friction.md) |
| Resolve tool transport, permissions or workspace routing | [Tool surface](references/tool-surface.md) |
| Understand Orbit nouns | [Concepts](references/concepts.md) |
| Single-owner distributed drain setup or claimed-attempt recovery | [Distributed drain](references/setup/distributed-drain.md) |

For dispatch, backlog supervision and failed runs, use
[orbit-orchestrate](../orbit-orchestrate/SKILL.md). For machine, repository,
MCP, automation or upgrade configuration, use [orbit-setup](../orbit-setup/SKILL.md).

## Lifecycle essentials

Normal delivery is `proposed → backlog → in-progress → review → done`.
Moving to `in-progress` requires a plan from every status except `backlog`;
starting `proposed`, `someday`, or `blocked` work also needs authorization.
`done` is reachable only from `review` and needs completion evidence. The
lifecycle table does not reopen `done` or `archived`; file a regression as a
new task with a `regression_from` relation. Only an operator's
`orbit task update <id> --status <status> --force` restores them, and
`orbit task archive <id>` is that table's move to `archived` (`done` and
`rejected` are not sources). `rejected` returns to `backlog` or
`in-progress`. Follow supported transitions and the assigned workflow; agents
have no lifecycle force override. A task assigned to a human crew
(`[crews.<name>] kind = "human"`) is operator work: the operator's own
`orbit task update --status` or dashboard status control sets any status from
`proposed` to `done` with no plan, review, evidence or `--force`, and task
history records it as a `forced` entry naming the human-crew bypass. Agents and
MCP stay on the normal lifecycle for that task, and leaving `done` or
`archived` still needs `--force`.

When you need a person to choose between known options, such as a sign-off, a
price or a policy fork, raise `orbit.decision.raise` on a human-crew task you
filed instead of writing a free-text human task: a question, two to four options
(one-line label and consequence, exactly one `recommended`), and an optional
`topic`. Only the operator answers; the answer is recorded in the task's history,
completes the task and unblocks its `blocked_by` dependents. Free text stays for
open-ended asks. A decision is scoped to its task, and past answers are never
precedent. An answer that changes lasting behaviour or policy must be carried into
the doc, config or code that owns it, in the PR of the task it unblocks.
Details: [Tool surface](references/tool-surface.md#raising-a-decision).

When you work a task directly in an interactive session instead of through a
drain, set its `context_files` to the files the work will touch as you claim
it, and keep them current as the change grows. Drains schedule around a
task's `context_files`, so an interactive claim without them collides with
queued work.
Set the task's `crew` to the registered crew that actually does the work
(for example `opus`), replacing whatever filing assigned. Crew analytics
attribute outcomes by that field.
