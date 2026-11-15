---
type: issue
applies_to: [agent_implement, final_recovery]
status: draft
links: [ORB-20160]
---

# Isolated fixture cannot be opened through an authorized route

## Signature

An implementer collects zero valid samples because the copied isolated fixture cannot be opened through an authorized route (`--root` premature end of input, scratch-root policy denied, registry maps to the live checkout).

## Mechanism

The fixture registry still maps the workspace to the live checkout and the only working route resolves the live registry, so isolation is unavailable.

## Resolution

Restore an authorized isolated-fixture route or restate the criterion to use live evidence. Uncertainty: the exact fixture-opening defect was not verified in this pass.

## Fix task

No fix task filed in ORB-22246; counted only (below recurrence threshold in this slice).

## Occurrences

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 1 source occurrence(s) on 1 task(s); route `setup`, disposition `counted`. Change vs prior pass (ORB-22099): `new` (prior 0). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-30T09:59:24.148603222Z | ORB-20160 | jrun-20261030-0920-c1 | isolated fixture cannot be opened through an authorized route; zero valid samples; source ORB-20160 history:blocked_triage@2026-10-30T09:59:24.148603222Z | blocked_triage disposition=human_only recorded in ORB-22246 resume pass |
