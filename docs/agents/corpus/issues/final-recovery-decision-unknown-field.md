---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-20833, ORB-27941]
---

# Final recovery decision rejected for an unknown field

## Signature

`final recovery returned a malformed decision: unknown field diagnosis, expected one of friction_id, friction_title, friction_body, human_action, summary`.

## Mechanism

The recovery agent returned a `friction_reported` decision that included `diagnosis`. The current schema rejects that field on `friction_reported`: it requires `fault`, `fault_evidence`, and `human_action`, plus either `friction_id` or both `friction_title` and `friction_body`; `summary` is optional. An unknown field fails the decision, and the engine escalates instead of applying it. This is a contract miss by the agent, not a lost run result. `diagnosis` is accepted on `escalate`.

The same rejection fires for any unknown field, for example `rationale` on a decision that does not take it (ORB-27932, jrun-20261124-1209-c3).

## Where the operator sees both failures

Since ORB-27941 the escalation's diagnosis carries the parser error followed by `the original failure was step `<id>`: <error>`. The error is the failed step's own message, redacted, retaining the first 4096 characters and appending a single `…` only when truncated. The full diagnosis is written to the task's `final_recovery` comment (`diagnosis: ...`); read that comment for the complete evidence. The decision request carries only a summary: the malformed escalation has `summary: None`, so the applier substitutes the first two sentences capped at 280 bytes, and a long unknown-field name can therefore push all original step evidence out of the request. Before that change the diagnosis held only the parser error, so a failure such as `managed_block_not_writable` was visible only in the task's execution summary. The response still authorizes nothing: the decision stays an escalation with an `unknown` fault.

## Resolution

Re-issue the decision with the current accepted fields: include `fault`, `fault_evidence`, and `human_action`, plus either `friction_id` or both `friction_title` and `friction_body`; `summary` is optional. Do not pass `diagnosis` on `friction_reported`. Do not treat the rejection as a task defect. No code change is indicated by this single occurrence.

## Fix task

None. One occurrence, below the recurrence threshold. The refusal is the contract working.

## Occurrences

Recorded by ORB-20998 from the retained collection. No triage marker followed this decision in that snapshot.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01 08:32 | ORB-20833 | jrun-20261101-0830-c3 | unknown field `diagnosis` on a friction_reported decision | escalated; human_action was to inspect the run and move the task by hand |
