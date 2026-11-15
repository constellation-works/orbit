---
type: issue
applies_to: [agent_implement]
status: active
links: [ORB-20625]
---

# Implementer declares regression evidence incomplete

## Signature

`implement_one` exits 0 with envelope `status: failed`, `error.code=regression_evidence_incomplete`: `The delivery test mocks the stopped state rather than exercising the new stopped proof, and the core test directly exercises a private helper contrary to the workspace guide. A compliant public-boundary regression is still required.`

## Mechanism

The first candidate's tests did not satisfy the workspace test rule: one passed without the fix (a fake host with the state preset), the other tested a private function. The implementer correctly refused to hand off non-compliant evidence. The filing described the cause as no public-boundary harness being found; the history records mocked and private-helper tests, and a harness modelled on `blocked_task_triage.rs` existed.

## Resolution

Replace the evidence rather than the fix: delete the private-helper test and its baseline entry, and write one public-boundary regression through the real runtime that fails before the fix. Daniel's direction was `Requeue with hint` then `Discard and requeue`; the fresh run delivered.

## Fix task

No fix task: the rule is already in the repository guide.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01 00:08 | ORB-20625 | jrun-20261031-2344-c9 | regression_evidence_incomplete: mocked delivery test, private-helper core test | PR #7375 failure handoff; Daniel discarded and requeued at 00:36Z; run jrun-20261101-0037-c3 landed PR #7390 at 01:03Z |
