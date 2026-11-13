---
type: issue
applies_to: [final_recovery, triage_escalation]
status: active
links: [ORB-22357, ORB-23280, ORB-24239]
---

# Operator-cancelled run needs a fresh start or manual rescue

## Signature

`workflow_run_failed ... error_code=RUN_CANCELLED, error=run cancelled by cli` on a `task_pr_pipeline` run, where the cancellation was an operator action (a scope rewrite or an explicit manual-rescue instruction), followed by a blocked task whose final recovery either is refused as stale or escalates with a human action to salvage the retained candidate manually.

## Mechanism

The operator cancelled the run on purpose, so no automated requeue is authorised. ORB-22357 was cancelled for a scope rewrite and delivered fresh; ORB-23280 was cancelled with the candidate preserved in a rescue patch and final recovery escalated for manual salvage under an on-call task. Whether a generic mechanism should distinguish these cases is not established in this pass; both episodes are operator-initiated, not Orbit defects verified here.

## Resolution

Unknown as a general rule. Do not resume or requeue an operator-cancelled run automatically. Follow the operator's recorded instruction for that task (fresh start after a rewrite, or manual salvage of the retained candidate).

## Fix task

None. Two episodes across two tasks in this slice, below the recurrence threshold of 3 occurrences across 2 tasks, and both operator-initiated. Counted in ORB-24239.

## Occurrences

Proposed 2026-11-13 by ORB-24239 from the fixed resume window `2026-10-28T14:54:43Z` through `2026-11-11T14:54:43Z`, after `2026-11-02T03:29:09.726782719+00:00`. This slice counts 2 source occurrences on 2 tasks; route `run_failure`, disposition `counted`. Durable sources: task `history` and `comments` read with `orbit.task.show` and `orbit run show jrun-20261109-0159-t1 --no-reconcile --json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-06T06:07:20Z | ORB-22357 | jrun-20261106-0600-c11 | workflow_run_failed RUN_CANCELLED (run cancelled by cli) after operator scope rewrite; source ORB-22357 history:workflow_run_failed | candidate discarded, fresh run delivered, task done 06:37Z |
| 2026-11-09T01:58:55Z | ORB-23280 | jrun-20261108-2253-c6 | workflow_run_failed RUN_CANCELLED (operator manual-rescue instruction, candidate in rescue patch); final_recovery escalated 2026-11-09T01:59:44Z run jrun-20261109-0159-t1; source ORB-23280 history | operator forced task done 2026-11-09T02:10Z after manual handling |
