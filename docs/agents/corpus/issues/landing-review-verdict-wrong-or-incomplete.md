---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-16359, ORB-18103, ORB-18135, ORB-18547, ORB-19818]
---

# Landing review verdict wrong or incomplete

## Signature

The reviewer returns `accept_with_fixes` with zero changes, rejects on one criterion, or the settlement gate downgrades an accept to `incomplete` (`validation_contradicted`; counterfactual mutation targets compared against the implementation commit only).

## Mechanism

The review gate compares evidence against the wrong baseline, cannot run Linux-only tests on a Mac reviewer, or misses a guard test. Some final-recovery diagnoses were refuted at triage (ORB-18135, F2026-10-586: an empty `required_validation_commands` does not block landing).

## Resolution

Verify the claim against the PR before acting. Operator resolutions seen: run the missing tests on the right host and requeue onto the kept candidate (ORB-18547, twice), requeue onto the candidate after an independent review (ORB-19818), discard and implement fresh when the PR is red (ORB-18135), decide a criterion (ORB-18103).

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-17 05:03 | ORB-16359 | jrun-20261017-0342-c1 | settlement gate validation_contradicted on reviewer repair | claude moved to review at 2026-10-17T05:31Z |
| 2026-10-17 05:05 | ORB-16359 | jrun-20261017-0503-t3 | review accept_with_fixes downgraded to incomplete | claude moved to review at 2026-10-17T05:31Z |
| 2026-10-25 05:37 | ORB-18103 | jrun-20261025-0454-c4 | review rejected on F3, criterion 6 QA used a stub | human forced blocked->done at 2026-10-25T05:51Z |
| 2026-10-25 08:24 | ORB-18135 | jrun-20261025-0815-c3 | review missed a red guard test; gate diagnosis refuted | operator requeued at 2026-10-25T08:33Z |
| 2026-10-26 15:07 | ORB-18547 | jrun-20261026-1425-c1 | review incomplete on Linux-only sandbox tests | operator requeued at 2026-10-26T15:10Z |
| 2026-10-29 16:35 | ORB-19818 | jrun-20261029-1617-c1 | accept_with_fixes with zero changes | operator requeued at 2026-10-29T16:57Z |
