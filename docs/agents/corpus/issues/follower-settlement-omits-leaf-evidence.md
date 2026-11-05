---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-20833, ORB-20906]
---

# Owner settlement omits the follower leaf's identity and cause

## Signature

Final recovery records friction that owner failure scans cannot attribute or fold follower leaves: the owner settlement has no follower job identity, build, or recorded root cause, and `orbit run show` on the owner does not find the follower-local run. Observed as F2026-11-014.

## Mechanism

In the recorded incident, a claimed leaf ran on a follower and the owner kept its task outcome without the leaf's job, build and root cause. A later scan could not attribute the failure from that settlement. ORB-20906 has since added `leaf_run_report` capture and the owner's `run_evidence` projection; legacy settlements can still lack that evidence.

## Resolution

Read `orbit run settlements --no-reconcile --json` on the owner and inspect `run_evidence`; `orbit run show` still need not find the follower-local run. ORB-20906 implemented owner-side retention of the leaf's recorded job, build and root cause. Legacy records need the bounded operator-only `orbit run settlements-hydrate` route, and a build or cause never recorded by the leaf remains `unrecorded`. Requeueing the scan alone cannot recover it. See [the distributed-drain runbook](../../../runbooks/distributed-drain.md).

The original pass read ORB-20906 as done (`updated_at` 2026-11-01T10:38:42Z), under three days before its scan end, and classified effectiveness as too early to judge. That dated observation is not a current effectiveness verdict.

## Fix task

ORB-20906 (done). Not filed by this pass. No second bug was filed.

## Occurrences

Recorded by ORB-20998 from the retained collection, window 2026-10-29T18:19:18Z to 2026-11-01T18:19:18Z.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01 07:52 | ORB-20833 | jrun-20261101-0746-c3 | F2026-11-014 follower settlement omits leaf identity and cause | friction_reported; repair ORB-20906 later done |
