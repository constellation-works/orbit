---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18096, ORB-18124, ORB-18542, ORB-18816]
---

# Final recovery returns no decision while required checks are unconcluded

## Signature

Escalation text: `final recovery activity final_recovery did not return a decision: completion recovery required checks have not all concluded; no complete failure diagnosis is available`.

## Mechanism

The recovery agent ran while the PR's required checks were still pending, so it could not diagnose; the engine escalates instead of waiting. Triage found genuine content conflicts behind some of them (ORB-18096) and an Orbit defect (F2026-10-653, ORB-18816).

## Resolution

Do not treat the escalation as a diagnosis. Check whether checks have since concluded and rerun recovery; if a conflict is real, hand it to an operator with the exact refusal.

## Fix task

No open fix task found; ORB-19030 (pending required checks escalate instead of retrying) is landed and by search related, not verified.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 05:54 | ORB-18096 | jrun-20261025-0448-c8 | no decision, checks unconcluded | human forced blocked->done at 2026-10-25T06:05Z |
| 2026-10-25 08:48 | ORB-18124 | jrun-20261025-0757-c31 | no decision, checks unconcluded | human:daniel moved to backlog at 2026-10-25T08:49Z |
| 2026-10-26 08:29 | ORB-18542 | jrun-20261026-0716-c9 | no decision, checks unconcluded | merged outside Orbit at 2026-10-26T08:54Z, settled done |
| 2026-10-27 02:47 | ORB-18816 | jrun-20261027-0224-c3 | no decision, checks unconcluded | no clearing event in history; task now done |
