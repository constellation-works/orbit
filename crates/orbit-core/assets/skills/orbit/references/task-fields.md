# Task fields and validation access

Use this when choosing task projections, editing context selectors, or setting
dependencies, relations or additional tool requirements.
The connected tool schema is authoritative for supported fields.

## Context selectors

`orbit.task.update` accepts `context_files` as a comma-separated string or an
array of strings. The legacy `context` alias accepts a string;
`context_files` takes precedence when both are supplied.

- Omitting both fields preserves the current list. Supplying either replaces
  the **whole list**, never appends.
- To extend the list, first read the durable `context_files` with
  `orbit.task.show`, then send the deduplicated union of **all existing
  selectors and the additions**. Re-read afterward to verify none were lost.
- `context_files: []` clears the list. A comma-only string such as `","`
  also clears it through either field. An empty string is rejected by the
  tool; the human CLI's `--context ""` clears it. The string `"[]"` is a
  selector, not an empty list.
- Add only files, directories or symbols the task expects to modify or
  delete, or files it will create as described below. Do not add entries only
  for background reading. Prefer `symbol:path#name:kind` for existing
  symbols the change edits, else `file:path` or `dir:path` (see
  [Selector granularity](task-authoring.md#selector-granularity)); legacy
  raw paths are accepted and upgraded.
- Existence checks verify the filesystem anchor only; a `symbol:` name and
  kind are not looked up. A `symbol:` selector locks its whole file.

Use `allow_missing_context: true` to declare a target before creating its
file. Missing targets are rejected by default. Each accepted missing selector
records durable creation intent for that **exact selector**; later writes and
task preparation honor it. Re-sending a declared selector needs no flag, and a
replacement list that omits it revokes the intent. Syntax, workspace
containment and file/directory kind are still checked.

For the read → full-union update → verify command sequence, see
[Keep context_files current](task-execution.md#step-4--implement-and-validate).

## Task projections and delivery evidence

`orbit.task.show` resolves `id` globally through the host task registry by
default. Cwd, MCP initialize metadata and linked-worktree runtime identities
are not filters. Optional `workspace` is an explicit fail-closed filter.

For `orbit.task.show`, `orbit.task.add` and `orbit.task.update`, use `fields`
as a string or array of field names, or `field` as a single-field alias.
`fields` takes precedence when both are supplied. The projection catalogue is:

```text
id, parent_id, title, description, acceptance_criteria,
dependencies, resolved_dependencies, tags, required_tools, granted_tools,
requires_labels, not_before, plan, execution_summary, context_files,
created_by, planned_by, implemented_by, status, terminal, priority,
complexity, type, pr_status, external_refs, relations,
source_task_id, job_run_id, job_run_machine, crew, resolved_crew,
crew_model, crew_unresolved, os_requirement, placement_wait, experiments,
readiness, pilot_warnings, open_decisions, orchestrator, orchestrator_model,
orchestrator_model_source, origin, created_at, updated_at, comments,
history, artifacts
```

Show without a projection returns the full task. A projection returns only
the requested fields as JSON: a single field returns its value, and multiple
fields return an object keyed by field name. A single derived `terminal`
selection remains keyed as `{"terminal": ...}`. It is read-only and reports
whether the task refuses implementer writes.

Write responses without a projection omit the append-heavy `comments` and
`history`; request those fields explicitly when needed. Write projections
use the same field catalogue above.

`crew` selects execution; `orchestrator` records separate orchestration
attribution. The derived, never-persisted `os_requirement` reports the task's
OS tags intersected with the stored crew executor `host_os`, its source, and
unresolved/conflict states.

For show only, `field: "delivery"` selects delivery evidence instead of task
fields. It cannot be combined with other fields. Optional `run_id` chooses
the delivery run; without it, the newest task-delivery run submitted with
this task is used. `run_id` requires the delivery selection.

Delivery reports what the run committed and landed for the task: typed status,
base/head and landed commit SHAs, PR number, timestamps and provenance only,
from the host's own commit and merge step records. Missing or inconsistent
evidence is reported as unavailable, never inferred. Full run details remain
on the operator-only `orbit.workflow.run.show`.

## Behavior-affecting optional fields

- `context_files: ["file:<path>", "dir:<path>", "symbol:<path>#<name>:<kind>"]`
  — optional selectors used for context-lock serialization. An empty list
  holds no context lock. A single-slot run, an explicit ship and owner pull
  admit such a backlog task on the next pass; a drain or ship with more than
  one slot waits for the task pilot (`awaiting_footprint`), then runs it only
  alone (`awaiting_exclusive_slot`). Declared selectors keep their existing
  conflict checks; undeclared edit conflicts surface at landing.
- `dependencies: ["<task-id>", ...]` — prerequisites must reach a satisfying
  status first. Not an `orbit.task.add` input: it is refused as an unknown
  field, like the other creation-time fields below, so set it with
  `orbit.task.update` after creation. Unlike `resolves`, task IDs are global: a prerequisite owned by
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
- A `regression_from` relation may carry
  `"regression": {"nature": "<nature>", "escape": "<escape>"}` (both required;
  no other type accepts it). Choose `unclassified` rather than guess; an
  omitted payload reads back as absent, not as `unclassified`. Unknown values
  are refused with the allowed list.
  - `nature` — what kind of regression the link records:
    - `broke_existing`: the culprit broke behavior that worked before it landed.
    - `defect_in_new`: the defect is in code the culprit itself introduced.
    - `incomplete`: the culprit shipped without part of what its task required.
    - `merge_reintroduced`: a merge restored code or assets an earlier change had removed.
    - `contract_drift`: docs, schemas or a contract drifted from the behavior the culprit shipped.
    - `unclassified`: the evidence does not settle the nature.
  - `escape` — why the delivery gates missed it:
    - `no_test`: no test covered the broken behavior.
    - `test_not_selected`: a covering test existed but the delivery's gates did not run it.
    - `platform_only`: it shows only on a platform or host the gates did not exercise.
    - `review_missed`: it was visible in the diff and review did not flag it.
    - `spec_ambiguous`: the task or spec left the expected behavior open to the shipped reading.
    - `nondeterministic`: it depends on timing, ordering or environment and passed when gated.
    - `unclassified`: the evidence does not settle the escape.
- `parent_id` is not an `orbit.task.add` input and is refused as an unknown
  field, like `plan`, `status` and `dependencies`; use a `child_of` relation
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
- `granted_tools: ["<exact.canonical.tool>", ...]` — tools an **operator**
  grants the implementing activity despite its deny list. Separate from
  `required_tools`: only an operator session may set or change it, on
  `orbit.task.add` or `orbit.task.update`, and any other caller that supplies
  the field (even `[]`) is refused naming the missing operator authority,
  never silently stripped. Names are checked like `required_tools`. An update
  replaces the list (`[]`, or CLI `--granted-tools ""`, clears it) and is
  accepted only while the task is `proposed`, `backlog` or `someday` with no
  live run, footprint-protecting execution claim or worktree. A preserved
  worktree keeps the grant frozen after its run stops or claim is released;
  requeue and remove any preserved worktree before changing the grant for a
  new attempt. Every change records a `granted_tools_changed` history entry
  naming the writer, both tool sets and the run/claim/worktree state. Only
  `agent_implement` reads a grant;
  review, recovery, pilot and every other activity ignore it. A grant lifts
  the deny list only for a tool classified read/repeat-safe. Every tool
  `agent_implement` denies today is classified mutating, so such a grant
  stays denied and is never called live, and also listing it in
  `required_tools` still fails implementer admission. A lifted tool still
  passes every other check: capability and field-level operator gates,
  filesystem and process policy, and claimed-mode denials.
- **Mutation proposals** carry a mutating grant's call instead. Attach
  `mutation-proposals/<name>.json` with `orbit.task.artifact.put`, content
  `{"tool": "<granted tool>", "input": {…exact tool input…}, "rationale":
  "<why>"}`. It is refused unless the task grants the tool, the tool is a
  grantable mutation, and an agent session could make that exact call (a
  grant never satisfies a field-level gate such as `expected_enabled`).
  Orbit stores it with the task id and an `idempotency_key` derived from the
  task, tool and input; nothing changes live. The review gate's manifest
  lists every unapplied proposal for the reviewer, and when that review
  passes the pipeline applies each one once, with the reviewed input, and
  records a `mutation-receipts/<key>.json` receipt (`applied` or `failed`).
  Receipts are Orbit's alone. A re-emitted identical proposal never applies
  twice; a changed one is a new key that waits for the next review. A failed
  or unreviewed proposal fails the review step with a task comment, a run
  that reviews nothing refuses the task, and a task cannot become `done`
  while a proposal is unapplied. A change the agent needs during its run
  cannot be proposed: it is an operator action or a prerequisite task.
- `requires_labels: ["<label>", ...]` — capability labels a host must carry,
  **all of them**, to run the task. A host carries its `machine.labels` setting
  plus its OS as an implicit label (`linux`, `macos`, `windows`); `orbit doctor`
  prints the effective set. `os:` tags keep their any-of meaning and both
  apply. Labels are opaque, trimmed and lowercased. Add and update refuse a
  requirement no known serving host carries with the task's `os:` tags, naming
  the missing labels: this host, plus followers that pulled from the workspace
  in the last seven days. Update replaces the list; `[]` (CLI
  `--requires-labels ""`) clears it. A host lacking a label skips the task
  (`host_labels_missing`), and while no live host carries them task show/list
  and `orbit run readiness` name the wait in `placement_wait`. Use it for a
  real capability (a GPU, a signed toolchain), not to pin a hostname.
- `not_before: "<RFC 3339 instant>"` — the earliest time a dispatcher may
  start the task, stored in UTC and shown only when set. Give an instant with
  an offset (`2026-11-06T09:00:00Z`); the CLI's `--not-before` also takes a
  local `YYYY-MM-DDTHH:MM` in the host's zone and echoes UTC and local time.
  A malformed time is refused naming it. On `orbit.task.update` an empty
  string (CLI `--clear-not-before`) clears it; `null` leaves it unchanged, as
  for any field. Until then a `backlog` task waits `scheduled`, with the time
  in `detail`, in drain listings, `orbit run readiness` and `run show`. No
  local or pull drain starts it, and `orbit run ship <id>` and
  `orbit.workflow.ship` refuse it with `task_scheduled`. The hold lifts by
  itself on the first admission pass after that time, which can be later
  than the time itself. Only an operator's
  `orbit run ship <id> --ignore-schedule` starts it early, recording a
  `schedule_overridden` history event; changing `not_before` reinstates the
  hold. Other statuses ignore it.
- `origin` records why the task was filed, from a closed set:
  `operator_request` (a human operator asked for the work),
  `orchestrator_initiative` (the orchestrator judged it necessary on its own)
  or `automation` (a system filer). Orchestrators set it on every filing; the
  operator's request usually reaches you in chat, so Orbit cannot infer it.
  Tasks minted by auto-task definitions, routines and other system filers, and
  sweep findings tagged `auto-task:*`, `qa-sweep`, `code-review`,
  `security-review` or `friction-curation`, record `automation` when no origin
  is given. Any other filing without one stays unset, which never reads as a
  value. `orbit.task.update` can correct an origin but not clear it, an unknown
  value is refused with the allowed list, and `orbit task list --origin`
  (`origin` on `orbit.task.list`) filters by it.
- `covered_by` names a task or GitHub PR covering an archived/rejected CI
  sweep finding. Its target is a task ID or an external key such as
  `github-pr:NUMBER` (this checkout's repository) or
  `github-pr:https://github.com/OWNER/REPO/pull/NUMBER`. Preserve existing
  relations and append it in the same update that archives/rejects the finding;
  see [CI recovery](../../orbit-orchestrate/references/recovery.md).

## Operator runbook: granting tools

Use an operator session on the task's owner host. Inspect the task and the
exact tool schema first (`orbit task show <id>`, `orbit tool show <tool>`).
Decide whether the work needs a reviewed mutation or a prerequisite operator
action: all currently denied implementer tools are mutating, so a grant does
not enable their live execution. Do not also put a denied mutation in
`required_tools`; that still refuses implementer admission.

1. Before dispatch, set the complete grant list with
   `orbit task update <id> --granted-tools orbit.auto_task.update`.
   Creation also accepts `orbit task add --title "<title>" --complexity low --granted-tools
   orbit.auto_task.update`. These are operator commands; an agent must not
   claim operator authority to run them. A replacement is the whole list,
   not an append; `--granted-tools ""` revokes it before the next attempt.
2. Confirm `Granted Tools` beside `Required Tools` in task show/list and
   inspect `orbit task show <id> --fields history`. The
   `granted_tools_changed` entry records the operator identity, timestamp,
   before/after lists and dispatch state. A grant pins execution to the owner
   host. If required labels exclude the owner, resolve that intersection;
   a follower carrying the labels cannot take over the grant.
3. Have the implementer attach the proposal described above, then inspect it
   with `orbit task show <id> --fields artifacts` and
   `orbit tool run orbit.task.artifact.get --input
   '{"id":"<id>","path":"mutation-proposals/<name>.json"}'`.
   Review the exact input with the diff. A proposal-only delivery without a
   reviewed diff is refused. Approval applies the reviewed proposal before
   push/landing; verify its system-written receipt afterward.
4. Diagnose refusals by their named gate. An exact disallow or wildcard
   refusal is a deny-policy gate; `orbit.routine.control` or another missing
   capability is a capability gate; an owner-host/label wait is placement.
   None is resolved by adding `required_tools`. To change a frozen grant,
   stop/requeue the attempt and remove its preserved worktree first.

`orbit audit list --kind operation --json` exposes authorization decisions;
the `command = "authorization"` rows include grant authorizations (operation
`orbit.task.grant_tools`) and activity-policy/capability denials. Match them
with task history to distinguish an authorized attempt from a committed grant
change. `orbit audit stats --tool orbit.auto_task.update --json` gives the
existing denial-by-operation breakdown; it counts a decision once even when
an entry-point audit row repeats it.

A `failed` receipt is evidence of a failed application. A `pending` receipt
means the result is unknown: do not retry it or edit/forge a receipt. Inspect
the receipt and the actual owner state, retain evidence, and resolve the
outcome as separate operator work. There is no automatic pending-receipt
reconciliation command. A changed proposal creates a new key and requires a
new review; it must not be used to repeat an unknown outcome.

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
another OS (see [Native host evidence](#native-host-evidence)).

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
  override a denial, and such a requirement refuses dispatch. Screenshot and
  task-artifact attachment is part of the implement lane. Owner mode attaches
  with `orbit.task.artifact.put`. Claimed mode attaches declared files through
  the `task_artifacts` handoff even without that tool. A criterion that only
  requires those screenshots is feasible; task-pilot does not warn that
  artifact attachment is unavailable because `required_tools` is empty. A
  criterion that names a tool the lane does not grant, and that
  `required_tools` does not declare, is still reported.
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

### Native host evidence

When a criterion needs execution or evidence that only a native host of one OS
can produce (a real macOS `sandbox-exec` launch, a Linux Bubblewrap result), the
pilot records a typed `required_os` finding naming the one-based criterion and
the OS, and commits it as `native_os_hold` in its assessment audit. Apply
rejects an entry whose criterion text names no OS or native facility (mac,
macos, darwin, seatbelt, sandbox-exec, linux, bwrap, bubblewrap, windows,
launchd, systemd, keychain, cgroup), unless its evidence cites one
`#[cfg(target_os = "<os>")]` gate at a repository-relative `path:line` that
apply confirms in the pinned source, on an item the criterion names, and the
entry's `os` is that OS. A portable criterion, a gate on an item the
criterion does not name, an `os` that differs from the gate, a path or line
the pin does not confirm, and a `cfg(unix)` or `cfg(not(windows))` gate that
does not determine one OS are still rejected, and the rejection tells the
pilot to leave `required_os` empty. When every
finding names one OS and the task carries no `os:` tag, the same atomic write
adds that tag (`os:linux`), so admission routes the task by its tags and a host
of another OS waits as `host_os_mismatch` (see [Executor definitions](../../orbit-setup/references/configuration.md#executor-definitions)). The pilot never removes or replaces
an `os:` tag already on the task. While those tags do not name the required OS
(an operator chose another, or findings name several OSes), a local drain, ship
discovery, `orbit run ship` and an owner's pull admission on a host that cannot
meet it leave the task in `backlog` as `native_os_required`; readiness,
`orbit run show` and the dashboard Drain card name the criterion and the tag.

When only the machine that owns the task store can produce the evidence (its
live store, services or data), the pilot records a `required_machine` finding
naming the criterion and that owner machine, committed in the same hold. The
owner then refuses a pull claim from any other machine with a reason naming
the machine; its own drain may start the task. A requirement on any other
machine stays a `utility_warnings` finding for an operator to arrange.

A claimed leaf that finds its base already resolves such a task hands off
`NoDiff` with an `already_resolved` outcome naming `cargo test -p <crate>
--test <target> [<filter>]` selectors. Its claim host replays each on the base
outside the agent sandbox and pins the passing logs for the owner, so the task
settles without a PR; a failing replay fails the claim with its output, and an
empty list or a selector that runs no test is refused.

When only the deployed build on a named host can evidence a criterion, after
the change merges and is deployed there (that host's live service latency, the
installed binary against its live store), the pilot records a `post_deploy`
finding naming the criterion, the host and the exact measurement command,
committed as `post_deploy` in its assessment audit. It is separate from
`required_machine` and holds no admission: running on that host would still
measure the deployed build, not the candidate. A criterion is typed in one of
the two, never both. The finding only lets the implementer hand the criterion
off as owed after deploy (see the task-execution reference); measurements the
candidate can repeat, such as timing the worktree's own binary, are ordinary
criteria.

Write such work as two kinds of criteria. Candidate criteria are measured by
the implementer with its own build: execve or request counts, payload size,
CLI timing of the worktree build, a test server started from the candidate.
Post-deploy targets are stated as such and name the host and the exact
measurement command, for example "After deploy, on `<host>`: `<command>`
reports p95 <= 150 ms over 20 paced samples". Only the second kind can be
owed.

Only typed findings route work: a host requirement stated in warning prose does
not. Platform mentions, cross-compilation targets, mocked checks, negative
tests and measurements any host could repeat are not findings.

The wait clears when the matching `os:` tag is added (admission then routes the
task by its tags), when the acceptance criteria are re-scoped, when a newer
assessment carries no such finding, or through the same evidenced human
decision as above, which records `native_os_requirement_resolved`.

A trusted native-host decision stays valid across later pilot assessments with
the same acceptance-criteria digest, OS requirement set and machine requirement.
Requirement order does not matter. Changed criteria or requirements supersede
the decision. Operator-validation and host-operational decisions remain scoped
to the assessment they answered; a new assessment supersedes them.

### User-facing UI

When a task's criteria or context files are UI a user sees, the pilot records
`user_facing_ui: true`, and the same atomic write adds the `ui` tag to a task
that lacks it. `false`, null or an absent field adds nothing, and the pilot
never removes a `ui` tag; any other value is refused for repair. The
task-authoring reference (UI tag) defines the tag.

## Duplicate recovery

If a creation reply was lost, inspect existing tasks before repeating the write.
For a confirmed duplicate, use the advertised terminal disposition supported by
this server and record the canonical task ID. Do not invent a `cancelled` task
status, force a transition, or assume deletion/rejection is exposed to agents.
