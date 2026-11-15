---
type: issue
applies_to: [task_pilot, agent_implement]
status: draft
links: [ORB-21875, ORB-20620]
---

# Design status request differs from the documented vocabulary

## Signature

`advisory utility_warnings: requested Proposed status absent from design vocabulary`.

## Mechanism

The task requested Proposed while design conventions enumerated Draft, Accepted, Implemented and Deprecated. The pilot explicitly said the generator validates stage and this warning does not block promotion.

## Resolution

Follow the task wording and record the vocabulary gap; do not turn an advisory note into a hold.

## Fix task

No new fix: advisory observations below the recurrence threshold.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T23:30:26.936911009Z | ORB-20620 | jrun-20261031-2328-c1 | advisory utility_warnings: requested Proposed status absent from design vocabulary; Criterion 1 requires status Proposed, but docs/design/CONVENTIONS.md section 2 (line 76) and section 7 (lines 185-188) define only Draft, Accepted, Implemented and Deprecated. The generator validates stage, not status (CONVENTIONS line 91). Use the requested status and note the vocabulary gap in the hand-off. Not a blocker.; source ORB-20620 comments:comment@2026-10-31T23:30:26.936911009Z | external_merge_settled done at 2026-11-01T00:13:39.993109179Z |
