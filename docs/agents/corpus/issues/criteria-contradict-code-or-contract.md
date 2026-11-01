---
type: issue
applies_to: [agent_implement]
status: active
links: [ORB-19533, ORB-19553, ORB-19680, ORB-19863]
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
