---
type: issue
applies_to: [task_pilot, triage_escalation]
status: resolved
links: [ORB-21875, ORB-21264, ORB-21286, ORB-21291, ORB-22481]
---

# Pilot holds on a tool named as the subject of a change

## Signature

`decision|untriaged: reword subject-only criterion or clear admission; validation_tool_warnings/operator_validation_hold; tool mention mistaken for invocation`.

## Mechanism

The positive-tool parser detected an operator-reserved tool name in acceptance text without distinguishing editing its advertised description or testing its runtime boundary from invoking the operation. ORB-21264 was held again after its triage and operator clear; ORB-21286 was reworded to remove the name.

## Resolution

ORB-21291 adds per-criterion subject/invoke classification and drops subject-only operator warnings. Unclassified or actual invocation requirements keep the hold. A recheck must classify current text again; ORB-21324 repairs stale classifications.

## Fix task

ORB-21291 (done, b929807a1f56cf46078399bdda2e3666ce0ac04f). All observed holds precede landing; no regression is established.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 4 source occurrences across 2 tasks; route `pilot`, disposition `counted`. Covering repair: ORB-21291.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T05:58:20.496844381Z | ORB-21286 | jrun-20261102-0556-c1 | decision / untriaged: reword subject-only criterion or clear admission; validation_tool_warnings/operator_validation_hold; tool mention mistaken for invocation; operator_validation_hold despite description-text or runtime-boundary-test scope; tool is the subject, not an operation to execute; source ORB-21286 comments:comment@2026-11-02T05:58:20.496844381Z | status_changed done at 2026-11-02T06:39:03.797631136Z |
| 2026-11-02T06:14:24.850377897Z | ORB-21264 | jrun-20261102-0448-c1 | decision / untriaged: reword subject-only criterion or clear admission; validation_tool_warnings/operator_validation_hold; tool mention mistaken for invocation; decision: tool names identify advertised descriptions; no invocation needed; operator clear later recorded twice.; source ORB-21264 history:pilot_warning_triaged@2026-11-02T06:14:24.850377897Z | status_changed done at 2026-11-02T07:03:07.348942943Z |
| 2026-11-02T06:15:52.477939489Z | ORB-21264 | jrun-20261102-0615-c1 | decision / untriaged: reword subject-only criterion or clear admission; validation_tool_warnings/operator_validation_hold; tool mention mistaken for invocation; operator_validation_hold despite description-text or runtime-boundary-test scope; tool is the subject, not an operation to execute; source ORB-21264 comments:comment@2026-11-02T06:15:52.477939489Z | status_changed done at 2026-11-02T07:03:07.348942943Z |
| 2026-11-02T06:20:59.713701847Z | ORB-21264 | jrun-20261102-0619-c1 | decision / untriaged: reword subject-only criterion or clear admission; validation_tool_warnings/operator_validation_hold; tool mention mistaken for invocation; operator_validation_hold despite description-text or runtime-boundary-test scope; tool is the subject, not an operation to execute; source ORB-21264 comments:comment@2026-11-02T06:20:59.713701847Z | status_changed done at 2026-11-02T07:03:07.348942943Z |

All four holds predate ORB-21291 landing 2026-11-02T07:25:58Z. Two tasks; three assessments on ORB-21264. Primary triage replaces, rather than duplicates, its first assessment.

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 4 source occurrences across 2 tasks (ORB-21264 three holds, ORB-21286 one); route `pilot`, disposition `counted` (covered by done ORB-21291). Rows confirm episodes already recorded above.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T04:49:37Z | ORB-21264 | jrun-20261102-0448-c1 | operator_validation_held: criterion 1 names operator-only `orbit.experiment.abandon` and `orbit.experiment.conclude`; triage pilot_warning_triaged disposition=decision outcome=proposed@06:14:24Z (option 1: reword criteria); source ORB-21264 history:operator_validation_held | held until re-piloted |
| 2026-11-02T06:15:52Z | ORB-21264 | pilot run not read | operator_validation_held again, same requirements, no material change; source ORB-21264 history | operator `task-pilot-admission: clear` at 06:18:16Z (false hold) |
| 2026-11-02T06:20:59Z | ORB-21264 | pilot run not read | operator_validation_held a third time, same requirements | operator `task-pilot-admission: clear` at 06:22:49Z (false hold); approved, ORB-21264 done 07:03Z |
| 2026-11-02T05:58:20Z | ORB-21286 | pilot run not read | operator_validation_held: criterion 1 names operator-only `orbit.task.recover` action `requeue`; source ORB-21286 history:operator_validation_held | acceptance_criteria edited 06:15:20Z, re-piloted clean 06:16:13Z; ORB-21286 done 06:39Z |

Pilot run ids for these three holds were not read (only history operation ids), so the Run column says so; the history timestamps are authoritative. All four precede ORB-21291 landing (2026-11-02T07:25Z); no hold after landing was read.
