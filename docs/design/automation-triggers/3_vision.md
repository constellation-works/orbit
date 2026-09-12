---
title: Automation Triggers — Vision
owner: codex
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Draft
feature: automation-triggers
doc_role: vision
type: design
summary: Alternatives, costs and open decisions for bounded shared trigger evaluation over the existing sweep machinery.
tags: [automation-triggers, scheduling, architecture]
paths: ["crates/orbit-automation/src/routines/**", "crates/orbit-automation/src/auto_tasks/**"]
related_features: [routines, auto-tasks, operation-mode]
related_artifacts: [ORB-11315, ORB-11314, ORB-11316]
---

# Automation Triggers — Vision

This is a proposed direction for [ORB-11315], not shipped behavior. The concrete
candidate contract is in [Design](./2_design.md). Astra owns unresolved design
formulation; workers may implement approved, bounded semantics but must escalate
product/architecture choices that materially change the contract.

## 1. Open Questions

1. **Delivery evidence coverage.** Approve the PR/explicit direct-receipt unit and
   policy for repositories whose provider cannot map rebased commits. The candidate
   leaves uncertain units uncounted and visible; counting arbitrary first-parent
   commits would be a different product metric. Implementation must prove how
   receipts are retained/recovered across external landings.
2. **Operational bounds.** Confirm example thresholds, maximum pending age, scan
   budgets and retention windows against real delivery rates. These are proposed
   tuning values, not permission to enable all automation. A busy repository may
   need more capacity rather than larger invisible backlogs.
3. **Coverage acceptance.** Agree the minimum QA/review evidence schema and who
   can accept it. The candidate requires an authorized deterministic validator;
   process success or unstructured summary prose is insufficient. Review meaning
   stays with [operation-mode's review proposal](../operation-mode/3_vision.md).
4. **Pilot base churn.** Begin with conservative full-revision invalidation or
   invest in a proven dependency/path fingerprint? The candidate chooses the
   conservative rule first, accepting more pilot work and possible escalation.
   Do not silently accept stale readiness just to improve throughput.
5. **Debt disposition.** Approve the explicit retry/replacement/waiver interface
   for rejected actions, definition replacement and history divergence. Waived
   debt must remain distinguishable from covered work. Automatic expiry would
   weaken the no-lost-coverage contract.
6. **State layout and recovery proof.** Final table/contract names and task-bundle
   creation-key representation belong in the first implementation design review.
   Use the current store owners and prove every crash boundary; no cross-crate
   dependency or parallel scheduler should be needed.
7. **Interaction review.** The mode proposal excludes proven before-PR patch
   coverage while permitting context reads. If integrated architectural review
   needs distinct obligations even when every patch was reviewed, add an explicit
   examination class through a scoped follow-up, not an undocumented exception
   to threshold counts. QA already remains independent.

### Alternatives and costs

| Alternative | Assessment |
| --- | --- |
| Faster cron plus model inspection | Smallest operational change, but repeated empty runs and prose cursors leave coverage/incident correctness to agents. Retain for genuinely periodic work; not the shared state contract. |
| Count raw `done` transitions or task markers | Cheap, but epic/bundle/no-diff completions inflate counts and open PRs can look delivered. Reject as the delivery metric. |
| Dispatch-time cursor as success | Minimal state, but failed sweeps permanently skip content. Reject; immutable batches and receipts cost storage and recovery complexity. |
| Independent event logic in each scheduler | Avoids a shared module initially, but duplicates identity, retry and coverage invariants. Use one orbit-automation evaluator with Core task/job action adapters [ORB-11330]. |
| Resident event bus, webhook handlers, arbitrary expressions | Lower latency and broader extensibility, with another service, authorization surface and replay model. Defer: existing clock plus bounded reconciliation can establish correctness first. |
| Distributed leases across hosts | Enables automatic failover, but requires authority transfer and side-effect fencing beyond current pins. V1 uses one authoritative state-consumer host. |
| Hash only task `updated_at` / changed paths | Cheap freshness checks, but summary writes cause loops and indirect source/contract changes can be missed. Use material task data and conservative source revision first. |

The cost of the preferred design is explicit pending state, source-object
retention, creation-key recovery, and honest stalled consumers when evidence is
missing. Avoid claiming exactly-once arbitrary model effects; only action
admission and receipt application get idempotency guarantees.

## 2. Prior Work

### Within Orbit

- [Routines](../routines/2_design.md) already own clock-driven job dispatch and
  fire history; [auto-tasks](../auto-tasks/2_design.md) own recurring task minting.
  The source inventory in [Design section 1](./2_design.md#1-current-implementation-and-gaps)
  distinguishes those actual contracts from proposed extensions.
- [Operation-mode design](../operation-mode/2_design.md) and
  [proposal](../operation-mode/3_vision.md), from [ORB-11314] and [ORB-11316],
  define cadence/authority and review semantics that can consume trigger evidence.
- Existing pilot partitions, triage disposition application, run child linkage,
  store claims and repository delivery checks supply concrete ownership seams.

### External vocabulary

Time slots, reconciliation, immutable inputs, idempotency keys and outbox-style
recovery name ordinary mechanisms; this proposal does not import another workflow
engine or rely on an external service's delivery guarantees. No external product
contract is required to understand or implement the proposed Orbit boundaries.

## 3. What May Be Distinctive

Triggers operate over accountable work: they can explain which deliveries need
examination, which task meaning needs preparation, and which execution episode
needs diagnosis. Due work remains visible when authority or capacity defers it.
The action remains an ordinary Orbit job or task, with the same delivery and
human decision boundaries.

Optional immediate wakeups can later send only an affected workspace/source hint
to the existing evaluator. They must never dispatch independently, advance
coverage, or become the sole source of correctness. Losing a hint merely delays
work until the next reconciliation lap. This keeps the OS sweep as the reliable
fallback without requiring a resident Orbit process in the first version.

## 4. References

Orbit-internal:

- [Proposed contract and rollout](./2_design.md).
- [Architecture and ownership](../../../ARCHITECTURE.md).
- [Routines vision](../routines/3_vision.md) and [auto-tasks vision](../auto-tasks/3_vision.md).
- [Operation-mode proposal](../operation-mode/3_vision.md).

External: none required; provider-specific delivery evidence must be verified
against its actual adapter contract during implementation.

## Task References

- [ORB-11315] — formulates shared triggers and leaves these decisions for review.
- [ORB-11314] — proposes mode defaults and scoped automation.
- [ORB-11316] — proposes review timing, repair limits and coverage meaning.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
