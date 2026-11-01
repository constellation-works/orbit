---
type: guideline
applies_to: [agent_implement, final_recovery, task_pilot, triage_escalation]
status: active
links: []
---

# Evidence and authority

## Rule

Check the current task, candidate and run evidence before recommending an
option. Cite matching issue signatures and decision precedent when relevant.
A previous human answer is evidence for a recommendation, never the answer to
a new decision request.

## Rationale

A similar failure can have a different owner, head revision or lifecycle state.
Corpus knowledge does not expand the activity's permissions.

## Escalation

Escalate an unresolved authority conflict with the exact refusal and current
evidence. Do not manufacture a new answer or bypass a gate.
