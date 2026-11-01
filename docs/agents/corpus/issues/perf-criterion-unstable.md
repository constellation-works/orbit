---
type: issue
applies_to: [agent_implement, final_recovery]
status: resolved
links: [ORB-20627, ORB-20470, ORB-20473, ORB-20474]
---

# Performance criterion unstable on identical binaries

## Signature

`warm p95 ratio` of 0.921, 0.404, 0.903, 0.834 (ORB-20470), 0.831 and 0.762 versus 0.67 (ORB-20473), 0.683 versus 0.67 then 0.601 for the identical binary (ORB-20474); AC1 unmet while correctness checks pass.

## Mechanism

The criterion pinned a threshold on a single-pair p95 ratio. The identical binary landed on both sides of it (ORB-20474: 0.683, then 0.601), so the check measured noise; the ORB-20627 title calls it a p95 coin-flip.

## Resolution

Do not resume to try another pair. Ask for the criterion to be restated on a stable measure. Daniel rejected ORB-20470 and ORB-20473 on 2026-10-31T16:09Z.

## Fix task

ORB-20627 (done): perf tasks need a stable criterion.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31 09:41 | ORB-20470 | jrun-20261031-0903-c8 | warm p95 ratio criterion unstable | human forced blocked->rejected at 2026-10-31T16:09Z |
| 2026-10-31 11:28 | ORB-20474 | jrun-20261031-0922-c3 | warm p95 ratio criterion unstable | merged outside Orbit at 2026-10-31T16:08Z, settled done |
| 2026-10-31 12:47 | ORB-20473 | jrun-20261031-0920-c3 | warm p95 ratio criterion unstable | human forced blocked->rejected at 2026-10-31T16:09Z |
