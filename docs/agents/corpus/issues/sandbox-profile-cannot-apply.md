---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18099, ORB-19265, ORB-20040]
---

# Nested sandbox profile cannot be applied in the worker

## Signature

`/usr/bin/sandbox-exec -p '(version 1) (allow default)' /usr/bin/true` exits 71 with `sandbox-exec: sandbox_apply: Operation not permitted` (macOS), or `bwrap --unshare-user --ro-bind / / -- /bin/true` is not verified on the owning Linux host. Cargo SKIP results stand in for the confined tests.

## Mechanism

A worker already inside a sandbox cannot nest the profile the confined tests need, so validation is unavailable rather than failed. Skipped tests are not passing tests.

## Resolution

Run the named command outside the worker sandbox on a host where nesting works and attach the result; retrying in the same lane fails the same way. For a security-tagged task only an operator may requeue (ORB-20040).

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 15:14 | ORB-18099 | jrun-20261025-1451-c1 | sandbox-exec exit 71, confined tests unexecuted | merged outside Orbit at 2026-10-25T15:44Z, settled done |
| 2026-10-28 02:00 | ORB-19265 | jrun-20261028-0149-c1 | Seatbelt block, environment unchanged | human forced blocked->review at 2026-10-28T02:16Z |
| 2026-10-30 04:33 | ORB-20040 | jrun-20261030-0423-c4 | bwrap unavailable on owning host | merged outside Orbit at 2026-10-30T05:00Z, settled done |
| 2026-10-25 08:47 | ORB-18196 | not recorded in triage comment | nested sandbox profile cannot be applied in worker | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |
| 2026-10-26 15:07 | ORB-18547 | jrun-20261026-1425-c1 | nested sandbox profile cannot be applied in worker | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 3 source occurrence(s) on 3 task(s); route `setup`, disposition `counted`. Change vs prior pass (ORB-22099): `held` (prior 3). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-28T02:04:28.397828986Z | ORB-19265 | jrun-20261028-0149-c1 | nested sandbox profile cannot be applied / native sandbox evidence cannot be obtained; source ORB-19265 history:blocked_triage@2026-10-28T02:04:28.397828986Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-30T04:37:07.805136364Z | ORB-20040 | not recorded in triage comment | nested sandbox profile cannot be applied / native sandbox evidence cannot be obtained; source ORB-20040 history:blocked_triage@2026-10-30T04:37:07.805136364Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T01:19:57.007423130Z | ORB-20250 | jrun-20261031-0023-c1 | nested sandbox profile cannot be applied / native sandbox evidence cannot be obtained; source ORB-20250 history:blocked_triage@2026-10-31T01:19:57.007423130Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
