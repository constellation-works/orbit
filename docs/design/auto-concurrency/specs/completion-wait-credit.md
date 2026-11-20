---
type: design
summary: "Proposal (Draft, nothing built): an owner-drain delivery that is only waiting on CI and the merge stops counting against the drain's ceiling, up to a separate cap"
last_validated: 2026-11-19
title: Spec — Completion-wait credit
owner: claude
status: Draft
feature: auto-concurrency
tags: [auto-concurrency, spec, drain, admission]
related_features: [auto-concurrency, distributed-drain, review-gate]
related_artifacts: [ORB-25844, ORB-15194, ORB-15757]
last_updated: 2026-11-19
---

# Spec: Completion-wait credit

**This is a proposal. Nothing here is built, and it changes what the operator's ceiling
means (§6), so it waits for Daniel.** [2_design.md](../2_design.md) stays the description of
what runs today.

The contract it proposes: on an owner drain, `max_active_leaf_runs` limits the deliveries that
are doing work on the host. A delivery that has finished its work and is waiting for hosted CI
and the merge does not take a slot, up to a second, separate cap.

## Why This Exists

Daniel (2026-11-19): "we are currently counting "review" + "in-progress" tasks for
concurrency. but only in-progress should count towards concurrency."

The owner drain does not count task statuses. It counts live leaf runs
([2_design.md §1](../2_design.md#1-today)), and one `task_pr_pipeline` run carries a task from
implementation through the before-landing review, the wait for hosted CI and the merge. The
task's status moves to `review` at `promote_tasks`, part-way through that run, so a task in
`review` still holds its slot.

**Measured** on the owner drain of one workspace, 2026-11-06 to 2026-11-20: 2,927
`task_pr_pipeline` runs. The phase split is from the 2,588 successful runs (723 slot-hours);
load is from `run_resource_minutes`, each minute assigned to the step that covers it.

| Phase (step) | Share of slot time | Mean CPU | 90th pct CPU | Mean memory |
|---|---|---|---|---|
| Implementing (`implement_bundle`) | 67% | 0.96 cores | 2.56 | 1.4 GiB |
| Reviewer (`landing_review`) | 9% | 0.87 cores | 2.01 | 1.9 GiB |
| CI wait and merge (`complete_pr`) | 22% | 0.07 cores | 0.04 | 0.4 GiB |

The remainder is short steps. The mix depends on whether the before-landing review ran:

| Runs | Implementing | Reviewer | CI wait and merge | CI wait, 90th pct |
|---|---|---|---|---|
| With review (849) | 61% | 24% | 12% | 7.9 min |
| Without review (1,739) | 71% | 0% | 28% | 10.2 min |

Three things follow.

1. **The CI wait is idle.** It uses 7% of an implementing leaf's CPU and holds a full slot.
2. **The reviewer is not idle.** It runs an agent and its baseline commands at about the load
   of an implementer, with more memory.
3. **The wait grows when the drain is full.** With 10 to 14 runs live, 1.6 of them were
   waiting on CI on average; with 15 to 19 live, 3.1; with 20 or more, 6.6.

**Not measured.** The other 339 runs (failed, held, cancelled, interrupted) used 183
slot-hours, a fifth of the total, and record one job-level step row with no phases. 75 of
them ended held on the PR or its checks (`delivery_completion_budget_expired`,
`delivery_check_runs_absent`, `delivery_base_checks_pending`), so the idle share is probably
higher than 22%, by an unknown amount.

**Precedent.** Pull drains already do a version of this. Since 2026-10
(`pull_admission_load`, `crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs`), a
claimed leaf in its before-landing review earns its drain a replacement admission, capped at
the drain's own ceiling; [ORB-15194] added it after reviews with 1.5 to 2 hour tails cut a
follower's effective concurrency. The same code says: "Owner/local drains continue counting
every in-flight child against their ceiling."

## 1. Working and Waiting

A live owner-drain leaf is in one of two states for admission.

- **Waiting.** Its `complete_pr` step is running, and no repair or re-review that step started
  is live.
- **Working.** Everything else: implementation, validation, publishing, the before-landing
  review, and any conflict recovery, auto-fix repair or re-review round.

The state is read from the leaf's durable pipeline state, as `in_landing_review` is today. A
state that does not decode reads as working, so it never earns a credit.

## 2. Admission

Admission load is `occupied − min(waiting, max_waiting_leaf_runs)`, and a pass has free slots
while that load is below `max_active_leaf_runs`.

- `max_waiting_leaf_runs` is a per-drain setting, changed live the same ways as the ceiling. It
  defaults to the ceiling's value, which is the bound [ORB-15194] chose for pull drains.
- With an unchanged ceiling a drain therefore carries at most `max_active_leaf_runs` working
  leaves by admission and at most that many again waiting.
- Explicit `orbit run ship` deliveries are counted the same way, as they are in occupancy
  today.
- The resource gate, the forecast and conflict selection run after this, unchanged. Profiles
  are indexed by minutes since admission and already include the idle tail.

## 3. Leaving the Wait

A waiting leaf whose PR needs a repair becomes working again without asking for a slot.
Nothing is paused, cancelled or queued for it.

- Working leaves can then exceed the ceiling, by at most the number of waiting leaves that
  start a repair at once. No new task is admitted until the load is back under the ceiling.
- This is the rule the ceiling already follows when an operator lowers it: running work is
  never cancelled.

## 4. What Does Not Change

- **Locks.** A waiting leaf keeps its footprint until its run ends, so a task that touches the
  same files still waits. The credit frees a slot, not a path.
- **Pull drains.** A claimed leaf hands off to the owner and has no `complete_pr` wait. Its
  existing review credit stands as it is.
- **The resource throttle** stays the backstop.
- **Held and finished runs** already release their slot.

## 5. Surfaces

- Drain occupancy reports `waiting` beside `occupied`, as it reports `reviewing`.
- `orbit run show <drain>`, the readiness payload and the dashboard show working and waiting
  leaves against the ceiling and the waiting cap, and say which of the two binds.
- The occupancy diagnostic (`admission/leaf_occupancy.rs`) gains a `completing` phase.

## 6. The Decision This Would Amend

[The operator's ceiling is hard, and admission never cancels work](../4_decisions.md#the-operators-ceiling-is-hard-and-admission-never-cancels-work)
says `max_active_leaf_runs` is the hard ceiling on leaves. Under this proposal it is the
ceiling on working leaves, and live leaves may reach the ceiling plus the waiting cap. The
second half of that decision is untouched.

New costs:

- **Disk.** Every waiting leaf keeps its worktree and build output. Live worktrees can reach
  twice the ceiling, and that decision records a host whose disk filled with build output.
- **Memory.** About 0.4 GiB per waiting leaf.
- **More open PRs at once.** Each landing moves the base under the others, which means more
  rebases, re-reviews and hosted CI runs. Admission by resource use does not see this
  ([Admit on forecast resource use, not on a throughput model](../4_decisions.md#admit-on-forecast-resource-use-not-on-a-throughput-model)).
  The waiting cap is the only bound on it.
- **Transient excess.** Repairs can push working leaves over the ceiling (§3).

## 7. Alternatives

- **Count only `in-progress` tasks.** This is the rule as Daniel stated it: a leaf stops
  counting when its task reaches `review`, which adds the reviewer to the credit. It matches
  what pull drains do. With the review off it is the same rule as this proposal. With the
  review on it frees 36% of slot time against 12%, and the difference is the reviewer, which
  works at an implementer's load. At a binding ceiling of N that is about 1.4 N working
  leaves, so the rule is this proposal plus an unstated 40% raise of the ceiling. An operator
  who wants that load can raise the ceiling and see it.
- **Raise the ceiling.** It adds working leaves as well as waiting ones, so it buys the idle
  share only by adding load.
- **Move completion out of the leaf.** A separate owner pass could wait for CI and merge, as a
  claimed leaf's handoff does. It would also free the worker process, and it is a much larger
  change to the delivery pipeline.

## 8. What It Buys, and When

Arithmetic from the shares above, while the ceiling is what binds: freeing the wait raises
admissions by about 13% with the review on and about 39% with it off.

The ceiling binds only part of the time. In the measured window the drain had 15 or more
live runs for 6 of 176 active hours, and 10 or more for 26. The ceilings in force during
those hours were not recovered, so the realised gain is not known.

## 9. Phasing

Each slice is one task, filed only after Daniel accepts this proposal.

- **W1. Report.** The waiting state and the `waiting` count in occupancy and the surfaces
  (§1, §5). No admission change. A week of it shows live how often the ceiling binds with
  leaves waiting.
- **W2. Credit.** `max_waiting_leaf_runs` and the admission rule (§2, §3).

## 10. Limits and Open Questions

**Limits**

- One workspace on one host, two weeks.
- A fifth of slot time has no phase record (see "Not measured").
- The `complete_pr` window includes any repair it ran. A 90th percentile of 0.04 cores says
  repairs are rare inside it, but they are not separated out.
- If conflicts or hosted CI are what limit throughput, more open PRs will make it worse, not
  better. W1 does not measure that; a landed-per-hour comparison after W2 would.

**For Daniel**

1. Credit only the CI wait, or everything after `in-progress` as pull drains do?
2. Is the ceiling's own value the right default for the waiting cap?
3. Should pull drains keep crediting the reviewer, or move to this rule?
4. Should failed and held runs record their steps, so the next measurement covers them?

## Task References

- [ORB-25844] — writes this proposal and holds the measurement method
- [ORB-15194] — gave pull drains a replacement admission beside a leaf in before-landing review
- [ORB-15757] — authored the auto-concurrency design

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
