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

No open fix task found. Reported deferred by the ORB-20623 pass (3 occurrences).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-30 04:33 | ORB-20040 | jrun-20261030-0423-c4 | security-tagged: only an operator may requeue | merged outside Orbit at 2026-10-30T05:00Z, settled done |
| 2026-10-30 18:37 | ORB-20216 | jrun-20261030-1831-t1 | criterion requires merging PR7130 | codex moved to review at 2026-10-30T18:56Z |
| 2026-10-31 01:04 | ORB-20336 | jrun-20261031-0058-t1 | criterion requires a bypass push | human:daniel moved to archived at 2026-10-31T01:36Z |
