---
title: Mid-Run Operator Steering — Decisions
owner: codex
last_updated: 2026-10-18
last_validated: 2026-10-18
status: Accepted
feature: mid-run-steering
doc_role: decisions
type: design
summary: Decisions for mid-run steering — steer through the durable task record and gate on delivery, never on agent acknowledgement
tags: [mid-run-steering]
paths: ["crates/orbit-core/src/adapter/engine_host/actions/task_context.rs", "crates/orbit-engine/**"]
related_features: [activity-job, review-gate]
related_artifacts: [ORB-16577, ORB-10499, ORB-10382]
---

# Mid-Run Operator Steering — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line. The decision below governs the implemented safe-point mechanism and any future extensions; the unimplemented directions remain in [3_vision.md](./3_vision.md).

## Steer running work through the durable task record, never a side channel

**Recorded:** 2026-10 · [ORB-16577]

### Context

A dispatched agent works for tens of minutes from a prompt snapshot; the operator's knowledge keeps moving. Three channels could carry later guidance: a live side channel into the provider process (per-CLI session protocols, sockets, stdin conventions — none of them portable across the multi-lane executor surface), a new steering store beside the task, or the comment log the task record already has. The [ORB-16577] incident showed the failure is real and silent: every step green, artifact wrong, correction sitting unread in the record the whole time.

### Decision

Any mechanism that moves operator intent into a live run reads and writes the durable task record: steering input is ordinary task comments, delivery is a host-recorded watermark and history event, and enforcement is a deterministic gate over that record. No step, gate or recovery decision may consume an agent's acknowledgement that it read or applied guidance — response envelopes remain advisory diagnostics only, as they already are for recovery activities after [ORB-10382]. Provider-native injection, if ever added, is an optimisation *under* this record: it may deliver sooner, but the comment, watermark and gate stay authoritative.

### Consequences

- A steering miss becomes observable and enforceable by any host that can read the task, including an owner settling a claimed leaf it never ran.
- The generic seam stays executor-agnostic: no lane needs a session protocol to be steerable, and a new lane inherits steering by meeting the existing prompt contract.
- What is ruled out: steering stores with their own lifecycle, "the agent said it applied the comment" as promotion evidence, and per-provider steering behaviour that an operator must learn lane by lane.
- Cost: delivery latency is bounded below by the step structure — guidance can be minutes stale inside a long single step, where a live socket would be instant. The design accepts stale-but-provably-delivered over instant-but-unrecorded, and pays for it whenever a long step does expensive work the operator had already countermanded.

## Task References

- [ORB-16577] — added the Muse executor lane; the operator comment its implement run missed is the context above.
- [ORB-10499] — established re-reading the task record at checkpoints as the way a live run learns the record moved.
- [ORB-10382] — made a recovery activity's structured result advisory only, the precedent for refusing envelope-carried acknowledgement.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
