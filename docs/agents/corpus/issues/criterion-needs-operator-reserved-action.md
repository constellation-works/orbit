---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-20216, ORB-20336, ORB-20040]
---

# Criterion needs an operator-reserved merge, push or requeue

## Signature

Pilot or triage: `criterion requires operator-reserved merge, bypass push or security requeue the lane cannot perform`; the lane has `github.pr.list` only and no merge tool; a non-force push of a merge commit to `origin/agent-main` bypassing the agent gate; a task tagged `security` that only an operator may requeue.

## Mechanism

The acceptance criterion is an action the implementer is not authorized to take, so it can never be satisfied inside the lane.

## Resolution

Hand the action to the operator by name and keep the rest of the delivery. Do not merge, push to the integration branch or release.

## Fix task

ORB-21014 was filed from ORB-20998 to address this mechanism.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-30 04:33 | ORB-20040 | jrun-20261030-0423-c4 | security-tagged: only an operator may requeue | merged outside Orbit at 2026-10-30T05:00Z, settled done |
| 2026-10-30 18:37 | ORB-20216 | jrun-20261030-1831-t1 | criterion requires merging PR7130 | codex moved to review at 2026-10-30T18:56Z |
| 2026-10-31 01:04 | ORB-20336 | jrun-20261031-0058-t1 | criterion requires a bypass push | human:daniel moved to archived at 2026-10-31T01:36Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 2 source occurrence(s) on 2 task(s); route `authoring`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-30T18:37:07.817426735Z | ORB-20216 | not recorded in triage comment | criterion needs operator-reserved merge/push/requeue; source ORB-20216 history:pilot_triage@2026-10-30T18:37:07.817426735Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T01:04:43.199746512Z | ORB-20336 | not recorded in triage comment | criterion needs operator-reserved merge/push/requeue; source ORB-20336 history:pilot_triage@2026-10-31T01:04:43.199746512Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
