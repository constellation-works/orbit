---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-19781, ORB-19785, ORB-19786]
---

# Two tasks own the same friction

## Signature

`criteria AC5/AC8 are unmet for F2026-10-685: it is covered by both ORB-19785 and ORB-19786`, and rejecting the duplicate was refused because both were in-progress.

## Mechanism

Friction curation assigned one friction to two live tasks; the duplicate cannot be closed while both run.

## Resolution

Wait for one to settle, then reject the duplicate; do not reject an in-progress task.

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-29 08:16 | ORB-19781 | jrun-20261029-0753-c31 | friction owned by two in-progress tasks | codex moved to backlog at 2026-10-29T08:41Z |
