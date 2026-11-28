---
title: Conflict Budget — Overview
owner: claude
created_date: 2026-10-25
updated_date: 2026-11-28
last_validated: 2026-11-21
status: Draft
stage: parked
next: "Parked for free time (Daniel, 2026-11-02); on pickup settle open questions, file phase 1"
feature: conflict-budget
doc_role: overview
type: design
summary: "Let footprint admission run overlapping tasks together when the expected cost of repairing their landing conflicts fits an operator budget that can be high by day and low at night, scored per path from recorded conflict history"
tags: [conflict-budget, footprints, admission, locks, conflict-recovery]
---

# Conflict Budget — Overview

Bearing: admission holds a context lock for every declared `context_files` selector and defers a
candidate on any overlap with a held claim, reservation or wave member
(`footprints_block`, defined in [`lock_mode.rs`](../../../crates/orbit-common/src/fs/lock_mode.rs), is consulted by [`coordination/admission/footprint.rs`](../../../crates/orbit-store/src/boundary/coordination/admission/footprint.rs),
via the `overlaps` closure in `selection.rs`, and by `owner_wave.rs` and the runtime lock index;
a footprint-less editing task was treated as a whole-tree lock in multi-slot drains by [ORB-15191]). Three settings already
relax that per path: [`workflow.unlocked_context_paths`](../../config/workflow.md#unlocked-context-paths)
takes no lock for a matching entry of a task that also declares a non-matching one, so the lock
never blocks it on that path; a task whose every entry matches keeps them all locked, because two
rewrites of one document are where a merge produces contradictory text (built-in default
`["**/*.md"]`; init template value `["**/*.json", "**/*.txt", "**/*.md", "**/mod.rs", "**/*.toml"]`);
[`workflow.generated_paths`](../../config/workflow.md#generated-paths-are-regenerated-not-merged)
([ORB-16784]) regenerates a conflicting generated file at landing instead of merging it; and
[`workflow.lock_modes`](../../config/workflow.md#footprint-lock-modes) ([ORB-18157], done) waives an
overlap at a path whose mode is `shared`. `unlocked_context_paths` filters the context lock only.
A live execution claim in a footprint-protecting phase is a separate check: the work-start guard
([ORB-23894]) compares a candidate's whole canonical context, unlocked paths included, with the
claim's frozen footprint, and admission defers the candidate on that answer until the claim settles.
That comparison takes only the lock-mode table, so a `shared` path is waived there too, while
`unlocked_context_paths` and `generated_paths` do not waive it. Backlog discovery, classification,
readiness, explicit selection and local drain admission all apply it. So a task that mixes a
Markdown file with distinct code is still deferred by a claim on that Markdown file: `design.md`
beside `queued.rs` takes no lock, yet a live claim holding `design.md` defers the task until it
settles. A conflict that reaches landing is repaired by `pr_conflict_recovery`, whose crew comes from
`workflow.purpose_crews.failure_recovery` ([ORB-15663]). This feature makes the lock a priced decision instead of a
static allow-list.

## Problem

On 2026-10-24 eleven `auto-task-catalog-2026-10-24` tasks entered ws_orbit's backlog. Each adds
one shipped auto-task: a YAML file of its own, one line in the `DEFAULT_AUTO_TASK_FILES` list in
`crates/orbit-core/src/app/auto_task/mod.rs`, and a regenerated
`crates/orbit-core/assets/auto_tasks/body-history.json`. Their own files never overlap, yet they
run one at a time, because all eleven declare those two shared files. The same happens wherever
work fans into a registry, a generated index or a hub config such as
`crates/orbit-config/assets/default-config.toml`.

Run one at a time, the eleven take eleven delivery wall-times. Run together, the ten that land
second or later each rebase, and a conflict on the registry line costs one cheap recovery
round. The operator wants to make that trade **while at the desk** — their time costs more than a
haiku rebase — and to keep the conservative behaviour **overnight**, when nobody is waiting and a
broken `agent-main` would sit until morning.

Orbit has no way to express that trade today:

- The lock is static. `unlocked_context_paths` and `workflow.lock_modes` are static lists that
  apply all day, however rarely a given path actually conflicts.
- Orbit does not know what a conflict on a given path costs, or how often overlapping work on it
  conflicts. What it records today:

  | Outcome | Where the conflicting paths are recorded | Read by codebase facts |
  |---|---|---|
  | Conflict recovery failed, local run (`pr_conflict_blocked`, or `pr_failure_handoff` with `failed_step=sync_base`) | Task history note text (`conflicts=a, b`) | Yes, `BlockCause::BaseConflict(paths)` in [`block_cause.rs`](../../../crates/orbit-core/src/application/task/block_cause.rs) (read by codebase facts through `app/codebase/fact/block_cause.rs`, which reads the claim settlements and re-exports the classifier) |
  | Conflict recovery failed, claimed leaf (`ClaimFailureClass::BaseConflict`) | Nowhere on the owner: the settlement carries the class only | As a conflict with no paths |
  | Conflict recovered by `pr_conflict_recovery` | The run's `PipelineState::rebase_recovery_checkpoints` (`RebaseRecoveryCheckpoint::conflicting_paths`), on the host that ran it | No |
  | Overlapping work that rebased cleanly | Nowhere directly; derivable from landed paths and in-flight intervals | No |
  | Admission refused for a lock conflict | `orbit.task.locks.reserve` denials (`snapshot.retention().task_lock_denials`) | Yes, `LockDenial` |

  The commonest and cheapest outcome — a recovered conflict — is the one the owner cannot see,
  and nothing records what a recovery cost.

## Goal

The operator sets how much expected conflict-repair work Orbit may take on to run overlapping
tasks together, and can set it higher during working hours than overnight. Admission admits an
overlapping candidate when the expected repair cost of its overlaps, estimated per path from
recorded history, fits that budget. With the budget at its default of zero, admission is exactly
today's: an overlap at a `shared` path (`workflow.lock_modes`) is waived as it is now, every other
locked path is exclusive, and `unlocked_context_paths` still takes no lock for a matching entry
beside a non-matching one. Landing,
conflict recovery and review are unchanged; the budget changes only which work runs at the same
time.

## Non-goals

- **Changing landing or conflict recovery.** The budget only decides what runs at the same time;
  recovery keeps its crews, rounds and generated-path regeneration.
- **Language-aware locking in core.** Symbol resolution and call-graph analysis stay in a plugin (Shape §5),
  per Orbit's generic-core rule.
- **An agent in the admission path.** Scores come from recorded facts and an optional durable
  weights artifact; admission never waits on, or trusts, an agent's answer.
- **Auto-tuning the budget.** The operator sets it. Tying it to usage pressure or queue depth is a
  later question.

## Acceptance (feature level)

- With `workflow.conflict_budget` unset or `0`, every admission decision is identical to today's,
  for local waves, owner pull admission and lock reservations.
- After a conflict is recovered on any host, the owner's task holds a conflict record naming the
  paths, the resolution and the recovery cost, and `orbit codebase show` counts it for each path.
- Given a positive budget and two backlog tasks overlapping only on a path whose risk fits it,
  both are admitted together, and the diagnostic names the overlap set, its risk and the budget;
  over budget, the later one is deferred with the same detail.
- A path matching `exclusive_context_paths` is never shared, and no path ever has more than
  `conflict_budget_max_holders` holders.
- Inside a configured window the window's budget applies; outside it the default does; a drain
  started with `--conflict-budget` uses that value for its whole window, and every receipt records
  the budget it used.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## Task References

- [ORB-15191] — treated a footprint-less editing task as a whole-tree lock in multi-slot drains
- [ORB-15663] — routed conflict recovery through the failure-recovery purpose pool
- [ORB-16784] — regenerated configured generated paths at conflict recovery instead of merging them
- [ORB-18157] — landed per-path regex lock modes as `workflow.lock_modes` (exclusive/shared)

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
