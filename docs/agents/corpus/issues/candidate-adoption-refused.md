---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18164, ORB-19689, ORB-20625, ORB-17437, ORB-16193]
---

# Requeue or resume refused for the published candidate

## Signature

A resume of a preserved candidate fails admission: `published candidate lacks a unique requeue binding; inspect explicit source repair ...` (ORB-19689, F2026-10-685); the preserved requeue digest (crew=sol) differs from the current binding after a crew redraw (ORB-18164, F2026-10-594); the commit gate rejects HEAD against the immutable setup checkpoint (ORB-17437); or the failure handoff disagrees with the task's run record (ORB-20625).

## Mechanism

Adoption binds the candidate to the run, crew, requeue digest and failed step that produced it. A redraw, an extra preservation record, a forced status change or a failure handoff whose failed step (`implement_bundle`) differs from the step that raised the error (`implement_one`) breaks the match, so admission correctly refuses. The refusal text for ORB-20625 is recorded only as a summary in history.

## Resolution

Do not retry the same resume. Either an operator discards the candidate and requeues fresh, which is what Daniel did for ORB-20625 ("Discard and requeue") and ORB-18124, or an operator admits an authenticated same-PR source repair. When the cause is a redraw, file or reuse the friction rather than fixing admission inside an unrelated task.

## Fix task

No open fix task found. Landed related tasks by search, not verified to cover each occurrence: ORB-18560 (adoption across an admission redraw), ORB-19786 (authenticate published review-to-backlog moves), ORB-16438 (duplicate preservation record).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-15 02:36 | ORB-16193 | jrun-20261015-0235-t3 | cancelled owner-local published candidate re-admission | codex moved to backlog at 2026-10-15T02:39Z |
| 2026-10-21 07:23 | ORB-17437 | jrun-20261021-0723-t1 | commit gate: HEAD differs from setup checkpoint | merged outside Orbit at 2026-10-21T17:08Z, settled done |
| 2026-10-25 10:39 | ORB-18164 | jrun-20261025-1037-c16 | requeue digest differs after crew redraw (F2026-10-594) | merged outside Orbit at 2026-10-25T10:40Z, settled done |
| 2026-10-29 07:54 | ORB-19689 | jrun-20261029-0753-c30 | no unique requeue binding (F2026-10-685) | human forced blocked->rejected at 2026-10-29T21:37Z |
| 2026-11-01 00:36 | ORB-20625 | jrun-20261031-2344-c9 | failure handoff disagrees with the task's run record; resume refused | Daniel: "Discard and requeue"; fresh run jrun-20261101-0037-c3 landed PR #7390 at 2026-11-01T01:03Z |
