---
type: issue
applies_to: [final_recovery]
status: draft
links: [ORB-18269]
---

# Provider subprocess exits 1 with no failure text

## Signature

`step implement_one: cli subprocess exited with code 1: Muse run ended with terminal failed and no failure text`, delivery `not_reached`, retained candidate clean with no changed paths.

## Mechanism

Not established. The provider process failed twice the same way in one run (first at 2026-10-25T19:32:52Z) and left no diagnostic. Final recovery found no evidence that the environment had changed since, so the requeue criteria were not met and it escalated. Friction F2026-10-601 was recorded by the recovery run.

## Resolution

Inspect the `implement_one` provider logs for the run to identify the failure before requeueing. The retained candidate holds nothing to preserve. The block did not clear on its own; ORB-18269 was moved to backlog by `human:daniel` after the earlier worktree incident, and its outcome after this episode was not read.

## Fix task

No fix task: one occurrence, cause unknown.

## Occurrences

Proposed 2026-11-04 by the pilot-block-patterns pass (ORB-22099).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 19:45 | ORB-18269 | jrun-20261025-1921-c16 | Muse implement_one exit 1, no failure text | outcome after this episode not read in this pass |
