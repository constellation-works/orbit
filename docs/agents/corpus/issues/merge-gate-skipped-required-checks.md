---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-20578, ORB-20615, ORB-20620, ORB-20631, ORB-20641]
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
