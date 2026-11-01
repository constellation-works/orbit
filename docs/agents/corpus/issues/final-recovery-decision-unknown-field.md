---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-20833]
---

# Final recovery decision rejected for an unknown field

## Signature

`final recovery returned a malformed decision: unknown field diagnosis, expected one of friction_id, friction_title, friction_body, human_action, summary`.

## Mechanism

The recovery agent returned a `friction_reported` decision that included `diagnosis`. The applier accepts `friction_id` or `friction_title` plus `friction_body`, with `human_action` and an optional `summary`. An unknown field fails the decision, and the engine escalates instead of applying it. This is a contract miss by the agent, not a lost run result. Whether a later schema added `diagnosis` was not re-checked beyond this occurrence.

## Resolution

Re-issue the decision with only the accepted fields. Do not treat the rejection as a task defect. No code change is indicated by this single occurrence.

## Fix task

None. One occurrence, below the recurrence threshold. The refusal is the contract working.

## Occurrences

Recorded by ORB-20998 from the retained collection. No triage marker followed this decision in that snapshot.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01 08:32 | ORB-20833 | jrun-20261101-0830-c3 | unknown field `diagnosis` on a friction_reported decision | escalated; human_action was to inspect the run and move the task by hand |
