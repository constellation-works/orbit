---
title: Automation Triggers — Decisions
owner: claude
last_updated: 2026-11-15
last_validated: 2026-11-15
status: Draft
feature: automation-triggers
doc_role: decisions
type: design
summary: Standing rules for automation triggers; an operator's `no-pilot` tag skips ordinary pilot holds and leaves a current host-operational hold in place, and `readiness-waived` waives backlog readiness gaps.
tags: [automation-triggers, pilot]
paths: ["crates/orbit-core/src/application/task/pilot/**", "crates/orbit-core/src/application/task/readiness_waiver.rs"]
related_features: [routines, user-interface]
related_artifacts: [ORB-21238, ORB-21767, ORB-25084]
---

# Automation Triggers — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## `no-pilot` is an operator skip of pilot admission

**Recorded:** 2026-11 · [ORB-21238]

### Context

Editing a task's description or acceptance criteria after its pilot makes admission fail closed as `pilot_assessment_stale` until a new assessment completes — an `--approve-proposed` drain or scheduled discovery repilots it automatically when no live execution claim holds the task, or an operator requests one explicitly — ([design §6](2_design.md#6-pilot-eligibility-and-freshness)). When an operator has just settled the footprint and criteria themselves, as for a docs-only design task, that second pilot only adds delay. The `no-pilot` tag already existed but meant only "skip the first automatic pilot", and admission ignored it once a receipt existed.

### Decision

A task carrying `no-pilot` is not held by a stale assessment or by an ordinary receipt hold (operator validation, native OS, duplicate, already landed) at any admission path (drain, `orbit run ship`, pull admission, readiness). A current host-operational hold is not one of those holds: admission keeps it until an operator records `task-pilot-admission: evaluated`, `clear` or `approve-anyway` with evidence, the acceptance criteria are re-scoped, or a newer assessment supersedes it. [ORB-21238] recorded the tag as also skipping host operational; that part was narrowed later and is restated in [no-pilot leaves a current host-operational hold](#no-pilot-leaves-a-current-host-operational-hold). The unverified review-finding hold stays: it is a review gate, not a pilot assessment. Readiness gaps, context locks, dependencies and delivery holds are other rules and are unchanged. Preparation eligibility keeps its meaning, and an explicit repilot still works on a tagged task.

Adding the tag to a task that has a pilot receipt or a pending pilot is operator authority. An attributed agent, a tool writer or a writer inside an agent envelope is refused with the whole write, never stripped of the tag. The operator's write appends a `pilot_skipped` history entry with the actor, the receipt's operation id and sealed digest, any pending pilot runs, and the requirements digest the write leaves. Filing a task with the tag, or adding it before any pilot, stays open to every caller. Removing the tag restores the gate for the holds the tag releases.

The dashboard Readiness card shows "Skip pilot" only while the drain holds a backlog task on a pilot reason the tag releases. It does not offer Skip pilot for `host_operational_handoff`. That card offers re-scope and, when the hold is resolvable, Clear hold and Approve anyway, and it does not say dispatch proceeds while the hold remains. Skip pilot adds the tag through the ordinary guarded task update and re-reads the task's readiness.

### Consequences

- The stale-hold detail, `orbit run readiness` and the Readiness card name both remedies: `orbit task repilot <id>` and the `no-pilot` skip.
- A proposed task held on pilot warnings is approved as before; once it is in the backlog, admission does not hold it on those warnings.
- Cost: a skipped task can dispatch on requirements no pilot has read, including ones an earlier assessment flagged as duplicate or landed. The `pilot_skipped` entry is the only record of what was dispatched unassessed, so audits read history, not the receipt.

## `no-pilot` leaves a current host-operational hold

**Recorded:** 2026-11 · [ORB-21767], dashboard wording aligned by [ORB-24879]

### Context

[ORB-21238](#no-pilot-is-an-operator-skip-of-pilot-admission) treated every receipt hold, including host operational, as something `no-pilot` skipped. Admission was later narrowed: a current host-operational hold is checked before the general skip, and `no-pilot` is removed from that hold's fingerprint, so adding the tag leaves the task held. The Readiness card still offered Skip pilot for that reason and, after the tag landed, said dispatch proceeds.

### Decision

Preserve that admission protection. Offer Skip pilot only for a hold the tag releases. For `host_operational_handoff`, the Readiness card explains that `no-pilot` does not release the hold and offers the remedies admission already accepts: re-scope the acceptance criteria, or record `task-pilot-admission: evaluated`, `clear` or `approve-anyway` with evidence (Clear hold and Approve anyway when the hold is resolvable). No success notice says dispatch proceeds while the hold remains.

### Consequences

- A task tagged `no-pilot` can still be held on a current host-operational finding. Removing the tag does not clear that hold either.
- Cost: an operator who expected the old Skip pilot button to dispatch this hold must use re-scope or an operator decision instead. The button for stale, operator-validation, native-OS, duplicate and already-landed holds is unchanged.

## `readiness-waived` is an operator waiver of readiness gaps

**Recorded:** 2026-11 · [ORB-25084]

### Context

A backlog task with no `context_files` waits for the task pilot and then runs only alone (`awaiting_footprint`, `awaiting_exclusive_slot`), and one with unassessed complexity is withheld. The only fixes were editing the task, a pilot run, or the `no-diff-expected` tag, which misstates a task that does edit files. The dashboard also put the footprint wait under "Waiting on locks · Held by: Lock holder unavailable", although no task holds a lock.

### Decision

An operator adds the `readiness-waived` tag, from Readiness (**Waive readiness**) or `orbit task update --tag`, to waive every backlog readiness gap: missing context files and unassessed complexity. The waivable set is what `readiness_gaps` reports at the backlog stage; the readout lists it as `readiness.waivable`, and a waived gap stays listed as advisory with `waived: true`. The proposed stage is not waivable: approval still needs a prepared task.

- File locks: a waived task with no context files holds no file lock and reserves nothing. It does not wait for a pilot or an exclusive slot, does not reserve the tree, and does not hold the tree while it runs. In its wave it counts as editing work: it respects a task that holds or reserves the tree, and an exclusive waiter behind it waits for it. Other work may run beside it, and may conflict with it; the operator accepts that.
- Complexity: the task is admitted with `unassessed` complexity, and its crew resolves from the configured crew or the workspace default.
- Not waivable: host OS, `requires_labels`, the browser kit for `ui` work and a human-crew assignment, the set [ORB-25082] keeps for explicit ship; also dependencies, real lock holders, pilot holds, schedules and `no-drain`. None of them is a readiness gap, so the tag never reaches them.
- Authority: only an operator adds the tag; an agent's write is refused whole and filing a task with it is refused. Removing it is open to every writer. History records `readiness_waived` (actor and the gaps waived) and `readiness_waiver_removed` (actor and the gaps that apply again).
- The dashboard names the empty-context waits as `awaiting footprint` and `awaiting exclusive slot` in "Waiting, other", not as lock waits.

### Consequences

- A tag carries the waiver, as `no-pilot` carries the pilot skip, so no task field or store migration is needed and every admission path (drain, discovery ship, readiness, forecast) reads the same predicate, `footprint_rule_applies`.
- Cost: a waived task with no context files can edit files another running task edits; the drain no longer protects against that conflict.

## Task References

- [ORB-21238] — made the `no-pilot` tag an operator skip of stale pilot holds, with a Readiness "Skip pilot" button.
- [ORB-21767] — kept a current host-operational hold when `no-pilot` is added.
- [ORB-24879] — stopped the dashboard from offering Skip pilot for that hold, and from saying dispatch proceeds while it remains.
- [ORB-25082] — explicit ship overriding drain readiness gates while keeping host-capability and human-crew refusals.
- [ORB-25084] — added the operator's `readiness-waived` waiver of backlog readiness gaps.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
