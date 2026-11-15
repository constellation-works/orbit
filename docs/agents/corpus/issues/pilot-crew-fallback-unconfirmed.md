---
type: issue
applies_to: [task_pilot]
status: draft
links: [ORB-21875, ORB-20631]
---

# Pilot recommendation uses an unconfirmed crew fallback

## Signature

`advisory utility_warnings: unreadable crew config leaves recommended crew unconfirmed`.

## Mechanism

The checkout did not expose live crew configuration. The assessment used the documented default and explicitly did not hold promotion. The warning alone does not prove a missing or invalid actual crew.

## Resolution

Resolve actual crew at normal admission from registered configuration. Do not infer a host defect or change assignment from this advisory finding.

## Fix task

No new fix: advisory observations below the recurrence threshold.

## Occurrences

### ORB-21875 owner scan, 2026-11-03

Reviewed proposal from fixed resume window `2026-10-19T09:18:37Z` through `2026-11-02T09:18:37Z`, after `2026-10-31T09:45:34Z`. This slice counts 1 source occurrence on 1 task; route `authoring`, disposition `counted`.

Durable source exports: ORB-21875 artifact `pilot-block-evidence.json`, retrieved with `orbit.task.artifact` action `get`; the original task histories/comments and `orbit run show <run> --no-reconcile --json` remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31T23:55:25.426877318Z | ORB-20631 | jrun-20261031-2353-c1 | advisory utility_warnings: unreadable crew config leaves recommended crew unconfirmed; The workspace crew configuration is not readable inside the checkout, so recommended_crew 'opus' is the documented default fallback (docs/CONFIG.md:1421) and is unconfirmed against the live workspace purpose or complexity pools. Confirm before dispatch. This does not hold promotion.; source ORB-20631 comments:comment@2026-10-31T23:55:25.426877318Z | external_merge_settled done at 2026-11-01T00:13:39.343396204Z |
