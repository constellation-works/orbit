---
title: Finding Verification — Decisions
owner: claude
last_updated: 2026-10-11
last_validated: 2026-10-18
status: Accepted
feature: finding-verification
doc_role: decisions
type: design
summary: Why finding verification is its own job rather than a task-pilot phase, why the verdict lives in task history, and why verifier crews come from the complexity pools.
tags: [finding-verification, review, automation]
paths: ["crates/orbit-core/src/application/task/finding_verification.rs", "crates/orbit-core/assets/jobs/finding_verification_pipeline.yaml"]
related_features: [automation-triggers, review-gate]
related_artifacts: [ORB-15662]
---

# Finding Verification — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## A separate job, not a task-pilot phase

**Recorded:** 2026-10 · [ORB-15662]
**Code anchors:** `crates/orbit-core/assets/jobs/finding_verification_pipeline.yaml`, `crates/orbit-types/src/workflow/automation/members.rs::StateTriggerKind::FindingUnverified`

### Context

Task-pilot already prepares proposed tasks: it pins a revision, runs one
read-only agent per partition and applies the result. A verification phase
inside it would reuse that machinery. But task-pilot asks a different
question ("is this task ready to implement, and what is its footprint?") with
a single `system` crew. Verification asks "is the claimed defect real?" and
must use a crew independent of the filer, escalating doubtful verdicts to a
stronger tier. Task-pilot's member fingerprint and settlement also belong
to preparation, not to the finding as filed.

### Decision

Verification is `finding_verification_pipeline`, with its own deterministic
prepare and apply actions, its own verifier activity, and its own state
trigger kind, `finding_unverified`. It reuses task-pilot's source snapshot,
input helpers and the shared member host. The gate sits on the shared
admission seam, `pilot_admission_hold_in`, not inside task-pilot. So a
finding is held whether or not task-pilot ever prepares it.

### Consequences

- A finding can be verified, and rejected, before anyone spends a pilot
  assessment on it; task-pilot and verification run independently.
- The verifier step names its crew per partition, so the pool draw and
  escalation are visible in the run and in the record.
- Cost: a second state routine and job to enable and monitor, and a finding
  may be both piloted and verified when both routines are on.

## The verdict is a history entry, mirrored as a comment

**Recorded:** 2026-10 · [ORB-15662]
**Code anchors:** `crates/orbit-core/src/application/task/finding_verification.rs::latest_verification`

### Context

The gate must not trust anything an agent or operator can write by hand. A
comment can be written by anyone, and a separate artifact would be a new
persisted format that `orbit task show` does not display.

### Decision

The trusted record is the `finding_verified` history entry attributed to the
`finding-verification` actor, written in the same task update as its comment
and, for REJECTED, the move to `rejected`. The comment carries the same
record for people and is never read by the gate.

### Consequences

- `orbit task show` displays the verdict with no new surface, and the gate
  reads one history scan.
- Cost: the record's schema lives inside a history note, so a schema change
  needs a version bump (`schema_version`) rather than a store migration.

## Verifier crews come from the complexity pools

**Recorded:** 2026-10 · [ORB-15662]
**Code anchors:** `crates/orbit-core/src/application/task/finding_verification.rs::draw_finding_verifier`

### Context

A dedicated review-crew setting would be one more key operators must keep in
step with the crews they already rank by capability in
`workflow.{low,medium,hard,xhard}_complexity_crews`.

### Decision

The verifier is drawn from the pool of the task's complexity tier, passing
over the filer's crew when the pool offers another. Escalation draws from
the next tier's pool. No new configuration key exists.

### Consequences

- Operators tune verification cost through the same pools that route
  implementation.
- Cost: a single-member pool cannot avoid the filer's crew, and a workspace
  without pools verifies every finding with the default crew.
