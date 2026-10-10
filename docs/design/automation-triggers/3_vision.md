---
title: Automation Triggers — Vision
owner: codex
last_updated: 2026-10-09
last_validated: 2026-10-09
status: Draft
feature: automation-triggers
doc_role: vision
type: design
summary: Alternatives, costs and open decisions for bounded shared trigger evaluation over the existing sweep machinery.
tags: [automation-triggers, scheduling, architecture]
paths: ["crates/orbit-core/src/application/routines/**", "crates/orbit-core/src/application/auto_tasks/**"]
related_features: [routines, auto-tasks, review-gate]
related_artifacts: [ORB-11315, ORB-11314, ORB-11316]
---

# Automation Triggers — Vision

This document records the broader direction first formulated for [ORB-11315].
Delivery triggers ([ORB-11330]) and bounded state preparation ([ORB-11331]) are
implemented. The `execution_failed` state trigger still parses, but its
`task_triage_pipeline` target was retired. The remaining proposals and
alternatives below are not a description of currently shipped behavior; the
current contract is in [Design](./2_design.md) and [Operations](./5_operations.md).
Astra owns unresolved design formulation; workers may implement approved,
bounded semantics but must escalate product/architecture choices that materially
change the contract.

## 1. Open Questions and Settled V1 Contracts

1. **Delivery evidence coverage.** V1 uses provider-verified PR identities and
   authorized before/after direct receipts. Unprovable associations remain
   explicit debt; history replay requires exact patch mapping and renewed provider
   proof. These contracts are implemented in [Design](./2_design.md#3-delivery-identity-and-captured-coverage)
   and [Operations](./5_operations.md). Future provider adapters must meet the
   same evidence bar; arbitrary first-parent commits do not count as deliveries.
2. **Operational bounds.** The evaluator's scan and admission bounds and recovery
   retention behavior are documented in [Operations](./5_operations.md).
   Thresholds, maximum waits and review/QA capacity remain operator tuning values
   to check against real delivery rates. A busy repository may need more capacity
   rather than larger invisible backlogs; these values are not permission to
   enable all automation.
3. **Coverage acceptance.** V1 requires schema-v1 structured evidence bound to
   the frozen batch and assigned executor run; Core validates it and Store retains
   an immutable receipt. Process success and unstructured summary prose are
   insufficient. The evidence contract is in [Operations](./5_operations.md#evidence-submission);
   new coverage classes need their own contract. Review meaning stays with the
   [review gate](../review-gate/2_design.md).
4. **Pilot freshness.** Shipped preparation consumers fingerprint configured
   material task fields and optional source selectors or revisions. Full-revision
   invalidation is a selectable source-sensitivity policy, not the universal
   rule. Review `freshness.material_fields` and `source_sensitivity` when tuning
   source churn; the current options are in [Operations](./5_operations.md#state-preparation-orb-11331).
5. **Debt disposition.** Delivery auto-task consumers have audited settings
   adoption, action reissue, history replay, explicit waiver and reset operations.
   These keep waived, failed and exhausted work distinct from accepted coverage;
   state-member consumers still use the restore-definition path. See
   [Operations](./5_operations.md#inspection-and-recovery). Automatic expiry would
   weaken the no-lost-coverage contract.
6. **State layout and recovery proof.** The v1 implementation uses Store consumer
   checkpoints, keyed action admission, generation fences and transactional
   checkpoint/receipt writes over the existing host database. Its ownership and
   crash boundaries are documented in [Design](./2_design.md#5-crash-safety-concurrency-and-minimal-state);
   no new database or scheduler loop is needed.
7. **Interaction review.** The before-PR review gate now produces verified
   certificates that can exclude matching review obligations after landing while
   retaining them as context; QA remains independent. The open question is whether
   integrated architectural review needs a separate examination class even when
   every patch was reviewed. Resolve that through a scoped follow-up, not an
   undocumented threshold exception.

### Alternatives and costs

| Alternative | Assessment |
| --- | --- |
| Faster cron plus model inspection | Smallest operational change, but repeated empty runs and prose cursors leave coverage/incident correctness to agents. Retain for genuinely periodic work; not the shared state contract. |
| Count raw `done` transitions or task markers | Cheap, but epic/bundle/no-diff completions inflate counts and open PRs can look delivered. Reject as the delivery metric. |
| Dispatch-time cursor as success | Minimal state, but failed sweeps permanently skip content. Reject; immutable batches and receipts cost storage and recovery complexity. |
| Independent event logic in each scheduler | Avoids a shared module initially, but duplicates identity, retry and coverage invariants. Use one orbit-automation evaluator with Core task/job action adapters [ORB-11330]. |
| Resident event bus, webhook handlers, arbitrary expressions | Lower latency and broader extensibility, with another service, authorization surface and replay model. Defer: existing clock plus bounded reconciliation can establish correctness first. |
| Distributed leases across hosts | Enables automatic failover, but requires authority transfer and side-effect fencing beyond current pins. V1 uses one authoritative state-consumer host. |
| Hash only task `updated_at` / changed paths | Cheap freshness checks, but summary writes cause loops and indirect source/contract changes can be missed. Use material task data and the explicit `source_sensitivity` policy to make the freshness/cost tradeoff visible. |

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
- The [review gate](../review-gate/2_design.md), from [ORB-11333], defines the
  review semantics that consume trigger evidence. Operation mode, which also
  proposed cadence and scoped authority, was removed on 2026-09-21
  ([orbit-core decisions](../orbit-core/4_decisions.md)).
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
- [Review gate](../review-gate/1_overview.md).

External: none required; provider-specific delivery evidence must be verified
against its actual adapter contract during implementation.

## Task References

- [ORB-11315] — formulates shared triggers and leaves these decisions for review.
- [ORB-11314] — proposes mode defaults and scoped automation.
- [ORB-11316] — proposes review timing, repair limits and coverage meaning.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
