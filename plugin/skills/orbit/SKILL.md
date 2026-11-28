---
name: orbit
description: Creates, executes and reviews Orbit tasks; searches task history and friction records; records evidence. Use for an assigned task or task ID, task authoring, `orbit.task`/`orbit.search` MCP tools, `orbit tool run`, task records under `.orbit/`, or friction. Use orbit-orchestrate for `orbit run` dispatch, drains and failed `jrun-*` runs, and orbit-setup for installing or configuring a machine.
---

# Orbit

Work through the authoritative task record and leave verifiable results.
Read only the reference needed for the current action; this is not a reading
checklist. An injected task snapshot is your starting context.

## Operator attention

Start an orchestrator or on-call inspection with `orbit attention --json` or the read-only `orbit.attention` tool.

Task-level kinds report pilot warnings and holds, human-waiting blocks, open decisions and settled reviews; runtime kinds report failed or stuck runs, possibly stale claims, and stale owner-base deliveries (Operator capability required, or a live resident cycle for its covered workspaces; see [Attention](references/attention-reading.md)); `ci_red` reports a red base branch and `friction_untriaged` lists open friction records; `host_unhealthy` reports failing doctor checks and, across registered hosts, hosts that cannot be read.

Read `degraded` before interpreting an empty result. See [Attention](references/attention-reading.md) for filters, local scopes, suggested actions and the delta cursor.

## Connect and act

- With MCP, discover its tools. In a managed activity, use the injected task
  and inherited workspace binding; `orbit.workspace.list` may be outside the
  activity allowlist. In an unbound session, call `orbit_workspace_list` and
  copy the returned selector, including host qualification when federated.
  For a CLI-only installation, inspect `orbit workspace list/show` on the
  intended host and use that registered workspace. Never substitute another
  host or shadow store.
- Use registered tools: MCP `orbit_task_show`, or the installed CLI's
  `orbit tool run orbit.task.show --input '{"id":"<task-id>"}'`.
  Provenance comes from the session, not a `model` argument. Use `fields` for
  compact reads. The advertised schema decides which inputs and tools are available.
- When a single-task or single-run result includes `url`, format the ID in newly
  authored Markdown prose as `[<id>](<url>)`, leave IDs in commands and code
  as plain text so they stay copyable, and make no claim about rewriting
  earlier transcript rows.
- Update task state through tools, not files under `.orbit/`. Creating a task
  does not authorize dispatch or completion. In a managed activity, implement
  the assigned work; the pipeline owns delivery and lifecycle transitions.
- Verify artifacts, task/run state, diffs and checks. Agent messages are
  advisory. Do not report skipped validation as passing or merged as deployed.
- To check a UI change in a browser, use the host's headless browser kit:
  when it is verified, your environment carries `PLAYWRIGHT_MODULE`,
  `PLAYWRIGHT_BROWSERS_PATH` and `ORBIT_BROWSER_KIT_ENV`. Source
  `"$ORBIT_BROWSER_KIT_ENV"` in the shell that runs the check
  ([Task execution](references/task-execution-ui-checks.md#checking-a-ui-in-a-browser)).
- Use the installed `orbit` binary, never `cargo run -- ...`. Bare
  `orbit task ...` subcommands are the human surface and skip agent
  provenance; agents call `orbit tool run orbit.task.*`. Task IDs come from
  `orbit.task.add`; never invent one. Only a human can force an off-table
  transition (`orbit task update --force` or the dashboard); `orbit.task.update`
  and MCP have no `force`.

## Choose the reference

| Need | Read |
|---|---|
| Implement an assigned task: load, plan, start, validate and hand off | [`references/task-execution-steps.md`](references/task-execution-steps.md) |
| Handle an upgrade admission refusal, a verified no-change or invalid-finding handoff, a sanctioned sandbox deferral, or read-only delegation | [`references/task-execution-handoff-cases.md`](references/task-execution-handoff-cases.md) |
| Hand off a criterion the pilot typed `post_deploy` | [`references/task-execution-post-deploy.md`](references/task-execution-post-deploy.md) |
| Prepare configured agent-filled pull request sections | [`references/task-execution-pr-sections.md`](references/task-execution-pr-sections.md) |
| Check a UI change in a browser or attach its screenshots | [`references/task-execution-ui-checks.md`](references/task-execution-ui-checks.md) |
| Read context selectors, `fields` projections, delivery evidence or listing order; update a task from the human CLI; recover a duplicate | [`references/task-fields-projections.md`](references/task-fields-projections.md) |
| Set `context_files`, dependencies, relations, `regression_from` classification or `parent_id`/`source_task_id` | [`references/task-fields-relations.md`](references/task-fields-relations.md) |
| Set `required_tools` or `granted_tools`, propose a mutation, or grant tools as an operator | [`references/task-fields-tools.md`](references/task-fields-tools.md) |
| Set `requires_labels`, `not_before`, `origin` or `covered_by`, or read `rule_fingerprint` | [`references/task-fields-placement.md`](references/task-fields-placement.md) |
| Record a review verdict, rate a severity, or read scorecard weights | [`references/task-fields-review.md`](references/task-fields-review.md) |
| Decide which validation your lane can run, including native host evidence and user-facing UI | [`references/task-fields-validation.md`](references/task-fields-validation.md) |
| Create or revise a task: the contract, friction fixes, revisions and the description template | [`references/task-authoring-contract.md`](references/task-authoring-contract.md) |
| File a task: prepare and create, bound footprint and evidence, human-only work, host OS routing | [`references/task-authoring-create.md`](references/task-authoring-create.md) |
| Choose a new task's `context_files` selectors, or decide to skip the pilot | [`references/task-authoring-footprint.md`](references/task-authoring-footprint.md) |
| File a split, migration or other task that creates files | [`references/task-authoring-new-files.md`](references/task-authoring-new-files.md) |
| Tag a task: `no-drain`, `ui`, tags that carry instructions, light delivery | [`references/task-authoring-tags.md`](references/task-authoring-tags.md) |
| Review a delivered diff | [`references/task-review.md`](references/task-review.md) |
| Find prior tasks or frictions | [`references/search.md`](references/search.md) |
| Run and interpret task-flow analytics | [`references/concepts.md#task-flow-analytics`](references/concepts.md#task-flow-analytics) |
| Understand Orbit nouns | [`references/concepts.md`](references/concepts.md) |
| Record a concrete recurring obstacle | [`references/friction.md`](references/friction.md) |
| Resolve tool transport, schema shape, store selection, task IDs, host routing or common MCP arguments | [`references/tool-surface-routing.md`](references/tool-surface-routing.md) |
| Find the MCP tool or CLI command for a need | [`references/tool-surface-capability-map.md`](references/tool-surface-capability-map.md) |
| Read GitHub run logs or auth status, resident or distributed drain tool details, model provenance, or operations with no tool | [`references/tool-surface-registry.md`](references/tool-surface-registry.md) |
| Serve MCP or choose its toolsets | [`references/tool-surface-mcp-serve.md`](references/tool-surface-mcp-serve.md) |
| Record and inspect repeated capability costs | [`references/tool-surface-evolution.md`](references/tool-surface-evolution.md) |
| Read codebase risk analytics or path detail | [`references/tool-surface-codebase.md`](references/tool-surface-codebase.md) |
| Handle task-pilot dependencies, retries or pilot warnings | [`references/tool-surface-pilot.md`](references/tool-surface-pilot.md) |
| Raise or read a human decision request | [`references/tool-surface-decisions.md`](references/tool-surface-decisions.md) |
| Read agent scorecards or CI failures by cause | [`references/tool-surface-scorecards.md`](references/tool-surface-scorecards.md) |
| Post, read or close a fleet notice | [`references/tool-surface-notices.md`](references/tool-surface-notices.md) |
| Read attention: scope, filters, envelope, cursor, waiting and fixed values | [`references/attention-reading.md`](references/attention-reading.md) |
| Interpret one attention item kind | [`references/attention-kinds.md`](references/attention-kinds.md) |
| Act as a manager: file a mandate, set cadence, relay grants and directions | [`references/manager-mandates.md`](references/manager-mandates.md) |
| Act as a manager: read checkpoints and the digest, control or take over a resident | [`references/manager-oversight.md`](references/manager-oversight.md) |
| Run one resident cycle: check, recover and act | [`references/resident-cycle-act.md`](references/resident-cycle-act.md) |
| Finish a resident cycle: escalate, record costs, status line and checkpoint | [`references/resident-cycle-escalate.md`](references/resident-cycle-escalate.md) |
| Set up a single-owner distributed drain | [`references/setup/distributed-drain-setup.md`](references/setup/distributed-drain-setup.md) |
| Start and verify a follower's pull drain | [`references/setup/distributed-drain-follower.md`](references/setup/distributed-drain-follower.md) |
| Read the owner's claim and settlement surface for a distributed drain | [`references/setup/distributed-drain-owner-surface.md`](references/setup/distributed-drain-owner-surface.md) |
| Recover a claimed attempt or leftover epic and review state | [`references/setup/distributed-drain-recovery.md`](references/setup/distributed-drain-recovery.md) |
| Open the desktop control center or a task/run through `orbit_ui` | [`references/desktop-navigation.md`](references/desktop-navigation.md) |

For dispatch, backlog supervision and failed runs, use
[orbit-orchestrate](../orbit-orchestrate/SKILL.md). For machine, repository,
MCP, automation or upgrade configuration, use [orbit-setup](../orbit-setup/SKILL.md).

## Lifecycle essentials

Normal delivery is `proposed → backlog → in-progress → review → done`.
Moving to `in-progress` requires a plan from every status except `backlog`;
starting `proposed`, `someday`, or `blocked` work also needs authorization.
`done` is reachable only from `review` and needs completion evidence.

The
lifecycle table does not reopen `done` or `archived`; file a regression as a
new task with a `regression_from` relation. Only an operator's
`orbit task update <id> --status <status> --force` restores them.

`orbit task archive <id>` and the UI archive action are not table moves:
they archive a task in any status, including `done` and `rejected`,
through internal authority. Both refuse to archive a task while a live local
run owns it. An operator can override the refusal with `orbit task archive
<id> --force` or the UI force action. Task history records the override as
`forced`.

The transition table governs `task update
--status` instead, which refuses an off-table move to `archived`.
`rejected` returns to `backlog` or
`in-progress`. Follow supported transitions and the assigned workflow; agents
have no lifecycle force override.

A task assigned to a human crew
(`[crews.<name>] kind = "human"`) is operator work: the operator's own
`orbit task update --status` or dashboard status control sets any status from
`proposed` to `done` with no plan, review, evidence or `--force`, and task
history records it as a `forced` entry naming the human-crew bypass. Agents and
MCP stay on the normal lifecycle for that task, and leaving `done` or
`archived` still needs `--force`.

When you need a person to choose between known options, such as a sign-off, a
price or a policy fork, raise `orbit.decision` action `raise` on a human-crew task you
filed instead of writing a free-text human task: a question, two to four options
(one-line label and consequence, exactly one `recommended`), and a `topic` when
a corpus rule should be able to match the request exactly.

The operator answers,
or a current corpus rule the operator marked `auto` answers an exact topic or
rule id and records that answer as precedent. Either way the answer is in the
task's history, completes the task and unblocks its `blocked_by` dependents.
Free text stays for open-ended asks. Raw decision history is not authority.

Only a merged `current` rule may be cited, and only while its review date has
not passed and its `depends_on` digests still match. A stale or superseded
rule is not cited and does not answer. You cannot set `auto`. An answer that
changes lasting behaviour or policy must be carried into the doc, config or
code that owns it, in the PR of the task it unblocks.
Details: [Tool surface](references/tool-surface-decisions.md#raising-a-decision).

When you work a task directly in an interactive session instead of through a
drain, set its `context_files` to the files the work will touch as you claim
it, and keep them current as the change grows. Drains schedule around a
task's `context_files`, so an interactive claim without them collides with
queued work.

Set the task's `crew` to the registered crew that actually does the work
(for example `opus`), replacing whatever filing assigned. Crew analytics
attribute outcomes by that field.
Tasks minted for delivery coverage (such as `delivery-code-review`) accept `automation-coverage.json` only from their own Orbit run; dispatch them with `orbit run ship <task-id>` instead of running them by hand.
