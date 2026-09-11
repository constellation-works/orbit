# Authoring a task

Write a task another engineer or agent can execute without guessing: a crisp
problem statement plus acceptance criteria that define observable success. The
execution plan is authored later, at pickup, not here.

Every `orbit.task.*` call needs `model` — your agent family. Never use bare
`orbit task ...`; it skips agent provenance.

## Workflow

1. **Establish** the objective, constraints, and what done means from the user's
   request. Ask only for information that is genuinely missing. Select the
   authoritative workspace before searching or writing.
2. **Check for overlapping prior work.** Run a hybrid search on the title and
   description before creating anything — a brand-new task has no embeddings, so
   `--hybrid --kind task` on the text is the check that works. (`search similar`
   needs an existing task with vectors; it is for pickup, not creation.)
   → [search.md](search.md)
3. **Write acceptance criteria that name observable success** — a command, an
   inspection step, or an output. "Works correctly" is not a criterion.
4. **Optionally fill `context_files`** (see below).
5. **Set `complexity`** (`low` / `medium` / `hard`). It is required at
   creation and on every human or agent update — `orbit.task.update`, `orbit
   task update`, and the dashboard all reject `unassessed`, which is reserved
   for automated mint/import and system callers.
6. **Add assumptions, risks, and rollback notes** to the description when they
   matter.
7. **Call `orbit.task.add`.** Confirm via the result, or re-fetch with
   `orbit.task.show`.

```bash
orbit tool run orbit.task.add --input '{
  "title": "<title>",
  "description": "<multi-line markdown>",
  "acceptance_criteria": ["<observable outcome>", "<observable outcome>"],
  "context_files": ["file:src/lib.rs", "dir:src/command"],
  "required_tools": ["<exact.canonical.tool>"],
  "workspace": "<selector>", "priority": "<low|medium|high|critical>",
  "complexity": "<low|medium|hard>", "type": "<feature|bug|refactor|chore>",
  "model": "<agent-family>"
}'
```

## `context_files`

Names *only* modification and deletion targets, as canonical selectors
(`file:`, `dir:`, `symbol:path#name:kind`), each resolving inside the target
workspace's root — an out-of-root path fails pipeline admission.

Read-for-context files, convention and pattern docs, and files that don't exist
yet do not belong there; cite those in prose instead. Include a design doc only
when it exists and is itself an expected modification target.

Prefer `file:`/`symbol:` over `dir:` when the change can be named precisely.

The field is optional unless the workspace's own policy requires it. Leaving it
empty is valid, and **guessing entries to avoid an empty field is worse than
empty** — the list is what conflict detection reads, so a wrong entry actively
misleads. When an orchestrator needs selectors prepared at scale, the task-pilot
job fills them from real inspection. → [orchestration.md](orchestration.md)

## Operating rules

- Never edit task files directly; never invent task IDs (`orbit.task.add`
  allocates them).
- Required: `title`, `description`, `workspace`, `complexity`. Strongly prefer
  `acceptance_criteria`.
- `complexity` stays assessed for its whole life: an update may move it between
  `low`, `medium`, and `hard`, but a human or agent can never set it back to
  `unassessed`.
- `description` should be multi-line markdown for anything non-trivial.
- Valid `type`: `feature`, `bug`, `refactor`, `chore`.
- Do not pass `plan` to task creation; author it later through task update.
- Set `status: proposed` when filing findings for consideration. Creation does
  not imply approval, dispatch, or completion; preserve the user's intent.
- Blank companion files (`plan.md`, `execution-summary.md`) are blank *fields* —
  repair with `orbit.task.update`, never by hand.

## Behavior-affecting optional fields

- `dependencies: ["<task-id>", ...]` — prerequisites must reach a satisfying
  status first.
- `relations: [{"type": "resolves", "target": "<friction-id>"}]` — auto-resolves
  that friction when this task reaches `done`, **only in the same workspace**.
  Friction IDs are workspace-local; an unqualified target is never a global
  lookup. Completing a task whose `resolves` target exists only in another
  workspace on this host is rejected with `friction_not_local` — resolve the
  friction from its owning workspace (`orbit.friction.resolve`, or a covering
  task there) instead. Other types (`produces`, `blocked_by`, `child_of`,
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
  resume, the operation grants, `orbit.command.exec`, `orbit.agent.invoke`,
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

## Quality bar

Validation must not assume uncommitted artifacts or workspace-local runtime
state under `.orbit/state/`. File I/O checks use temp dirs or fakes.
Behavior-changing work that touches external services, the filesystem, or time
should ask for deterministic mock coverage in its acceptance criteria.

## Description template

```markdown
## Problem
<what is broken, missing, or needs to change>
## Why It Matters
<user impact, operational impact, or engineering rationale>
## Constraints / Notes
- <important constraint>
```

Exit: the task exists with a strong description, clear acceptance criteria, and
— when filled — `context_files` naming only real modification targets.
