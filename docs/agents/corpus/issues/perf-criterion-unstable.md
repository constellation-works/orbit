---
type: issue
applies_to: [agent_implement, final_recovery]
status: resolved
links: [ORB-20627, ORB-20470, ORB-20473, ORB-20474, ORB-21875]
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

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 2 source occurrences across 2 tasks; route `authoring`, disposition `counted`. Covering repair: ORB-20627. Rows below confirm existing episodes where already backfilled; they are not additional incidents.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T11:32:47.120675574Z | ORB-20474 | jrun-20261031-0922-c3 | human_only / decision: restate unstable warm p95 ratio criterion; implement_one; identical-binary spread; human_only: source-repair authorization requested; identical-binary p95 ratio 0.683 vs earlier 0.601; source ORB-20474 history:blocked_task_triaged@2026-10-31T11:32:47.120675574Z | external_merge_settled done at 2026-10-31T16:08:21.490225763Z |
| 2026-10-31T12:51:42.177713963Z | ORB-20473 | jrun-20261031-0920-c3 | human_only / decision: restate unstable warm p95 ratio criterion; implement_one; identical-binary spread; decision: median-of-three vs every-pair 0.67 threshold; ratios 0.627, 0.700, 0.623; source ORB-20473 history:blocked_task_triaged@2026-10-31T12:51:42.177713963Z | forced rejected at 2026-10-31T16:09:17.973860406Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 3 source occurrence(s) on 3 task(s); route `authoring`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

Boundary reconciliation (ORB-22248): ORB-22246's cursor `resume_after` `2026-10-31T12:50:12.003276941+00:00` is the sort time of its last read candidate, ORB-20473, whose earliest triage run `jrun-20261031-1250-t1` was created then and names only ORB-20473. It is not an occurrence time. The ORB-20473 row below, at `12:51:42.177713963Z`, was read and counted in this slice, and it is the only counted occurrence after that boundary. The next resume pass skips ORB-20473 as a candidate at the boundary and does not count this row again. Its next run-history candidate is ORB-20503 (not read by ORB-22246), run `jrun-20261031-1658-t1`, created `2026-10-31T16:58:06.971837323+00:00`. Evidence: ORB-22248 artifact `pilot-block-boundary-reconciliation.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T09:45:34.645574567Z | ORB-20470 | not recorded in triage comment | performance criterion unstable on identical binaries (warm p95 ratio); source ORB-20470 history:blocked_triage@2026-10-31T09:45:34.645574567Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T11:32:47.120675574Z | ORB-20474 | not recorded in triage comment | performance criterion unstable on identical binaries (warm p95 ratio); source ORB-20474 history:blocked_triage@2026-10-31T11:32:47.120675574Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T12:51:42.177713963Z | ORB-20473 | not recorded in triage comment | performance criterion unstable on identical binaries (warm p95 ratio); source ORB-20473 history:blocked_triage@2026-10-31T12:51:42.177713963Z | blocked_triage disposition=decision recorded in ORB-22246 resume pass |
