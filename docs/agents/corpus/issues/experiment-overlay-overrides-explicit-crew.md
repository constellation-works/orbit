---
type: issue
applies_to: [agent_implement, final_recovery]
status: resolved
links: [ORB-21875, ORB-21111, ORB-21202, ORB-22481]
---

# Experiment overlay overrides the explicit crew on a retry

## Signature

`friction_reported: reconcile explicit crew with experiment overlay; recovery retry; concluded experiment overrides task crew`.

## Mechanism

The persisted recovery diagnosis names an experiment overlay overriding explicit crew selection. The task comment at 2026-11-02T02:15:15Z reports fresh Terra admission after conclusion and explicit Opus selection. This is distinct from a missing recovery crew pool.

## Resolution

ORB-21202 repairs experiment admission/capture precedence at its source; preserve existing admitted treatment and distinguish a fresh admission from a captured run. Do not change a blocked task crew from a mining pass.

## Fix task

ORB-21202 (done): merged as 2ca2146c28fe24d1648bcb4f627f786a6503e549 at 2026-11-02T04:32:12Z. The single collected occurrence predates landing.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `run_failure`, disposition `counted`.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T01:56:37.782193532Z | ORB-21111 | jrun-20261102-0155-t2 | friction_reported: reconcile explicit crew with experiment overlay; recovery retry; concluded experiment overrides task crew; F2026-11-022: recovery reports experiment overlay defeating explicit crew. Later comment 02:15:15Z identifies ORB-21202 and admission after experiment conclusion.; source ORB-21111 history:final_recovery_decided@2026-11-02T01:56:37.782193532Z | review_approved done at 2026-11-02T03:34:27.103395859Z |

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 1 source occurrence on 1 task; route `run_failure`, disposition `counted` (covered by done ORB-21202).

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-02T01:56:37Z | ORB-21111 | jrun-20261102-0155-t2 | final_recovery decision=friction_reported outcome=escalated: Orbit-issued stop and crew-routing defect F2026-11-022; source ORB-21111 history:final_recovery_decided | continued with explicit run-level crew; ORB-21111 done 2026-11-02T03:34Z |
