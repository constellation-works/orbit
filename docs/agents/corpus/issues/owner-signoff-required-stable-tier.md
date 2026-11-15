---
type: issue
applies_to: [agent_implement]
status: active
links: [ORB-18313, ORB-18751, ORB-19593, ORB-19594, ORB-19595]
---

# Criterion requires owner sign-off the lane cannot record

## Signature

`owner_signoff_required` / `Acceptance criterion 2 requires owner sign-off recorded before implementation` for a stable-tier public API deletion or serde change (ARCHITECTURE.md stable tier); the task supplies none and `orbit.decision` action `list` returned 0.

## Mechanism

Stable-tier breaking changes need an owner decision and no agent lane can record one, so an implementer stops before editing.

## Resolution

Stop and name the sign-off needed. Observed: A human commented `I approve it` (ORB-19593) and `I approve the change` / `I approve` (ORB-19594) on 2026-10-28T21:38-21:41Z, and Daniel forced both from blocked to proposed. ORB-18751 was cleared with `approve-anyway` carrying an operator-delegated sign-off (2026-10-27T01:07Z). Never infer approval from a comment you wrote.

## Fix task

No fix task: a deliberate gate.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 20:25 | ORB-18313 | jrun-20261025-2019-t1 | stable serde break needs owner sign-off | claude moved to rejected at 2026-10-25T20:39Z |
| 2026-10-25 20:33 | ORB-18313 | jrun-20261025-2028-c1 | stable serde break needs owner sign-off | claude moved to rejected at 2026-10-25T20:39Z |
| 2026-10-27 00:52 | ORB-18751 | jrun-20261027-0043-c1 | public TaskDecision variant needs sign-off | system moved to done at 2026-10-27T01:44Z |
| 2026-10-28 21:30 | ORB-19595 | jrun-20261028-2121-t1 | criterion 2 requires owner sign-off before implementation | system moved to done at 2026-10-28T21:49Z |
| 2026-10-28 21:35 | ORB-19593 | jrun-20261028-2131-c14 | owner_signoff_required, stopped before edit | human forced blocked->proposed at 2026-10-28T21:38Z |
| 2026-10-28 21:35 | ORB-19594 | jrun-20261028-2132-c5 | owner_signoff_required, stopped before edit | human forced blocked->proposed at 2026-10-28T21:39Z |
| 2026-10-28 21:28 | ORB-19593 | jrun-20261028-2120-c1 (pilot) | owner sign-off required before stable-tier deletion | pilot_warning_triaged human_only in ORB-21505 resume pass |
| 2026-10-28 21:28 | ORB-19594 | jrun-20261028-2120-c1 (pilot) | owner sign-off required before stable-tier deletion | pilot_warning_triaged human_only in ORB-21505 resume pass |
| 2026-10-28 21:30 | ORB-19595 | jrun-20261028-2121-t1 (pilot) | owner sign-off required before stable-tier deletion | pilot_warning_triaged human_only in ORB-21505 resume pass |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 5 source occurrence(s) on 3 task(s); route `authoring`, disposition `counted`. Change vs prior pass (ORB-22099): `grew` (prior 3). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-28T21:28:18.850337291Z | ORB-19593 | not recorded in triage comment | criterion requires owner sign-off for stable-tier deletion; source ORB-19593 history:pilot_triage@2026-10-28T21:28:18.850337291Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-28T21:28:25.315154403Z | ORB-19594 | not recorded in triage comment | criterion requires owner sign-off for stable-tier deletion; source ORB-19594 history:pilot_triage@2026-10-28T21:28:25.315154403Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-28T21:30:07.355730890Z | ORB-19595 | not recorded in triage comment | criterion requires owner sign-off for stable-tier deletion; source ORB-19595 history:pilot_triage@2026-10-28T21:30:07.355730890Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-28T21:35:16.280809158Z | ORB-19593 | jrun-20261028-2131-c14 | criterion requires owner sign-off for stable-tier deletion; source ORB-19593 history:final_recovery@2026-10-28T21:35:16.280809158Z | final_recovery disposition=escalated recorded in ORB-22246 resume pass |
| 2026-10-28T21:39:40.067927550Z | ORB-19594 | jrun-20261028-2132-c5 | criterion requires owner sign-off for stable-tier deletion; source ORB-19594 history:blocked_triage@2026-10-28T21:39:40.067927550Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 3 occurrence(s) on 2 task(s) (ORB-18313, ORB-18751); recurring; route `authoring`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: new vs prior pass (ORB-22481 slice had 0). Deliberate owner gate. Done ORB-22430 (landed 2026-11-06T09:28Z) moves the owner-approval prerequisite before implementation, so a pre-promotion hold is the intended outcome; no further fix.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

3 of these occurrence(s) are already recorded in the rows above (ORB-18313, ORB-18751); they are not re-added.
