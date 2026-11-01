---
type: issue
applies_to: [agent_implement]
status: active
links: [ORB-17913, ORB-18682, ORB-18782, ORB-18783, ORB-19553]
---

# Task needs a release step only the release operator can do

## Signature

Pilot or triage: `implementer cannot perform npm publish without release-operator rights and OTP`, the breaking-change list awaits Daniel's confirmation (RELEASING.md step 3), or `make release-check` fails before publication because the registry version is compared.

## Mechanism

Releases belong to Daniel; the missing publication or confirmation cannot be supplied by repository code.

## Resolution

Do not bump versions, tag, publish or classify breaking changes. Hand it to the release operator; Daniel forced ORB-17913 done on 2026-10-25T00:59Z.

## Fix task

No fix task: a release gate.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-24 19:51 | ORB-17913 | jrun-20261024-1920-c1 | release step reserved to the release operator | human forced in_progress->done at 2026-10-25T00:59Z |
| 2026-10-24 22:04 | ORB-17913 | jrun-20261024-2116-c1 | release step reserved to the release operator | human forced in_progress->done at 2026-10-25T00:59Z |
| 2026-10-26 18:47 | ORB-18682 | jrun-20261026-1840-c1 | release step reserved to the release operator | human:daniel moved to archived at 2026-10-26T19:03Z |
| 2026-10-27 01:26 | ORB-18782 | jrun-20261027-0120-c1 | release step reserved to the release operator | human:daniel moved to archived at 2026-10-27T02:37Z |
| 2026-10-27 01:28 | ORB-18783 | jrun-20261027-0120-c2 | release step reserved to the release operator | human:daniel moved to archived at 2026-10-27T02:37Z |
| 2026-10-28 15:56 | ORB-19553 | jrun-20261028-1547-t1 | release step reserved to the release operator | human forced proposed->done at 2026-10-28T16:50Z |
| 2026-10-28 16:24 | ORB-19553 | jrun-20261028-1616-c1 | release step reserved to the release operator | human forced proposed->done at 2026-10-28T16:50Z |
