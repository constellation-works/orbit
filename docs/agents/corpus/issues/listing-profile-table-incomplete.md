---
type: issue
applies_to: [task_pilot, agent_implement, final_recovery]
status: draft
links: [ORB-21875, ORB-21174, ORB-22481]
---

# Listing profile table omits existing surface differences

## Signature

`decision: preserve unspecified listing profile differences and restate table; implement_one contract_ambiguity; missing text and condition-presence rows`.

## Mechanism

A refactor contract required a stop on any unlisted surface difference, while its table omitted bounded text and condition-field presence. Two successive decisions were needed on the same task.

## Resolution

Record all known surface differences together and state how unspecified fields retain current behavior. Preserve redaction, truncation and null-versus-absent semantics.

## Fix task

No new fix: two occurrences on one task, below recurrence threshold. ORB-21174 delivered after the two human refinements.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 2 source occurrences on 1 task; route `authoring`, disposition `counted`.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T01:11:18.457333768Z | ORB-21174 | jrun-20261102-0108-c4 | decision: preserve unspecified listing profile differences and restate table; implement_one contract_ambiguity; missing text and condition-presence rows; Bounded description/target redaction and 4096-byte truncation missing from difference table; human chose preserve policy at 01:44:11Z.; source ORB-21174 history:final_recovery_decided@2026-11-02T01:11:18.457333768Z | status_changed done at 2026-11-02T04:16:48.042586529Z |
| 2026-11-02T01:47:05.604430345Z | ORB-21174 | jrun-20261102-0144-c12 | decision: preserve unspecified listing profile differences and restate table; implement_one contract_ambiguity; missing text and condition-presence rows; Condition null-vs-absent and diagnostics presence missing; human added row plus preserve-unnamed-fields rule at 01:48:11Z.; source ORB-21174 history:final_recovery_decided@2026-11-02T01:47:05.604430345Z | status_changed done at 2026-11-02T04:16:48.042586529Z |

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 2 source occurrences on 1 task, below the recurrence threshold; route `authoring`, disposition `counted`.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T01:11:18Z | ORB-21174 | jrun-20261102-0108-c4 | final_recovery decision=decide: bounded MCP redacts and truncates description and target but the difference table omits them; source ORB-21174 history:final_recovery_escalated | human refinement |
| 2026-11-02T01:47:05Z | ORB-21174 | jrun-20261102-0144-c12 | final_recovery decision=decide: condition diagnostics field differs per surface and is unspecified; source ORB-21174 history:final_recovery_escalated | human refinement; ORB-21174 done 2026-11-02T04:16Z |
