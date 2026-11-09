---
type: issue
applies_to: [agent_implement]
status: active
links: [ORB-19533, ORB-19553, ORB-19680, ORB-19863, ORB-21875, ORB-20702, ORB-21012, ORB-22481]
---

# Acceptance criterion contradicts the code or a documented contract

## Signature

Pilot `contract_conflicts` (earlier receipts filed it as `adr_conflicts` or `utility_warnings`): the criterion names a command, key or behavior that is absent or contrary at the pinned revision, for example doubled braces versus Axum 0.8 single braces (ORB-18291), `orbit run ship-sweep` absent (ORB-19437), `make release-check` before publication (ORB-19553), append-only pricing history versus a backdated row (ORB-19680, ORB-20702), a serialization test for a pure refactor versus the test policy (ORB-18754).

## Mechanism

The filing assumed a design that the code or the repository's own contract no longer matches, so the criteria cannot all be met as written.

## Resolution

The triage `decision` disposition proposes a replacement; the human chooses amend-spec or restate-criterion. An implementer does not weaken a criterion; it reports the contradiction. A guide rule that the repository guide wins over a conflicting criterion applies.

## Fix task

ORB-21012 was filed from ORB-20998 to address this mechanism.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 04:36 | ORB-18083 | jrun-20261025-0429-c1 | criterion contradicts code or contract | system moved to done at 2026-10-25T06:45Z |
| 2026-10-25 19:48 | ORB-18291 | jrun-20261025-1941-c1 | criterion contradicts code or contract | system moved to done at 2026-10-25T21:42Z |
| 2026-10-26 15:55 | ORB-18661 | jrun-20261026-1551-c1 | criterion contradicts code or contract | system moved to done at 2026-10-26T16:47Z |
| 2026-10-26 17:16 | ORB-18674 | jrun-20261026-1708-c1 | criterion contradicts code or contract | human:daniel moved to backlog at 2026-10-27T01:07Z |
| 2026-10-27 00:52 | ORB-18754 | jrun-20261027-0043-c1 | criterion contradicts code or contract | system moved to done at 2026-10-27T02:21Z |
| 2026-10-27 06:17 | ORB-19035 | jrun-20261027-0610-c1 | criterion contradicts code or contract | system moved to done at 2026-10-27T07:25Z |
| 2026-10-28 08:17 | ORB-19437 | jrun-20261028-0809-t1 | criterion contradicts code or contract | human:daniel moved to rejected at 2026-10-28T08:23Z |
| 2026-10-28 08:20 | ORB-19440 | jrun-20261028-0809-c1 | criterion contradicts code or contract | human:daniel moved to rejected at 2026-10-28T08:23Z |
| 2026-10-28 08:23 | ORB-19437 | jrun-20261028-0814-c1 | criterion contradicts code or contract | human:daniel moved to rejected at 2026-10-28T08:23Z |
| 2026-10-28 13:03 | ORB-19529 | jrun-20261028-1258-c1 | criterion contradicts code or contract | system moved to done at 2026-10-28T21:44Z |
| 2026-10-28 13:31 | ORB-19533 | jrun-20261028-1319-c1 | criterion contradicts code or contract | system moved to done at 2026-10-28T16:44Z |
| 2026-10-28 15:56 | ORB-19553 | jrun-20261028-1547-t1 | criterion contradicts code or contract | human forced proposed->done at 2026-10-28T16:50Z |
| 2026-10-28 16:24 | ORB-19553 | jrun-20261028-1616-c1 | criterion contradicts code or contract | human forced proposed->done at 2026-10-28T16:50Z |
| 2026-10-29 04:07 | ORB-19680 | jrun-20261029-0358-t1 | criterion contradicts code or contract | system moved to done at 2026-10-29T05:10Z |
| 2026-10-29 08:56 | ORB-19808 | jrun-20261029-0846-c1 | criterion contradicts code or contract | system moved to done at 2026-10-29T09:28Z |
| 2026-10-29 16:33 | ORB-19863 | jrun-20261029-1627-t1 | criterion contradicts code or contract | human:daniel moved to archived at 2026-10-29T17:09Z |
| 2026-10-29 23:42 | ORB-19974 | jrun-20261029-2335-c1 | criterion contradicts code or contract | system moved to done at 2026-10-30T01:00Z |
| 2026-10-31 05:00 | ORB-20391 | jrun-20261031-0447-c3 | criterion contradicts code or contract | system moved to done at 2026-10-31T06:39Z |
| 2026-10-31 05:27 | ORB-20400 | jrun-20261031-0520-c4 | criterion contradicts code or contract | system moved to done at 2026-10-31T06:42Z |
| 2026-11-01 03:28 | ORB-20702 | jrun-20261101-0321-c1 | criterion contradicts code or contract | system moved to done at 2026-11-01T03:57Z |
| 2026-10-26 07:11 | ORB-18545 | not recorded in triage comment | criterion contradicts code or contract | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |
| 2026-10-28 08:17 | ORB-19437 | jrun-20261028-0809-t1 (pilot) | criterion contradicts documented contract | pilot_warning_triaged decision in ORB-21505 resume pass |
| 2026-10-28 13:31 | ORB-19533 | jrun-20261028-1319-c1 (pilot) | criterion contradicts documented contract | pilot_warning_triaged decision in ORB-21505 resume pass |
| 2026-10-29 04:07 | ORB-19680 | jrun-20261029-0358-t1 (pilot) | criterion contradicts documented contract | pilot_warning_triaged decision in ORB-21505 resume pass |
| 2026-10-29 16:33 | ORB-19863 | jrun-20261029-1627-t1 (pilot) | criterion contradicts documented contract | pilot_warning_triaged decision in ORB-21505 resume pass |
| 2026-10-31 05:27 | ORB-20400 | jrun-20261031-0520-c4 (pilot) | criterion contradicts documented contract | pilot_warning_triaged decision in ORB-21505 resume pass |

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`. Covering repair: ORB-21012. Rows below confirm existing episodes where already backfilled; they are not additional incidents.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01T03:28:12.452926675Z | ORB-20702 | jrun-20261101-0321-c1 | human_only: obtain reviewer sign-off before historical pricing edit; utility_warnings / adr_conflicts; backdating contradicts preserved rows; Two holding findings: historical repricing contradicts preserved July rows; reviewer cutoff/cache-rate sign-off absent. Later delivered.; source ORB-20702 history:pilot_warning_triaged@2026-11-01T03:28:12.452926675Z | status_changed done at 2026-11-01T03:57:17.908439234Z |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 10 source occurrence(s) on 9 task(s); route `authoring`, disposition `counted`. Change vs prior pass (ORB-22099): `grew` (prior 6). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-28T08:17:11.751121819Z | ORB-19437 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-19437 history:pilot_triage@2026-10-28T08:17:11.751121819Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-28T08:20:32.919099906Z | ORB-19440 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-19440 history:pilot_triage@2026-10-28T08:20:32.919099906Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-28T08:23:02.841651006Z | ORB-19437 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-19437 history:pilot_triage@2026-10-28T08:23:02.841651006Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-28T13:03:52.390811191Z | ORB-19529 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-19529 history:pilot_triage@2026-10-28T13:03:52.390811191Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-28T13:31:22.703661713Z | ORB-19533 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-19533 history:pilot_triage@2026-10-28T13:31:22.703661713Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-29T04:07:41.711389825Z | ORB-19680 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-19680 history:pilot_triage@2026-10-29T04:07:41.711389825Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-29T08:45:14.271904534Z | ORB-19773 | jrun-20261029-0834-c1 | acceptance criterion contradicts code or documented contract; source ORB-19773 history:final_recovery@2026-10-29T08:45:14.271904534Z | final_recovery disposition=escalated recorded in ORB-22246 resume pass |
| 2026-10-29T16:33:52.313789243Z | ORB-19863 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-19863 history:pilot_triage@2026-10-29T16:33:52.313789243Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-31T05:00:06.800819110Z | ORB-20391 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-20391 history:pilot_triage@2026-10-31T05:00:06.800819110Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-31T05:27:50.200262888Z | ORB-20400 | not recorded in triage comment | acceptance criterion contradicts code or documented contract; source ORB-20400 history:pilot_triage@2026-10-31T05:27:50.200262888Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |

### ORB-22481 owner scan, 2026-11-07

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-31T12:50:12.003276941Z` (resume pass continuing ORB-22246). This slice counts 1 source occurrence on 1 task (prior ORB-22246 slice: 10 across 9); route `authoring`, disposition `counted`; ORB-21012 is the covering done fix.

Durable sources: the task `history` and `comments` read with `orbit.task.show` and the run record read with `orbit run show <run> --no-reconcile --json` remain authoritative; ORB-22481 attaches `pilot-block-report.json`.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01T03:28:12Z | ORB-20702 | jrun-20261101-0321-c1 | pilot_warning_triaged disposition=human_only: holding findings adr_conflicts (proposed repricing conflicts with the design decision not to backdate rates) and utility_warnings (criterion 1 needs reviewer approval not recorded on the task); source ORB-20702 history:pilot_warning_triaged | description and acceptance_criteria edited 03:35:54Z, re-piloted clean 03:36:53Z; ORB-20702 done 03:57Z (PR #7420) |

The hold preceded ORB-21012 landing (2026-11-01T19:34Z).

### ORB-22592 owner scan, 2026-11-07

Steady window `2026-10-24T07:23:59Z` through `2026-11-07T07:23:59Z`, capped at the oldest 60 candidates (last read ORB-19240, sort time `2026-10-27T23:42:56Z`). This slice counts 6 occurrence(s) on 6 task(s) (ORB-18083, ORB-18291, ORB-18661, ORB-18674, ORB-18754, ORB-19035); recurring; route `authoring`; disposition `counted`. New since the prior cursor `2026-11-04T14:44:15Z`: 0. Change: grew vs prior pass (ORB-22481 slice had 1). Covered by done ORB-21012 (landed 2026-11-01T19:34Z: hold criteria that contradict the pinned contract before promotion); every occurrence in this slice precedes it.

Durable source evidence: task histories and comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts`, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

6 of these occurrence(s) are already recorded in the rows above (ORB-18083, ORB-18291, ORB-18661, ORB-18674, ORB-18754, ORB-19035); they are not re-added.
