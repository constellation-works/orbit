# Task fields: selectors, projections and listing

Contents:

- [Refused proposed pickup](#refused-proposed-pickup)
- [Context selectors](#context-selectors)
- [Task projections and delivery evidence](#task-projections-and-delivery-evidence)
- [Task lifecycle and listing](#task-lifecycle-and-listing)
- [Human CLI task update](#human-cli-task-update)
- [Duplicate recovery](#duplicate-recovery)

Use this when choosing task projections, editing context selectors, or setting
dependencies, relations or additional tool requirements.
The connected tool schema is authoritative for supported fields.

## Refused proposed pickup

Starting a proposed task records approval before the start. If the start fails
once approval has landed, the refusal reports the fresh task status and the
underlying reason. A backlog task can be selected by a drain: if it must not
start, promptly move it to `blocked`, then resolve the refusal before retrying.
If the status could not be read, inspect the task before taking further action.
A separate approval continues to authorize backlog admission on its own.

## Context selectors

`orbit.task.update` accepts `context_files` as an array of selectors on MCP.
The CLI also accepts a comma-separated string. A `context` input is refused;
the only field is `context_files`.

- Omitting `context_files` preserves the current list. Supplying it replaces
  the **whole list**, never appends.
- To extend the list, first read the durable `context_files` with
  `orbit.task.show`, then send the deduplicated union of **all existing
  selectors and the additions**. Re-read afterward to verify none were lost.
- `context_files: []` clears the list. A comma-only string such as `","`
  also clears it. An empty string is rejected by the tool; the human CLI's
  `--context ""` clears it. The string `"[]"` is a selector, not an empty
  list.
- Add only files, directories or symbols the task expects to modify or
  delete, or files it will create as described below. Do not add entries only
  for background reading. Prefer `symbol:path#name:kind` for existing
  symbols the change edits, else `file:path` or `dir:path` (see
  [Selector granularity](task-authoring-footprint.md#selector-granularity)); legacy
  raw paths are accepted and upgraded.
- Existence checks verify the filesystem anchor only; a `symbol:` name and
  kind are not looked up. A `symbol:` selector locks its whole file.

Without `allow_missing_context`, a newly submitted missing `file:` selector
whose parent directory exists gets an automatic creation grant for that exact
selector and a warning. A later replacement list retains an existing grant
while it keeps that selector; re-sending that selector needs no flag, and
omitting it revokes the grant.

`allow_missing_context: true` is an explicit grant for every missing selector
in the supplied list, including a file without an existing parent or a missing
directory selector. Review the full list before using it. Syntax, workspace
containment and file/directory kind checks still apply.

When a worker updates its own task from the run's linked worktree, the write
also accepts other missing selectors for that task. Selectors accepted only by
this owner-worker relaxation are returned as `context_files_unverified`; they
do not receive creation grants. A newly submitted missing file with an
existing parent still gets the automatic grant above. This fallback does not
authorize a target for another task or caller.

For the read → full-union update → verify command sequence, see
[Keep context_files current](task-execution-steps.md#step-4--implement-and-validate).

## Task projections and delivery evidence

`orbit.task.show` resolves `id` globally through the host task registry by
default. Cwd, MCP initialize metadata and linked-worktree runtime identities
are not filters. Optional `workspace` is an explicit fail-closed filter
argument. The `workspace` projection returns the owning registry identity
`{id, name}` (or `null` when unregistered); `workspace_path` is not a projection field.

For ordinary `orbit.task.show`, ordinary `orbit.task.add`, ordinary
`orbit.task.update`, and `orbit.task.reject`, the tool input accepts `fields` as
an array of task-record field names, or `field` as a single-field alias; `fields`
takes precedence when both are supplied. Two CLI routes reach these tools, and
they differ in what they can project:

- The direct task verbs: `orbit task show`, `orbit task add` and `orbit task update`.
  Only `orbit task show` takes `--fields`, as a repeated flag or a comma-separated
  string. `orbit task add` and `orbit task update` take no projection flag, so
  their output cannot be projected through the direct verb. `reject` is not a CLI verb.
- The CLI tool runner, `orbit tool run <tool> --input '<json>'`, passes the JSON
  to the tool as its input, so `fields` and `field` work there for the ordinary
  task tools. Its own `--fields` flag keeps only the named top-level keys of the
  output after the call; naming `comments` or `history` also asks the tool to
  include them on a task write.

The ordinary task-record projection catalogue is:

```text
id, workspace, parent_id, title, description, acceptance_criteria,
dependencies, resolved_dependencies, tags, required_tools, granted_tools,
requires_labels, not_before, started_at, scheduled_hold, plan,
execution_summary, context_files,
created_by, created_by_model, planned_by, implemented_by, status, terminal, priority,
complexity, type, pr_status, external_refs, relations,
source_task_id, job_run_id, job_run_machine, crew, resolved_crew,
crew_model, crew_unresolved, os_requirement, placement_wait, experiments,
readiness, pilot_warnings, pilot_notes, open_decisions, final_recovery, completion_hold,
orchestrator, orchestrator_model, orchestrator_model_source, origin,
rule_fingerprint,
created_at, updated_at, comments, history, artifacts
```

`pilot_notes` lists the notes from the task's latest applied assessment. Each
entry has the shape `{label, text, instruction_sha256, operation_id, at}`. It is
`[]` when the latest assessment has no notes, or `null` when the task has no
assessment.

`final_recovery` is the task's last final-recovery decision (`run_id`,
`decision`, `outcome`, `fault`, `fault_evidence`, `fault_judged_by`, `at`),
or `null`. `fault` says whose fault the failed run was: `implementer`,
`specification`, `review`, `infrastructure`, `orbit`, `external` or `unknown`
(also how a decision recorded before faults reads).

`fault_judged_by` is the
provider of the agent that judged it, as Orbit observed it; it is `null`
when Orbit decided without an agent or did not observe a provider. Older
decisions may retain a model name. The field does not name the implementer's crew or model.

`completion_hold` is the `completion.hold` rule that kept a `--complete`
run's task in `review` (`rule`, `field`, `pattern`, `matched`, `run_id`, and
`matched_task_id` when another task of its bundle matched), while the task is
still in that review; otherwise `null`. Approve the task to finish it.

Show without a projection returns the full task. A projection returns only
the requested fields, and its shape depends on the surface and on whether
the names came from `fields` or from the `field` alias:

- Over MCP (and `orbit tool run`), a `fields` array of any length returns an
  object keyed by field name: `fields: ["status"]` returns
  `{"status": "proposed"}` and `fields: ["artifacts"]` returns
  `{"artifacts": [...]}`. Multiple fields return that same keyed object.
- The `field` alias returns one field's bare value: `field: "status"`
  returns `"proposed"` and `field: "artifacts"` returns a bare array. On
  the MCP wire a bare non-object is carried as `{"value": ...}`, or as
  `{"items": [...]}` for an array; that envelope is the transport's, not
  the projection's.
- A single derived `terminal` selection remains keyed as
  `{"terminal": ...}` under both forms. It is read-only and reports
  whether the task refuses implementer writes.

On the CLI, `orbit task show --fields` takes the same names, repeated or
comma-separated. Without `--json`, a single field prints its bare value and
several fields print each under a `Field:` heading. With `--json`, a single
field returns its bare JSON value (`--fields status` returns `"proposed"`,
`--fields artifacts` returns a bare array; `terminal` alone stays keyed as
above) and several fields return an object keyed by field name, whether the
task was read locally or from another host.

Ordinary write responses (`orbit.task.add`, `orbit.task.update` without
`request_id`, and `orbit.task.reject`) without a projection omit the
append-heavy `comments` and `history`; request those fields explicitly when
needed. Ordinary write projections use the task-record field catalogue and the
same shapes above: a `fields` array keys even one field by name, and the `field`
alias returns its bare value.

To get a projected ordinary write result in one invocation, run the write
through `orbit tool run` and put the projection in its input. The write returns
the keyed object, with no readback:

```sh
orbit tool run orbit.task.update --input '{"id":"<task-id>","plan":"<plan>","fields":["id","plan"]}'
```

Or send the write's input unchanged and add the runner's `--fields` filter. The
filter returns the keyed object, and naming `comments` or `history` requests them
from the tool in the same call:

```sh
orbit tool run orbit.task.update --input '{"id":"<task-id>","plan":"<plan>"}' --fields id,plan,comments
```

`orbit task add` and `orbit task update` take no `--fields` flag, so a direct
write returns the unprojected record. After a direct write, read the fields back
with `orbit task show --fields`; the tool runner route above needs no readback.

Guarded writes (`request_id` on `orbit.task.add` or `orbit.task.update`) follow
mode-specific projection rules:

- Guarded creation, guarded edit, and guarded comment reject `fields` and `field`.
- Guarded review (`orbit.task.update` with `request_id`, `expected_revision`,
  `verdict`, and optional `complete`) accepts `fields` or `field` over the
  guarded review response catalogue:

```text
snapshot, replayed, conflict, mutation_applied, refusal, accepted, task_id,
refresh_error, workspace
```

- Guarded review projections use the same `fields` (keyed object) and `field`
  (bare value) shapes on applied or replayed results. A revision conflict
  (`conflict`), pre-mutation refusal (`mutation_applied: false`), or accepted
  write whose refresh failed (`accepted: true`) returns the complete outcome
  receipt regardless of `fields` or `field`.

A write response may also carry optional `warnings` metadata. It is not a task
field: `warnings` is absent when empty, is attached to every object response
(including a one-field `fields` projection), and is refused as a `fields`
selector. A bare single-field projection such as `field: "id"` returns the
bare value and cannot carry it, so use `fields` or omit the projection on
the write when warnings matter.

`crew` selects execution; work only a human can do (credentials, approvals,
physical access, operator-only decisions) sets it to the human-kind crew
(`orbit config get crews.<name>.kind` answers `human`), never an agent crew; with
no human crew, file a `setup`, `needs-operator` chore instead. A human crew
never bypasses an agent gate.

Its body is short: a 1-2 line summary, numbered
steps, fenced `sh` commands and a `Verify:` command, at most 2000 characters;
filing warns otherwise (see [task-authoring-create.md](task-authoring-create.md#work-only-a-human-can-do)).
`orchestrator` records separate orchestration
attribution.

`created_by` is the creator's agent family from the session
(`ORBIT_AGENT_*`, the `--orchestrator` crew, or CLI `--model`);
`created_by_model` is the full model id (`claude-opus-5-5`) recorded once
when that session identity is a full id.

It is never rewritten, and it is
`null` for family-only creates and tasks filed before it existed. The derived, never-persisted `os_requirement` reports the task's
OS tags intersected with the stored crew executor `host_os`, its source, and
unresolved/conflict states.

For show only, request delivery evidence with `field: "delivery"` as the
only field; do not combine it with other fields. Optional `run_id` is allowed
only with this selection and chooses the delivery run. Without it, the newest
task-delivery run submitted with this task is used. `run_id` requires the
delivery selection.

Delivery reports what the run committed and landed for the task: typed status,
base/head and landed commit SHAs, PR number, timestamps and provenance only,
from the host's own commit and merge step records. Missing or inconsistent
evidence is reported as unavailable, never inferred. Full run details remain
on the operator-only `orbit.run` action `show`.

A paginated claimed read of `orbit.task.show` (any of `limit`,
`comments_offset`, `history_offset` or `artifacts_offset`) accepts only `id`,
`model`, `limit` and the three offsets, and cannot be combined with `fields`,
`field` or `run_id`. Drop the projection to page the full task, or drop
pagination for a projection.

For `orbit.task.show` with `snapshot: true`, `workspace` is required and
`run_id`, `view`, `comments_offset`, `history_offset`, and `artifacts_offset`
are rejected. `snapshot: true` accepts `fields` or `field` over the snapshot
response catalogue rather than the task-record catalogue:

```text
schema_version, observed_at, revision, task, actions, comments, history,
artifacts, comments_total, history_total, artifacts_total, reviewed_head,
reviewed_head_reason, truncated_fields, content_truncated, review,
review_reason, workspace
```

- `fields` returns an object keyed by the selected snapshot field(s), and
  `field` returns that single field's bare value (carried as `{"value": ...}`
  or `{"items": [...]}` on the MCP wire for non-objects).
- Supplying `limit` with `snapshot: true` is accepted and ignored, adding
  `"note": "limit was ignored for snapshot: true"` (wrapping a non-object
  `field` projection as `{"value": ..., "note": ...}`).

For `orbit.task.show` with `view: "bounded"`, `workspace` is required and
`limit`, `comments_offset`, `history_offset`, and `artifacts_offset` are
accepted; `fields`, `field`, `run_id`, and `snapshot` are rejected.

## Task lifecycle and listing

The `--status` values on `orbit task list`, `orbit task lint` and the
minted-task `--status` on `orbit auto-task add`/`update` name the same
lifecycle:

- `proposed` — awaiting human approval before entering the backlog.
- `backlog` — approved and queued for work; not yet started.
- `in-progress` — actively being worked on.
- `review` — implementation complete; awaiting review/merge.
- `done` — accepted and closed. Terminal: reopening requires a human
  operator to use `task update --force`.
- `blocked` — temporarily paused (waiting on a dependency or decision).
- `archived` — soft-deleted. Terminal: restoration requires a human
  operator to use `task update --force`.
- `rejected` — declined. Ordinary edits may return it to backlog or
  in-progress; other transitions require a human operator to use
  `task update --force`.
- `someday` — future-scoped, wanted but not yet actionable; agents skip
  someday tasks.

`orbit task list` ordering: with no `--status`, tasks of every status are
listed with non-terminal tasks first and terminal tasks after, each group
newest first; an explicit `--status` filter is newest first. `--limit`
(default 50, at least 1) caps the rows; `total` counts all matching tasks
and `truncated` reports whether the limit omitted any.

`--scheduled`
keeps only future `not_before` schedules, soonest first, including
proposed tasks. `--path` keeps tasks whose `context_files` selectors
apply to the path (`file:`, `dir:`, `symbol:` and bare paths, with
bidirectional containment: a directory matches every selector under it).
`--all` is a deprecated no-op, accepted and ignored.

`orbit.task.eligible` (`orbit task eligible`) lists `backlog` and `proposed`
tasks that can be picked up without colliding with work in flight, as
`{tasks, total, truncated[, conflicting]}`. A task is eligible when its
context-file lock surface overlaps no `in-progress` or `review` task's surface,
decided by the same lock code automatic dispatch uses; no other gate applies
(dependencies, complexity, groups, crew and overlap between candidates are not
checked). Read-only.

`orbit.task.lint` (`orbit task lint`) checks one task (or sweeps active tasks or
those in `--status` when `id` is omitted) for stale paths, vague acceptance
criteria and stale repository names; `--restore-pruned` (`restore_pruned`)
re-declares `context_files` selectors recorded in task history by an earlier
prune (without it nothing is written).

## Human CLI task update

`orbit task update -h` keeps one line per flag; these are the rules behind
them. The fields in [`task-fields-relations.md`](task-fields-relations.md),
[`task-fields-tools.md`](task-fields-tools.md) and
[`task-fields-placement.md`](task-fields-placement.md) apply to the CLI flags of the same name.

- `--approve` takes the task's next approval step: `proposed` -> `backlog`, or
  `review` -> `done`. The transition is chosen from the current status, so it
  cannot be combined with field edits or an explicit `--status`; `--note` is
  recorded on that status history entry.
- `--status` on a task assigned to a human crew takes any status directly,
  with no plan, evidence or review and no `--force`; task history records it
  as a human-crew bypass. Such a task needs `--force` only to leave `done` or
  `archived`.
- `--force` applies `--status` even when the task lifecycle refuses the
  transition (for example reopening a `done` task). It is a human-operator
  override, recorded in task history as `forced`; `orbit.task.update` refuses
  `force`.
- `--discard-candidate` discards the candidate the last failed run preserved,
  so the next run implements fresh instead of resuming it. Combine it with
  `--status backlog` to requeue.
- `--tag` replaces the list. `os:linux`, `os:macos` or `os:windows` limits
  which host OS may run the task from its next admission; any other `os:`
  value is rejected. `--allow-drop-system-tags` permits a replacement that
  drops a system identity tag (`ci-failure:*`).
- `--granted-tools` is accepted only before the task's next attempt is
  dispatched; see [`granted_tools`](task-fields-tools.md).
- `--context` replaces the whole list and `--context ""` clears it; repeat or
  comma-separate selectors, and to extend send every existing selector plus
  the new ones. `--allow-missing-context` records creation intent per missing
  selector. Both follow [Context selectors](#context-selectors).
- `--not-before` and `--clear-not-before` follow [`not_before`](task-fields-placement.md).

## Duplicate recovery

If a creation reply was lost, inspect existing tasks before repeating the write.
For a confirmed duplicate, use the advertised terminal disposition supported by
this server and record the canonical task ID. Do not invent a `cancelled` task
status, force a transition, or assume deletion/rejection is exposed to agents.
