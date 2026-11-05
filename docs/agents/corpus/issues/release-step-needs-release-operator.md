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

### ORB-22246 owner scan, 2026-11-05

Reviewed proposal from fixed resume window `2026-10-21T14:44:15Z` through `2026-11-04T14:44:15Z`, after `2026-10-27T19:41:06.034010443+00:00`. This slice counts 2 source occurrence(s) on 1 task(s); route `setup`, disposition `counted`. Change vs prior pass (ORB-22099): `shrank` (prior 5). Rows below confirm existing episodes where already backfilled; they are not additional incidents beyond this slice count.

Durable source evidence: task histories/comments retrieved with `orbit.task.show` fields `pilot_warnings,history,comments,relations,job_run_id,job_run_machine,artifacts` in ORB-22246, and run history with `orbit run history --no-reconcile --json`; the original records remain authoritative.

| Date (UTC) | Task | Run | Matched signature and source | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-28T15:56:04.480909669Z | ORB-19553 | not recorded in triage comment | release step needs release operator / release-check before publication; source ORB-19553 history:pilot_triage@2026-10-28T15:56:04.480909669Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
| 2026-10-28T16:24:27.028899789Z | ORB-19553 | not recorded in triage comment | release step needs release operator / release-check before publication; source ORB-19553 history:pilot_triage@2026-10-28T16:24:27.028899789Z | pilot_triage disposition=decision recorded in ORB-22246 resume pass |
