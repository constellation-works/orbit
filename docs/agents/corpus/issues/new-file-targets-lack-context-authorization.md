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
