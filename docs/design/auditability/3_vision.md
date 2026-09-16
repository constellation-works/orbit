---
summary: "Auditability — Vision"
type: design
title: "Auditability — Vision"
owner: codex
last_updated: 2026-07-18
last_validated: 2026-09-16
status: Draft
feature: auditability
doc_role: vision
tags: ["auditability"]
---

# Auditability — Vision

This document captures the questions that remain before Orbit's auditability story matches its product promise end to end. [2_design.md](./2_design.md) is the current implementation; this file names the pressure points that should drive future tasks and ADRs.

---

## 1. Open Questions

1. **Canonical query surface.** Should `orbit audit` stay command-row-only, or grow a run-centric view that joins command rows, job-run state, v2 envelopes, loop JSONL, blobs, and invocation metrics?
2. **Local tamper evidence.** Should Orbit use per-file hash chains, SQLite append proofs, signed manifests, git-backed checkpoints, or export-time attestations while staying self-hosted by default?
3. **Auditing audit reads.** Should `orbit audit` reads, exports, and prunes remain outside the guard to avoid recursion, or be recorded through a separate path?
4. **Stable identity across channels.** MCP now records trusted caller/process machine identity separately from managed agent identity; what joins those fields to human CLI usage, task attribution, v2 envelopes, metrics, commits, and PR metadata?
5. **Stdout/stderr retention.** The command schema has truncated stdout/stderr fields, but most paths leave them empty. What retention policy should exist before broad capture?
6. **JSONL migration.** [T20260426-0519] moved run traces to `.orbit/state/audit/`; should old `.orbit/audit/` files be migrated, ignored, or read through a legacy fallback?
7. **Replay payload depth.** When are redacted verbatim prompts/responses required, and when are summaries enough?
8. **Uniform denials.** Can filesystem denials, tool allowlist denials, task-lock conflicts, and gate starvation share one audit shape?
9. **Coverage enforcement.** What lint or tests should fail review when a new mutation path lacks audit coverage?
10. **Metrics derivation.** Should invocation metrics stay primary records, or eventually derive from loop traces at the cost of tighter coupling?

---

## 2. Prior Work

### 2.1 Orbit-Internal

Activity / Job's audit-envelope section and ADRs in [../activity-job/2_design.md](../activity-job/2_design.md) and [../activity-job/4_decisions.md](../activity-job/4_decisions.md) document the v2 envelope, backend differences, CLI allowlist delegation, file-backed audit placement, and invocation trace persistence.

The product contract lives in [../../../README.md](../../../README.md) and [../../POSITIONING.md](../../POSITIONING.md): auditability should answer what, why, and who; audit rows should be structured and queryable; provider interactions should be reproducible after redaction; retention should become tamper-evident; and identity should attach to every write.

The redaction and blob-store implementation is the existing write-side safety boundary. Future query tools should preserve it even if they read blobs more directly.

[ORB-10228] closes the MCP caller-JSON ambiguity for command rows: trusted adapter/runtime provenance wins, standalone calls are unverified, capability is a full set, and call/session/lease correlation is additive. Future remote authentication may strengthen caller-machine assurance but must not move authority back into model-authored JSON.

### 2.2 Workflow and Observability Systems

Temporal and Airflow show the value of explicit run, step, retry, and state-transition history. OpenTelemetry shows the value of trace/span identifiers and exportable schemas. Orbit should borrow the join model while keeping accountability and replay obligations distinct from generic telemetry.

### 2.3 Supply-Chain Attestation

SLSA, in-toto, Sigstore, and Rekor point toward signed or transparency-backed provenance. Orbit can learn from those systems, but local/self-hosted operation must remain the default.

### 2.4 Security Event Logging

Security audit systems treat reads, writes, privilege decisions, and retention as one accountability surface. Orbit's domain is narrower, but a policy denial, task update, provider request, and PR operation should all be explainable from the audit trail.

---

## 3. What May Be Distinctive

1. **Code-aware audit joins.** Orbit can join records by run id, task id, context selector, lock reservation, worktree, graph node, and commit attribution.
2. **Agent identity as a first-class actor.** The trail can name humans, model families, provider backends, and task roles without treating agents as generic service accounts.
3. **Transcript plus structure.** V2 envelopes describe the workflow tree while loop blobs preserve redacted provider/tool payloads.
4. **Local tamper evidence.** A self-hosted team-scale runtime can produce verifiable local audit bundles without a hosted control plane.
5. **Coverage as a design gate.** New mutation paths should name their audit event family before they ship.

---

## 4. References

Orbit-internal:

- [1_overview.md](./1_overview.md) — auditability purpose and current concept map.
- [2_design.md](./2_design.md) — shipped implementation and limitations.
- [specs/event-schema.md](./specs/event-schema.md) — prescriptive event-channel contract.
- [specs/coverage-matrix.md](./specs/coverage-matrix.md) — expected audit coverage by operation class.
- [specs/redaction-retention.md](./specs/redaction-retention.md) — write-side redaction and retention boundaries.
- [specs/actor-identity.md](./specs/actor-identity.md) — canonical actor projection and its versioned alias map.
- [../activity-job/specs/audit-envelope.md](../activity-job/specs/audit-envelope.md) — v2 activity/job audit envelope spec.
- [../../../README.md](../../../README.md) — product-level auditability commitments and public status.
- [../../POSITIONING.md](../../POSITIONING.md) — auditability as a non-negotiable.

External reference categories:

- Workflow run history: Temporal, Airflow.
- Structured traces and logs: OpenTelemetry.
- Supply-chain provenance and tamper evidence: SLSA, in-toto, Sigstore, Rekor.
- Security audit logging: append-only security event logs and retention policies.

---

## Task References

- **[T20260419-0002]** — Add workspace provenance and v2 audit envelope events for activity/job execution.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under workspace state.
- **[T20260426-0526]** — Persist v2 invocation traces for metrics beside audit.
- **[T20260426-0605]** — Add this auditability design folder and name future auditability questions.
- **[T20260430-20]** — Shorten the auditability docs while preserving required guarantees.
- **[ORB-00090]** — Aligned auditability identity wording with the family-as-identity convention.
- **[ORB-10228]** — Established trusted MCP caller/process provenance and anti-spoofing.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
