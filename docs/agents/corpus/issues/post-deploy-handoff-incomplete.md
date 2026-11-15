---
type: issue
applies_to: [task_pilot, agent_implement, final_recovery]
status: draft
links: [ORB-21875, ORB-21111]
---

# Post-deploy handoff lacks a host and exact command

## Signature

`advisory utility_warnings: deployed full-pipeline criterion lacks host and exact command`.

## Mechanism

The task explicitly handed the deployed full-pipeline repeat/dedup check to on-call, but named no host or exact command. Pilot left post_deploy empty and warned against claiming fixture checks as deployed evidence.

## Resolution

Name the host and exact command when authoring an owed criterion. Keep the existing on-call obligation and report candidate checks separately.

## Fix task

No new fix: advisory observations below the recurrence threshold.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 2 source occurrences on 1 task; route `authoring`, disposition `counted`.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-01T23:32:01.808357424Z | ORB-21111 | jrun-20261101-2330-c1 | advisory utility_warnings: deployed full-pipeline criterion lacks host and exact command; Criterion 8 requires the final deployed full pipeline and repeat/dedup run. The task text makes this an explicit on-call ORB-20863 handoff, and no host or exact measurement command is named, so no post_deploy entry is recorded. This does not hold promotion; the implementer must not claim it from fixture success.; source ORB-21111 comments:comment@2026-11-01T23:32:01.808357424Z | review_approved done at 2026-11-02T03:34:27.103395859Z |
| 2026-11-02T01:16:54.102244426Z | ORB-21111 | jrun-20261102-0115-t1 | advisory utility_warnings: deployed full-pipeline criterion lacks host and exact command; Criterion 8 requires the final deployed full pipeline and repeat/dedup run, an explicit on-call ORB-20863 handoff. No host or exact measurement command is named, so no post_deploy entry is recorded. The implementer must not claim it from fixture success.; source ORB-21111 comments:comment@2026-11-02T01:16:54.102244426Z | review_approved done at 2026-11-02T03:34:27.103395859Z |
