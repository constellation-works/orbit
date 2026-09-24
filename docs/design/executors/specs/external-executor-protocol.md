---
title: External Executor Protocol v1 (Retired)
owner: claude
last_updated: 2026-09-24
last_validated: 2026-09-19
status: Retired
feature: executors
type: design
summary: "RETIRED: External Executor Protocol v1 was never a supported surface; the executor_type external transport was deleted in ORB-10395. Kept only as a retirement stub for operators who find an old external def."
tags: [executors, extensibility, protocol, retired]
paths:
  - "crates/orbit-types/src/workflow/executor_def.rs"
related_features: [executors]
related_artifacts: [ORB-00384, ORB-10395]
---

# Spec: External Executor Protocol v1 — RETIRED

> **Status: Retired ([ORB-10395], 2026-07-26).** Not a supported Orbit surface.
> **Do not implement against this document.**

## What remains

- `ExecutorType::External` (`executor_type: external`) still deserializes so
  pre-existing executor defs load. Nothing spawns them: the def is inert.
- `ExternalExecutor`, the shared `direct_agent` subprocess transport, the v1
  executor registry, the `external.example.yaml` template, and the conformance
  fixture were deleted with the rest of the v1 executor stack when v2 dispatch
  (`orbit-engine::activity_job`) became the only execution path.
- Dropping the wire variant is a separate release decision.
- A future out-of-process extension point would be a new, separately decided
  contract on the v2 dispatch path.

The original rationale and the retirement note live in
[4_decisions.md — External Executor Protocol for dynamic out-of-process executor registration (retired)](../4_decisions.md#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired).
The v1 wire contract (stdin JSON envelope, exit-code mapping) is in git history.

## Task References

- [ORB-00384] — defined External Executor Protocol v1 and added `ExecutorType::External`.
- [ORB-10395] — retired the protocol and deleted its transport, registry, template, and fixture.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
