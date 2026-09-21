# Task fields and validation access

Use this when setting dependencies, relations or additional tool requirements.
The connected tool schema is authoritative for supported fields.

## Behavior-affecting optional fields

- `dependencies: ["<task-id>", ...]` — prerequisites must reach a satisfying
  status first. Not an `orbit.task.add` input: it is stripped with the other
  `RETIRED_TASK_ADD_INPUT_FIELDS`, so set it with `orbit.task.update` after
  creation. Unlike `resolves`, task IDs are global: a prerequisite owned by
  another workspace registered on this machine is read from its owner, and
  completing it there satisfies the dependency here. A prerequisite this
  machine has never registered stays explicitly unverifiable — it is never
  treated as satisfied from here, and it is never restored or edited from the
  depending workspace.
- `relations: [{"type": "resolves", "target": "<friction-id>"}]` — auto-resolves
  that friction when this task reaches `done`, **only in the same workspace**.
  Friction IDs are workspace-local; an unqualified target is never a global
  lookup. Completing a task whose `resolves` target exists only in another
  workspace on this host is rejected with `friction_not_local` — resolve the
  friction from its owning workspace instead: `orbit friction resolve <id>` is
  the operator CLI path, or an agent runs `orbit tool run
  orbit.friction.update --input '{"id":"<id>","status":"resolved"}'` (same
  resolution metadata); a covering task there also counts. Other types
  (`produces`, `blocked_by`, `child_of`,
  `spawned_from`, `regression_from`, `supersedes`, `related_to`) are tracked
  but inert. Only `produces`/`resolves` accept non-task targets; the rest
  require a task ID. A dangling target (unknown in every workspace this host
  can see) succeeds but emits a `TaskRelationDangling` audit event.
- `parent_id` is a retired `orbit.task.add` input and is stripped with the
  other entries in `RETIRED_TASK_ADD_INPUT_FIELDS`; use a `child_of` relation
  in `relations` when creating a subtask. `source_task_id` is also retired
  from `orbit.task.add`; for bug tasks, set it after creation with
  `orbit.task.update` (which accepts the field), and use an empty string there
  to clear it. `tags` (reuse existing before inventing new).
- `required_tools: ["<exact.canonical.tool>", ...]` — tools the task must add to
  any agent activity's baseline. Use only exact, active, agent-facing registered
  names; wildcards and prefixes are rejected at dispatch. The list is normalized,
  sorted, and deduplicated at creation. It is immutable afterward, and every
  existing-task update surface rejects `required_tools` — so declare every tool
  your acceptance criteria name **at creation**, because setting it later
  cannot widen an already-dispatched activity. → [Validation your lane can
  actually run](#validation-your-lane-can-actually-run)
  Inclusion grants only activity allowlist membership: caller role, host
  capability, tool policy, filesystem/subprocess policy, and external
  authentication can still deny execution. A task that names exactly
  `github.auth.status`, `github.run.list`, `github.run.view`,
  `github.run.logs`, and `github.pr.list` is the worked example:
  `agent_implement` stays unchanged and
  `effective_tools = activity baseline ∪ those five`. Reaching
  `github.auth.status` can still yield a structured `available: false` or
  `authenticated: false` capability-unavailable result when the lane has no
  GitHub client or credentials; that is not a clean CI pass.

## Validation your lane can actually run

A criterion that names a tool is a promise about the lane that will run it, and
`required_tools` is the only field that can keep that promise — which is why it
has to be right at creation. Task-pilot preparation reads each criterion
against the registered tool surface, the canonical MCP tool list, the
implementation activity's allowlist, and the governed-operation registry, and
reports one `validation_tool_warnings` finding per contradiction before the
task is admitted. A finding never rejects a task; it names the repair.

- **Transport.** An MCP session reaches only MCP-advertised tools. `proc.spawn`
  is registered CLI-only, so a criterion that requires it over MCP can never
  pass — drive it through `orbit tool run proc.spawn` instead. Never ask for
  the MCP surface to be widened to match a criterion's wording.
- **Allowlist.** A tool outside the implementation activity's baseline has to
  be in `required_tools`, or the criterion is unreachable from the lane.
- **Operator capability.** Governed operations — workflow run observation and
  resume, `orbit.command.exec`, `orbit.agent.invoke`,
  and the other destructive surfaces — are reserved for an operator.
  `required_tools` grants allowlist membership, not capability, and an
  implementing agent must never set an authority environment variable to get
  past that gate. Write the criterion as an explicit operator handoff, or scope
  it to the registered dispatch path an agent can reach.
- **External credentials.** `github.*` reads depend on authentication the lane
  may not hold, so state that precondition instead of treating a
  capability-unavailable result as a pass.

A criterion that *expects* a refusal is a correct negative test, and a tool
name inside a quoted example is a copied observation, not a requirement.
Neither is reported, and neither grants anything.


## Duplicate recovery

If a creation reply was lost, inspect existing tasks before repeating the write.
For a confirmed duplicate, use the advertised terminal disposition supported by
this server and record the canonical task ID. Do not invent a `cancelled` task
status, force a transition, or assume deletion/rejection is exposed to agents.
