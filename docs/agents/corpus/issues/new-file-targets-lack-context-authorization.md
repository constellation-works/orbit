---
type: issue
applies_to: [agent_implement]
status: resolved
links: [ORB-20624, ORB-20362, ORB-20371, ORB-20373, ORB-20375]
---

# New file targets lack a context-creation authorization

## Signature

Pilot hold: `required creation targets are absent at source_revision and task history has no context_creation_authorized entry` for new module files (`scheduler/mod.rs`, `execute/mod.rs`, new fragment files, new help goldens).

## Mechanism

A split or refactor task names new files, but creation intent is only recorded through `orbit.task.update` with `allow_missing_context` and the exact selectors. Comments and plans grant nothing, so the pilot holds promotion.

## Resolution

Grant the exact selectors with `allow_missing_context: true`, confirm the `context_creation_authorized` history entry and re-pilot. Once the filer includes the grant, no hold is raised.

## Fix task

ORB-20624 (done): split tasks must grant creation for new module files.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 03:52 | ORB-18080 | jrun-20261025-0341-t1 | creation targets absent, no creation authorization | system moved to done at 2026-10-25T06:46Z |
| 2026-10-25 04:30 | ORB-18080 | jrun-20261025-0422-c1 | creation targets absent, no creation authorization | system moved to done at 2026-10-25T06:46Z |
| 2026-10-26 01:15 | ORB-18406 | jrun-20261026-0107-t1 | creation targets absent, no creation authorization | system moved to done at 2026-10-26T09:40Z |
| 2026-10-26 01:19 | ORB-18406 | jrun-20261026-0112-c17 | creation targets absent, no creation authorization | system moved to done at 2026-10-26T09:40Z |
| 2026-10-27 19:41 | ORB-19160 | jrun-20261027-1936-c10 | creation targets absent, no creation authorization | system moved to done at 2026-10-27T22:14Z |
| 2026-10-28 22:06 | ORB-19615 | jrun-20261028-2156-c1 | creation targets absent, no creation authorization | system moved to done at 2026-10-28T23:05Z |
| 2026-10-28 22:10 | ORB-19615 | jrun-20261028-2203-c4 | creation targets absent, no creation authorization | system moved to done at 2026-10-28T23:05Z |
| 2026-10-31 03:16 | ORB-20362 | jrun-20261031-0307-c1 | creation targets absent, no creation authorization | claude moved to review at 2026-10-31T04:23Z |
| 2026-10-31 03:40 | ORB-20371 | jrun-20261031-0334-t1 | creation targets absent, no creation authorization | system moved to done at 2026-10-31T05:15Z |
| 2026-10-31 03:41 | ORB-20373 | jrun-20261031-0334-t1 | creation targets absent, no creation authorization | system moved to done at 2026-10-31T06:29Z |
| 2026-10-31 03:42 | ORB-20375 | jrun-20261031-0334-t1 | creation targets absent, no creation authorization | system moved to done at 2026-10-31T05:13Z |
| 2026-10-31 04:27 | ORB-20371 | jrun-20261031-0422-c1 | creation targets absent, no creation authorization | system moved to done at 2026-10-31T05:15Z |
| 2026-10-31 04:29 | ORB-20373 | jrun-20261031-0424-c1 | creation targets absent, no creation authorization | system moved to done at 2026-10-31T06:29Z |
| 2026-10-31 04:31 | ORB-20375 | jrun-20261031-0426-c1 | creation targets absent, no creation authorization | system moved to done at 2026-10-31T05:13Z |
| 2026-10-31 04:40 | ORB-20371 | jrun-20261031-0435-c1 | creation targets absent, no creation authorization | system moved to done at 2026-10-31T05:15Z |
| 2026-10-25 03:49 | ORB-18079 | not recorded in triage comment | new-file targets absent, no context_creation_authorized; operator must grant exa | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |
| 2026-10-25 04:36 | ORB-18083 | not recorded in triage comment | new-file targets absent, no context_creation_authorized; operator must grant exa | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |
| 2026-10-26 05:10 | ORB-18473 | not recorded in triage comment | new-file targets absent, no context_creation_authorized; operator must grant exa | occurrence recorded in ORB-21385 partial pass; pre-cursor, no new action in this pass |
| 2026-10-27 19:41 | ORB-19160 | jrun-20261027-1936-c10 | creation targets absent, no creation authorization | pilot_warning_triaged human_only in ORB-21505 resume pass; operator grant required |
| 2026-10-28 22:06 | ORB-19615 | jrun-20261028-2156-c1 (pilot) | creation targets absent, no creation authorization | pilot_warning_triaged human_only in ORB-21505 resume pass |
| 2026-10-31 03:16 | ORB-20362 | jrun-20261031-0307-c1 (pilot) | creation targets absent, no creation authorization | pilot_warning_triaged human_only in ORB-21505 resume pass |
| 2026-10-31 03:40 | ORB-20371 | jrun-20261031-0334-t1 (pilot) | creation targets absent, no creation authorization | pilot_warning_triaged human_only in ORB-21505 resume pass |
| 2026-10-31 03:41 | ORB-20373 | jrun-20261031-0334-t1 (pilot) | creation targets absent, no creation authorization | pilot_warning_triaged human_only in ORB-21505 resume pass |
| 2026-10-31 03:42 | ORB-20375 | jrun-20261031-0334-t1 (pilot) | creation targets absent, no creation authorization | pilot_warning_triaged human_only in ORB-21505 resume pass |

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 12 source occurrence(s) on 7 task(s); route `authoring`, disposition `counted`. Change vs prior pass (ORB-22099): `grew` (prior 8). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-28T22:06:55.791821643Z | ORB-19615 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-19615 history:pilot_triage@2026-10-28T22:06:55.791821643Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-28T22:10:49.579790466Z | ORB-19615 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-19615 history:pilot_triage@2026-10-28T22:10:49.579790466Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-29T08:56:21.989573161Z | ORB-19808 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-19808 history:pilot_triage@2026-10-29T08:56:21.989573161Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-29T23:42:22.978498459Z | ORB-19974 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-19974 history:pilot_triage@2026-10-29T23:42:22.978498459Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-31T03:16:31.027357349Z | ORB-20362 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-20362 history:pilot_triage@2026-10-31T03:16:31.027357349Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T03:40:48.887415869Z | ORB-20371 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-20371 history:pilot_triage@2026-10-31T03:40:48.887415869Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T03:41:46.841606344Z | ORB-20373 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-20373 history:pilot_triage@2026-10-31T03:41:46.841606344Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T03:42:54.725027920Z | ORB-20375 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-20375 history:pilot_triage@2026-10-31T03:42:54.725027920Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T04:27:31.190215548Z | ORB-20371 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-20371 history:pilot_triage@2026-10-31T04:27:31.190215548Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T04:29:51.635918214Z | ORB-20373 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-20373 history:pilot_triage@2026-10-31T04:29:51.635918214Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T04:31:48.211506589Z | ORB-20375 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-20375 history:pilot_triage@2026-10-31T04:31:48.211506589Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
| 2026-10-31T04:40:29.877200041Z | ORB-20371 | not recorded in triage comment | new/creation targets lack context_creation_authorized entry; source ORB-20371 history:pilot_triage@2026-10-31T04:40:29.877200041Z | pilot_triage disposition=human_only recorded in ORB-22246 resume pass |
