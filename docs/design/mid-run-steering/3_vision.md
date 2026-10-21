---
title: Mid-Run Operator Steering — Vision
owner: codex
last_updated: 2026-10-20
last_validated: 2026-10-20
status: Accepted
feature: mid-run-steering
doc_role: vision
type: design
summary: Forward directions for mid-run steering — native session injection, urgency classes, and operator-visible delivery state
tags: [mid-run-steering]
paths: ["crates/orbit-core/src/adapter/engine_host/actions/task_context.rs", "crates/orbit-engine/**"]
related_features: [activity-job, executors, user-interface]
related_artifacts: [ORB-16577, ORB-10499]
---

# Mid-Run Operator Steering — Vision

Forward-looking: where steering could go now that the safe-point version in [2_design.md](./2_design.md) is implemented. Operational history still needs to accumulate. Everything here is speculation until a task adopts it; nothing in this doc is a commitment or a schedule. Native session injection, interrupt urgency, UI delivery states and automation-authored steering remain out of scope and unfiled unless an operator requests them. Accepted status records that boundary, not implementation of these ideas.

## 1. Open Questions

1. **Interrupt or wait?** Which steering justifies cancelling a running step (paying subprocess restart and lost progress) rather than waiting for the next safe point — and who may mark a comment that urgent, under what budget?
2. **Native session injection.** Several provider CLIs expose session or resume protocols (the Muse lane's MSP session host in [ORB-16577] is one). Should the executor contract grow an optional `steer` capability flag, negotiated per lane like native inspection, with the generic safe-point path as the floor every lane meets?
3. **Delivered vs understood in the UI.** What should the operator see after commenting on a live task: "queued for next step", "delivered to run X step Y", "candidate promoted with this unread"? Each is a different promise, and only the first two are facts the host actually holds.
4. **Steering that changes the task.** When a comment effectively amends acceptance criteria, should the review gate re-derive its criteria from the amended record, or keep judging the candidate against the criteria captured at admission? The absorption certificate in `sync_base` recovery already pins a `task_scope_digest` at admission time; steering may need the same pin-and-name-the-delta treatment.
5. **Non-human steering.** Current routine and auto-task code paths do not write task comments. Should an automation ever be able to steer a live run, or is steering a human-only channel by construction?
6. **Measuring the miss rate.** Before this feature, a missed comment left no trace. What metric — undelivered-at-promotion holds, rounds consumed, time-to-delivery — would prove the mechanism earns its prompt and latency cost per workspace?

## 2. Prior Work

### Orbit-internal precedents

- [ORB-10499]'s terminal precheck and checkpoint re-read: the same shape as steering's advisory pull path, proven for status. Steering extends the re-read to comments and adds the deterministic gate status never needed (a terminal write rejection already stops the agent).
- Recovery attempt budgets ([activity-job 2_design.md](../activity-job/2_design.md)): budgeted, settle-on-admission extra attempts are the house pattern the steering rounds copy, including "a component it cannot observe never counts as a change".
- The absorb-candidate certificate's `task_scope_digest`: pinning what the task said at a moment in time so later stages can name drift instead of silently absorbing it. Open question 4 is this problem wearing a steering hat.

### Adjacent industry patterns

Interactive coding agents have always had steering — a human typing into a live session. The gap is specific to *headless, dispatched* agents working from a frozen brief, where the operator's later knowledge has no channel. Orbit's wrinkle is that its runs are long, cross-machine and unattended, so the channel has to be a durable record with a gate, not a keystroke.

## 3. What May Be Distinctive

Steering through the system of record rather than a live socket: the comment, the watermark, the delivery event and the gate verdict are all durable artifacts a later reviewer (or a different host settling a claimed leaf) can re-read and re-verify. Most agent harnesses treat mid-run guidance as ephemeral session state that dies with the process; here the safe-point records survive crashes, resumes and host changes like every other Orbit decision input.

## 4. References

**Orbit-internal**

- [activity-job 2_design.md](../activity-job/2_design.md)
- [review-gate 1_overview.md](../review-gate/1_overview.md)
- [executor-onboarding.md](../../runbooks/executor-onboarding.md)

**External**

- None cited. No external source is load-bearing for this design; the constraints come from Orbit's own executor contract and task record.

## Task References

- [ORB-16577] — added the Muse executor lane and supplied the missed-comment incident; its out-of-scope MSP session host is the example in open question 2.
- [ORB-10499] — established the checkpoint re-read contract this vision extends.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
