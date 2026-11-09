---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-20578, ORB-20615, ORB-20620, ORB-20631, ORB-20641, ORB-21875, ORB-22481]
---

# Merge gate refuses PRs whose required checks were skipped

## Signature

`pr_complete` fails on a docs-only PR with `required check '...' has no successful run on the current head`. CI intentionally skipped Test shards 1-3 and `Check / Clippy / Test` (COMPLETED/SKIPPED), no job failed, and the PR is mergeable. Final recovery escalates; triage records `friction_reported` (F2026-10-727).

## Mechanism

ORB-20578 (d3d87d29a, PR #7350) widened `checked_pr_state` to read required checks on a provider-`Mergeable` head. `classify_owed` maps `SKIPPED` and `NEUTRAL` to `Pending`, so the refusal was unconditional rather than limited to a previously failed check. `.github/workflows/ci.yml` skips the Rust jobs when the change scope is docs, so every docs-only PR hit it (ORB-20615 description).

## Resolution

The candidate is sound; do not requeue or repair it. Confirm the diff is docs-only, that no required check failed on the head, and that the only non-success conclusions are SKIPPED. Escalate with this signature; an operator merges by hand. Observed hand merges: PR #7369 (ORB-20620), #7374 (ORB-20631) and #7388 (ORB-20641), each settled `external_merge_settled` as merged outside Orbit. A recurrence after ORB-20615 landed (agent-main 1387ce618, settled 2026-11-01T01:05Z) is a new defect, not this one.

## Fix task

ORB-20615 (done): the merge gate accepts skipped required checks that never failed on the head. Cause introduced by ORB-20578.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31 23:44 | ORB-20620 | jrun-20261031-2330-c6 | skipped required checks refused by complete_pr | merged outside Orbit at 2026-11-01T00:13Z, settled done |
| 2026-11-01 00:02 | ORB-20631 | jrun-20261031-2355-c3 | skipped required checks refused by complete_pr | merged outside Orbit at 2026-11-01T00:13Z, settled done |
| 2026-11-01 00:44 | ORB-20641 | jrun-20261101-0039-c3 | skipped required checks refused by complete_pr | merged outside Orbit at 2026-11-01T00:52Z, settled done |

ORB-20636 is named in the filing task as hand-merged, but its history shows only a `complete_no_diff` final recovery (run jrun-20261101-0012-c3: PR #7380 already landed on agent-main as 3e4c3a18b outside Orbit). It carries no skipped-check escalation, so it is not an occurrence row.

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 3 source occurrences across 3 tasks; route `run_failure`, disposition `counted`. Covering repair: ORB-20615. Rows below confirm existing episodes where already backfilled; they are not additional incidents.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T23:50:12.125600081Z | ORB-20620 | jrun-20261031-2330-c6 | friction_reported: repair skipped-check policy then resume; pr_complete; intentional skipped required checks lack strict successful run; F2026-10-727: docs-only Rust checks skipped, no failed jobs; separate delivery-policy repair requested; source ORB-20620 history:blocked_task_triaged@2026-10-31T23:50:12.125600081Z | external_merge_settled done at 2026-11-01T00:13:39.993109179Z |
| 2026-11-01T00:10:31.307118292Z | ORB-20631 | jrun-20261031-2355-c3 | friction_reported: repair skipped-check policy then resume; pr_complete; intentional skipped required checks lack strict successful run; F2026-10-727: same strict-success refusal on completed documentation candidate; source ORB-20631 history:blocked_task_triaged@2026-11-01T00:10:31.307118292Z | external_merge_settled done at 2026-11-01T00:13:39.343396204Z |
| 2026-11-01T00:51:01.936832339Z | ORB-20641 | jrun-20261101-0039-c3 | friction_reported: repair skipped-check policy then resume; pr_complete; intentional skipped required checks lack strict successful run; F2026-10-727: same strict-success refusal; no CI rerun can clear intentional skips; source ORB-20641 history:blocked_task_triaged@2026-11-01T00:51:01.936832339Z | external_merge_settled done at 2026-11-01T00:52:05.271898058Z |

Already fixed by ORB-20615 at 2026-11-01T01:05:24Z; all three occurrences predate landing. No duplicate run-failure task or comments on terminal tasks.

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 3 source occurrences across 3 tasks; route `run_failure`, disposition `counted` (covered by done ORB-20615). Rows confirm episodes already recorded above; they are not additional incidents.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T23:44:10Z | ORB-20620 | jrun-20261031-2330-c6 | final_recovery escalate: complete_pr rejects skipped required checks (delivery.rs strict-success guard); triage friction_reported F2026-10-727; source ORB-20620 history:final_recovery_decided, blocked_task_triaged@2026-10-31T23:50:12Z | merged outside Orbit, settled done |
| 2026-11-01T00:02:03Z | ORB-20631 | jrun-20261031-2355-c3 | same signature; triage friction_reported F2026-10-727@2026-11-01T00:10:31Z | merged outside Orbit, settled done |
| 2026-11-01T00:44:35Z | ORB-20641 | jrun-20261101-0039-c3 | same signature; triage friction_reported F2026-10-727@2026-11-01T00:51:01Z | merged outside Orbit, settled done |

All three precede ORB-20615 landing (2026-11-01T01:05Z). No occurrence after landing was found in the candidate set.
