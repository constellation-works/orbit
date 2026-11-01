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

A claimed leaf runs on a follower. Settlement on the owner keeps the task outcome without the leaf's run identity, build and root cause, so a later scan cannot tell which machine failed or why. The gap is in the settlement record, not in one scan's narrative.

## Resolution

Do not requeue the scan against the owner store and expect the leaf to appear. The repair is owner-side retention of follower identity, build and root cause. ORB-20906 is that repair. Its task record was already done when this pass read it (`updated_at` 2026-11-01T10:38:42Z), which is under three days before the scan end, so effectiveness is too early to judge.

## Fix task

ORB-20906 (done). Not filed by this pass. No second bug was filed.

## Occurrences

Recorded by ORB-20998 from the retained collection, window 2026-10-29T18:19:18Z to 2026-11-01T18:19:18Z.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01 07:52 | ORB-20833 | jrun-20261101-0746-c3 | F2026-11-014 follower settlement omits leaf identity and cause | friction_reported; repair ORB-20906 later done |
