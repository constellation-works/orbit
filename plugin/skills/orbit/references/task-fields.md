# Task fields and validation access

Use this when setting dependencies, relations or additional tool requirements.
The connected tool schema is authoritative for supported fields.

## Behavior-affecting optional fields

- `context_files: ["file:<path>", "dir:<path>", "symbol:<path>#<name>:<kind>"]`
  — optional selectors used for context-lock serialization. Local auto, ship
  and owner pull admit an otherwise eligible backlog task with an empty list
  on the next pass without a context lock. Declared selectors keep their
  existing conflict checks; undeclared edit conflicts surface at landing.
- `dependencies: ["<task-id>", ...]` — prerequisites must reach a satisfying
  status first. Not an `orbit.task.add` input: it is refused with the other
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
  but inert. Only `produces`/`resolves` accept non-task artifact IDs; `covered_by`
  also accepts GitHub PR external keys as described below. The remaining
  relation types require a task ID. A dangling target (unknown in every
  workspace this host can see) succeeds but emits a `TaskRelationDangling` audit event.
- `parent_id` is a retired `orbit.task.add` input and is refused with the
  other entries in `RETIRED_TASK_ADD_INPUT_FIELDS`; use a `child_of` relation
  in `relations` when creating a subtask. `source_task_id` is also retired
  from `orbit.task.add`; for bug tasks, set it after creation with
  `orbit.task.update` (which accepts the field), and use an empty string there
  to clear it. `tags` (reuse existing before inventing new).
- `required_tools: ["<exact.canonical.tool>", ...]` — tools the task requires.
  Allowlist activities add them to their baseline; deny-list activities refuse
  a requirement covered by `tool_disallow_list`, which it never overrides.
  Use only exact, active, agent-facing registered
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
  an allowlist activity stays unchanged and
  `effective_tools = activity baseline ∪ those five`. The shipped deny-list
  `agent_implement` already includes those GitHub reads. Reaching
  `github.auth.status` can still yield a structured `available: false` or
  `authenticated: false` capability-unavailable result when the lane has no
  GitHub client or credentials; that is not a clean CI pass.
- `covered_by` names a task or GitHub PR covering an archived/rejected CI
  sweep finding. Its target is a task ID or an external key such as
  `github-pr:NUMBER` (this checkout's repository) or
  `github-pr:https://github.com/OWNER/REPO/pull/NUMBER`. Preserve existing
  relations and append it in the same update that archives/rejects the finding;
  see [CI recovery](../../orbit-orchestrate/references/recovery.md).

## Validation your lane can actually run

A criterion that names a tool is a promise about the lane that will run it, and
`required_tools` records that requirement — which is why it
has to be right at creation. Task-pilot preparation reads each criterion
against the registered tool surface, the canonical MCP tool list, the
implementation activity's allowlist, and the governed-operation registry, and
reports one `validation_tool_warnings` finding per contradiction before the
task is admitted. Preparation stays advisory. Transport, allowlist, credential,
and utility findings do not withhold an already-approved backlog task. A current
operator-reserved validation requirement holds local workflow admission and
owner pull claims until an operator handles it, and a criterion that needs
native evidence from an OS the task's `os:` tags do not name holds a host of
another OS (see [Native OS evidence](#native-os-evidence)).

- **Transport.** An MCP session reaches only MCP-advertised tools. `proc.spawn`
  is registered CLI-only, so a criterion that requires it over MCP can never
  pass — drive it through `orbit tool run proc.spawn` instead. Never ask for
  the MCP surface to be widened to match a criterion's wording.
  `proc.spawn` clamps a larger `timeout_ms` to its ceiling: 60 seconds (60000
  ms) outside a managed activity; inside one, the activity's remaining
  wall-clock budget, at most `execution.proc_spawn_max_timeout_minutes`
  (default 45). Its result reports the applied `timeout_ms`,
  `timeout_ceiling_ms`, `timeout_ceiling_source`, `timeout_clamped` with a
  `timeout_notice` when clamped, and the explicit `requested_timeout_ms` when
  supplied. A timed-out result includes a `hint` with `transport: native_shell`
  and a message directing long validation to that transport. Run build, test,
  cargo and make validation that can take minutes in the provider's native
  shell session (Codex: `exec_command`/`write_stdin`) or another long-running
  transport the lane provides. A `proc.spawn` timeout on cargo/make is never a
  validation blocker; rerun there and await completion, including shared
  build-budget admission. Preserve sandboxing and build-budget admission.
- **Activity tools.** For an allowlist activity, a tool outside its baseline
  has to be in `required_tools`. A deny-list activity exposes registered
  agent-facing tools except its disallowed names; `required_tools` cannot
  override a denial, and such a requirement refuses dispatch.
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

The pilot commits a typed `operator_validation_hold` in its assessment audit and
an `operator_validation_held` history event, naming each one-based criterion and
canonical tool. This hold also applies when a human approved the task before the
pilot ran. It is scoped to the assessed task material: a changed criterion makes
the old assessment stale, while priority changes and discussion leave it current.
An operator can re-scope the criterion or add a human task comment with one of
these first lines and non-empty evidence on subsequent lines:

- `task-pilot-admission: evaluated` — describe the completed evaluation and
  reference a non-empty evaluation artifact attached to the task.
- `task-pilot-admission: clear` — explain why the requirement is satisfied.
- `task-pilot-admission: approve-anyway` — record the deliberate override and why
  the managed task can proceed.

These human updates record `operator_validation_resolved`. Attaching an artifact
alone or copying the header into an agent comment does not resolve the hold.
A new pilot assessment supersedes an earlier decision. No resolution grants a
tool or changes governed-operation authority.

Older pilot receipts without the typed material snapshot are interpreted only
while no document edit follows the assessment. Their governed-operation warnings
are checked against the current registry and positive criterion mentions; all
other warning kinds stay advisory.

### Native OS evidence

When a criterion needs execution or evidence that only a native host of one OS
can produce (a real macOS `sandbox-exec` launch, a Linux Bubblewrap result), the
pilot records a typed `required_os` finding naming the one-based criterion and
the OS, commits it as `native_os_hold` in its assessment audit, and adds a
`utility_warnings` entry naming the tag to add when the task lacks it. The pilot
never edits tags. While the task's `os:` tags do not name that OS, a local
drain, ship discovery, `orbit run ship` and an owner's pull admission on a host
of another OS leave the task in `backlog` as `native_os_required`; readiness,
`orbit run show` and the dashboard Drain card name the criterion and the tag. A
host of that OS may still take it, and a task whose own `os:` tags exclude the
host keeps `host_os_mismatch`. Platform mentions, cross-compilation targets,
mocked checks and negative tests are not findings.

The wait clears when the matching `os:` tag is added (admission then routes the
task by its tags), when the acceptance criteria are re-scoped, when a newer
assessment carries no such finding, or through the same evidenced human
decision as above, which records `native_os_requirement_resolved`.


## Duplicate recovery

If a creation reply was lost, inspect existing tasks before repeating the write.
For a confirmed duplicate, use the advertised terminal disposition supported by
this server and record the canonical task ID. Do not invent a `cancelled` task
status, force a transition, or assume deletion/rejection is exposed to agents.
