---
type: issue
applies_to: [triage_escalation]
status: resolved
links: [ORB-21875, ORB-21286, ORB-21414]
---

# Triage decision options disagree with the advertised schema

## Signature

`invalid_input: decision options advertised as strings but server requires DecisionOptionInput; triager; schema mismatch`.

## Mechanism

The triage run could not persist its proposed decision: the advertised options schema accepted strings, while the server required DecisionOptionInput objects. This is recorded in the terminal run error, not inferred from an agent transcript.

## Resolution

Use the repaired typed options contract. A refused write is not a triage disposition and must not be counted as a successfully applied marker.

## Fix task

ORB-21414 (done, e6c0574ba012f57f44723922b90b0652b39fa1a7). The single observed failure predates that repair.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `run_failure`, disposition `counted`. Covering repair: ORB-21414.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T06:16:08.427067108+00:00 | ORB-21286 | jrun-20261102-0613-t1 | invalid_input: decision options advertised as strings but server requires DecisionOptionInput; triager; schema mismatch; step triager: cli subprocess reported declared envelope status="failed" despite exit 0: error.code=invalid_input; error.message=The triage decision was not recorded: the advertised options schema accepts strings, but the server requires DecisionOptionInput objects.; source jrun-20261102-0613-t1 run.error_message | No triage marker persisted; task later reworded and delivered. |
