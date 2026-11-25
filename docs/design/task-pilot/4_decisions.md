---
title: "Task pilot — Decisions"
owner: orbit
created_date: 2026-11-24
updated_date: 2026-11-25
status: Accepted
feature: task-pilot
doc_role: decisions
type: design
summary: "Why Orbit skips the task pilot by default for ready low/medium tasks with resolving context_files, pairs it with a light drain-approval gate, and keeps exclusions and a config opt-out."
---

# Task pilot — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions
remain in place so their original reasoning stays legible.

## Skip the task pilot by default when context_files is filled

**Recorded:** 2026-11-24 · [ORB-27821]

**Code anchors:**
- `crates/orbit-config/src/registry/settings/table/keys_workflow.rs` (`workflow.task_pilot.skip_when_context_filled`)
- `crates/orbit-core/src/application/task/pilot/skip.rs::context_filled_pilot_skip`
- `crates/orbit-core/src/application/automation/members/member_host/observation.rs`
- `crates/orbit-core/src/application/automation/members/member_host/admission.rs`
- `crates/orbit-core/src/application/task/pilot/drain_promotion.rs::promote_without_pilot`
- `crates/orbit-core/src/application/workspace_auto/approvals.rs::select_proposed_approvals`

### Context

Nearly every agent-filed task already declares `context_files` and an assessed complexity. Running
the full task pilot on every such `proposed` or `backlog` task spends inspection tokens even when
the task already meets readiness. On 2026-11-24 Daniel asked for a configurable option to skip
`task_pilot` when `context_files` is already filled and passes readiness checks (including a set
complexity), with that skip enabled by default for token economy, and approved Option B alongside
[ORB-27821].

### Decision

Adopt **Option B** (on by default), governed by the workspace setting
`workflow.task_pilot.skip_when_context_filled` (boolean, default `true`):

- **Default skip.** When `workflow.task_pilot.skip_when_context_filled` is `true`, the
  `task_pilot` routine and automatic pilot discovery do not admit a never-piloted task whose
  `context_files` is non-empty, whose selectors all resolve against the repository, which passes
  proposed-stage readiness (`readiness_gaps`, no blocking gap), and whose complexity is `low` or
  `medium`, unless an exclusion below applies.
- **Light drain-approval gate.** Under the same condition, an `--approve-proposed` drain approves a
  qualifying `proposed` task on readiness alone without dispatching `task_pilot_pipeline`. The
  approval transition records `"approved without pilot (context_files filled)"` and the config key
  `workflow.task_pilot.skip_when_context_filled` in task history (both in the `proposal_approved`
  note and in `TaskDecision::DrainApproved::pilot_skipped_config_key`). Existing pilot holds,
  warnings, unresolved pilots, live pilots, and stale prior assessments are never bypassed.
- **Still piloted, as before:**
  - tasks tagged `design`, `breaking-change`, or `no-auto-approve`;
  - tasks with `hard` or `xhard` complexity;
  - tasks failing readiness (empty `context_files`, missing or `unassessed` complexity, any
    blocking `readiness_gaps` finding, or any selector that does not resolve);
  - tasks that already carry a prior pilot receipt (so stale-assessment re-piloting and warning
    holds stay intact);
  - explicit requests (`orbit run task-pilot <id>`), which always force a pilot.
- **Opt-out.** Setting `workflow.task_pilot.skip_when_context_filled = false` restores piloting
  every eligible task in both the routine and `--approve-proposed` drains. The dashboard task-page
  control for this setting is tracked separately in [ORB-27825].
- **Resident held promotion unchanged.** The resident `promote_held` right ([ORB-27598]) still
  requires a sealed, clean pilot assessment because `no-auto-approve` tasks are excluded from the
  default skip.

Rejected alternatives:

- **Option A — skip without the light approval gate.** Skipping the pilot while keeping drain
  approval gated on a pilot receipt would strand qualifying `proposed` tasks waiting for manual
  human approval, defeating unattended drains.
- **Option C — lighter pilot that skips only footprint discovery.** Running an agent leg for
  duplicate/dependency/criteria checks on every filled low/medium task still pays provider launch
  and context-reading cost on the majority of tasks, saving far fewer tokens than skipping the
  pilot outright.

### Consequences

- Low- and medium-complexity tasks with filled, resolving `context_files` move from `proposed` to
  `backlog` without spending pilot tokens, and task history explicitly flags those approvals as
  unpiloted so downstream failures remain traceable.
- Cost: a skipped task goes without duplicate detection against open tasks, automatic dependency
  discovery, semantic verification that the declared selectors are the right modification footprint
  (only syntactic validity and target resolution are checked), acceptance-criteria typing, a second
  estimate of complexity and crew, and the pilot's findings and holds. Operators who want those
  checks on all tasks can set `workflow.task_pilot.skip_when_context_filled = false` or run
  `orbit run task-pilot <id>`.

## Task References

- [ORB-27821] — skips the task pilot by default when `context_files` is filled and passes readiness, adds the light drain-approval gate and `workflow.task_pilot.skip_when_context_filled`.
- [ORB-27825] — adds the dashboard task-page control for `workflow.task_pilot.skip_when_context_filled`.
- [ORB-27598] — resident `promote_held` right for `no-auto-approve` proposals with a clean pilot.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
