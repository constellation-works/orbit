---
title: Routines — Vision
owner: claude
last_updated: 2026-09-12
last_validated: 2026-09-12
status: Draft
feature: routines
doc_role: vision
type: design
summary: Open questions and prior art for possible routines evolution beyond the current contract.
tags: [routines, scheduler]
paths: ["crates/orbit-core/src/application/routines/**", "crates/orbit-cmd/src/registry_routines.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-registry/src/**"]
related_features: [routines, auto-tasks, activity-job, host-registry, task-migration]
related_artifacts: [ORB-10001, ORB-10021, ORB-10207, ORB-10270, ORB-10319, ORB-11315, ORB-12236, ORB-12237]
---

# Routines — Vision

Forward-looking questions for the routines feature. The current contract lives in
[2_design.md](./2_design.md); items graduate through an explicit task, implementation,
and validation evidence, not by drifting in.

---

## 1. Open Questions

0. **First-class `activity:` targets.** v1 rejects `activity:<name>` at parse time because
   run dispatch is job-shaped ([Routine targets are catalog references only — no inline command payloads](./4_decisions.md#routine-targets-are-catalog-references-only-no-inline-command-payloads)); the wrapper-job idiom covers current needs. A
   standalone activity run entrypoint (or auto-wrapping) would let routines fire
   activities directly — worth doing only if the wrapper friction proves real.
1. **Single-fire across hosts.** Under the multi-owner model described in
   [2_design.md §2](./2_design.md#2-discovery-and-registration), every owner checkout
   is its own schedule and nothing needs to fire exactly once across hosts — each host's
   automation acts only on its own store. The residual case is a definition with a
   repo-global side effect (one PR per owner instead of one). If that ever bites, the
   additive answer is an `owner:` field on the definition, not a lease protocol; it is
   deliberately not designed now.
2. **State-driven triggers.** The [shared trigger proposal](../automation-triggers/2_design.md)
   from [ORB-11315] defines bounded reconciliation of deliveries, preparation eligibility
   and settled failures over the existing sweep clock. This does not require a resident
   process. Immediate file-watch/webhook wakeups remain optional future optimizations;
   the proposal is unimplemented and does not change the current cron-only contract.
3. **Routine-emitted tasks.** A routine whose job files an Orbit task on findings (nightly
   drift check → task per drift) works today via job semantics; what's open is whether
   routines should get first-class dedup support ("don't file a duplicate of an open task
   from a previous fire") or leave that to job logic.
4. **Sub-minute and jitter.** Minute granularity is a v1 floor. Per-routine jitter matters
   only if many routines land on the same slot and contend; revisit when there are enough
   routines for it to be observable.
5. **Missed-run variants.** `catch_up_once | skip` covers current needs; a count-preserving
   `catch_up_all` (anacron-style) is additive if a routine ever needs per-slot semantics.
6. **Cross-host visibility.** Each host's state is local and each host's
   schedule is independent, so "did the nightly commit fire on the other box?" is a
   question about that box's own automation and requires asking it. The single-host half of this is now built:
   `GET /api/routines` projects this host's routine health (last fire, outcome, duration,
   next due) over the dashboard HTTP API [ORB-10138], so a stopped sweep is visible remotely
   without box ssh. True cross-host *aggregation* (one surface querying every box's store)
   remains open; state *sync* is still explicitly not the answer.

### Graduated

- **Workspace-local ship-sweep convergence ([ORB-10207], [Delegate workspace ship routines through a synchronous wrapper job](./4_decisions.md#delegate-workspace-ship-routines-through-a-synchronous-wrapper-job)).** The default
  `ship_sweep` routine delegates synchronously to the normal shipment pipeline for only
  its source workspace. It is seeded disabled and enabled through the versioned
  definition. The legacy global CLI entrypoint remains during burn-in; removing it is a
  separate compatibility task.

---

## 2. Prior Work

### OS schedulers
- **cron / anacron** — the trigger vocabulary (5-field expressions) and the missed-run
  problem anacron exists to solve; routines adopt the vocabulary and make missed-run policy
  per-definition instead of system-wide.
- **systemd timers / launchd** — v1's actual clock. launchd wake behavior and systemd's
  monotonic startup/post-activation triggers guarantee another sweep without replaying
  every missed clock tick; routine cursors and `missed_run` own cron-gap semantics.

### Workflow engines
- **Temporal / Cadence schedules** — durable schedules attached to durable executions,
  with overlap policies (`skip`, `buffer_one`) that v1's `overlap: forbid` and
  `catch_up_once` consciously echo at much smaller scale. The full replayable-execution
  model is what this feature deliberately does *not* adopt.
- **Kubernetes CronJob** — `concurrencyPolicy`, `startingDeadlineSeconds`, and the
  documented pain of missed-fire semantics; a compact catalog of the edge cases §6 of
  [2_design.md](./2_design.md) must test.

### CI schedulers
- **GitHub Actions `schedule:`** — git-versioned schedule definitions co-located with the
  code they operate on; the definition-review-as-security-boundary posture routines share.

---

## 3. What May Be Distinctive

- **Git-versioned, PR-reviewed schedules over a knowledge-integrated runtime.** Fires are
  ordinary Orbit runs with audit envelopes, linkable to tasks — the scheduler
  and the knowledge system share one substrate.
- **Agent-invoking targets.** A routine can fire an `agent_loop` activity: scheduled agent
  work (nightly triage, periodic research) with the same policy and audit surface as any
  other run — most schedulers fire commands; this one fires accountable agent runs.
- **Definitions-shared / state-local as a stance.** Hosts converge on *what* to schedule
  through git alone and never on *whether it fired*; there is no scheduler network protocol.
  Under the multi-owner model that stance is the whole coordination story: the store an
  owner's automation writes to is the store that owner's prefix names.

---

## 4. References

Orbit-internal:
- [../activity-job/1_overview.md](../activity-job/1_overview.md) — the execution substrate
  routines trigger into.
- [../activity-job/4_decisions.md](../activity-job/4_decisions.md) — [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed), the
  removed-shell posture routines inherit.
- [../executors/4_decisions.md](../executors/4_decisions.md) — [External Executor Protocol for dynamic out-of-process executor registration (retired)](../executors/4_decisions.md#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired), sandbox caveats
  relevant to what scheduled targets may do.

External:
- systemd.timer(5), launchd.plist(5) — monotonic restart and wake semantics.
- Temporal "Schedules" documentation — overlap/catch-up policy vocabulary.
- Kubernetes CronJob documentation — concurrency and missed-fire edge cases.

---

## Task References

- [ORB-12236] — removed `hosts:` pins and `[routines] role`; the current eligibility
  contract is in [2_design.md §2](./2_design.md#2-discovery-and-registration).
- [ORB-12237] — delivered `orbit clock`, the combined tick, scheduler retirement, and
  folded that graduated contract into [2_design.md](./2_design.md); depends on [ORB-12236].
- [ORB-11315] — proposes shared state-driven triggers and durable coverage semantics.

- [ORB-10001] — authored this design-doc folder (proposal; no implementation).
- [ORB-10021] — implemented routines v1.
- [ORB-10207] — graduated workspace-local ship-sweep scheduling from this vision.
- [ORB-10319] — historical boundary separation; current local registry/runtime composition lives in `orbit-cmd` over `orbit-registry`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
