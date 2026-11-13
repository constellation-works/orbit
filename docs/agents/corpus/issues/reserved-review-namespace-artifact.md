---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-22978, ORB-24239]
---

# Claimed settlement names an artifact in the reserved review- namespace

## Signature

Claimed leaf fails at `git_commit` with `task_artifacts path 'review-plan.md' is in the reserved review- namespace, which only the review gate writes`, the task goes to blocked, and final recovery completes it as no-diff because the underlying work is already delivered and no tracked change was lost.

## Mechanism

The claiming agent included a `review-*` artifact name in its handoff. That namespace is reserved for the review gate, so the settlement policy correctly denied the commit step even though the candidate work was complete. The exact authoring instruction that produced the name is not established in this pass.

## Resolution

Unknown for the authoring source. Name handoff artifacts outside the `review-` namespace. Final recovery's no-diff completion was correct on the recorded evidence (covering commit reachable, clean checkout, no artifact lost).

## Fix task

None. One episode on one task in this slice, below the recurrence threshold. Counted in ORB-24239.

## Occurrences

Proposed 2026-11-13 by ORB-24239 from the fixed resume window `2026-10-28T14:54:43Z` through `2026-11-11T14:54:43Z`. This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`. Durable sources: ORB-22978 `history` and `comments` read with `orbit.task.show` and `orbit run show jrun-20261108-0401-t2 --no-reconcile --json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-08T04:00:51Z | ORB-22978 | jrun-20261108-0339-c1 | claim_failed blocked: `git_commit` denied, task_artifacts path `review-plan.md` in reserved review- namespace; source ORB-22978 history:claim_failed | final recovery complete_no_diff at 04:02:38Z, task done after review |
