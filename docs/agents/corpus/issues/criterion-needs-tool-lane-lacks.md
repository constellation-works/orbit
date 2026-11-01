---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-18545, ORB-19033, ORB-18656, ORB-18848]
---

# Acceptance criterion needs a tool the implementing lane cannot use

## Signature

Pilot `validation_tool_warnings`: `acceptance criterion requires <tool>, a governed operation reserved for the operator capability; required_tools grants allowlist membership` for `orbit.routine.control`, `orbit.workflow.run.show`, `orbit.workflow.run.resume`, `orbit.agent.invoke`, `orbit.experiment.assign`, `orbit.task.retry_handoff`, `orbit.auto_task.mint`, or `github.run.list` / `github.run.logs` that depend on GitHub auth in the lane.

## Mechanism

`required_tools` can add allowlist members but cannot lift operator-only capability or supply authentication, so no implementer can produce the evidence as written.

## Resolution

Verify by source comparison, `--help` or a tool listing, not a live call. Triage proposed replacing the criterion with synthetic-log or source checks for ORB-18702 and ORB-18848; whether it was applied was not checked. Operator resolutions seen: ORB-18197 `clear` (criterion 3 proved from the diff, no tool call needed), ORB-18911 and ORB-19240 `rescope`. Do not call operator tools; state the unverifiable criterion in the summary.

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 08:47 | ORB-18196 | jrun-20261025-0841-t3 | validation tool not available to the lane | system moved to done at 2026-10-25T09:31Z |
| 2026-10-25 08:48 | ORB-18197 | jrun-20261025-0841-t3 | validation tool not available to the lane | system moved to done at 2026-10-25T09:59Z |
| 2026-10-25 10:46 | ORB-18223 | jrun-20261025-1042-c1 | validation tool not available to the lane | system moved to done at 2026-10-25T11:24Z |
| 2026-10-25 19:39 | ORB-18288 | jrun-20261025-1935-c1 | validation tool not available to the lane | system moved to done at 2026-10-26T00:21Z |
| 2026-10-25 19:52 | ORB-18299 | jrun-20261025-1945-t1 | validation tool not available to the lane | merged outside Orbit at 2026-10-25T20:34Z, settled done |
| 2026-10-26 06:33 | ORB-18531 | jrun-20261026-0627-c1 | validation tool not available to the lane | system moved to done at 2026-10-26T12:50Z |
| 2026-10-26 07:11 | ORB-18545 | jrun-20261026-0703-t1 | validation tool not available to the lane | system moved to done at 2026-10-26T13:31Z |
| 2026-10-26 07:22 | ORB-18545 | jrun-20261026-0716-c1 | validation tool not available to the lane | system moved to done at 2026-10-26T13:31Z |
| 2026-10-26 07:41 | ORB-18545 | jrun-20261026-0736-c1 | validation tool not available to the lane | system moved to done at 2026-10-26T13:31Z |
| 2026-10-26 08:26 | ORB-18545 | jrun-20261026-0806-c1 | validation tool not available to the lane | system moved to done at 2026-10-26T13:31Z |
| 2026-10-26 08:37 | ORB-18545 | jrun-20261026-0829-t1 | validation tool not available to the lane | system moved to done at 2026-10-26T13:31Z |
| 2026-10-26 08:48 | ORB-18545 | jrun-20261026-0834-c1 | validation tool not available to the lane | system moved to done at 2026-10-26T13:31Z |
| 2026-10-26 15:14 | ORB-18656 | jrun-20261026-1508-t1 | validation tool not available to the lane | system moved to done at 2026-10-26T23:36Z |
| 2026-10-26 23:53 | ORB-18701 | jrun-20261026-2348-c1 | validation tool not available to the lane | human:daniel moved to rejected at 2026-10-27T01:07Z |
| 2026-10-26 23:55 | ORB-18702 | jrun-20261026-2350-c1 | validation tool not available to the lane | system moved to done at 2026-10-27T05:09Z |
| 2026-10-27 01:14 | ORB-18702 | jrun-20261027-0109-c1 | validation tool not available to the lane | system moved to done at 2026-10-27T05:09Z |
| 2026-10-27 02:50 | ORB-18848 | jrun-20261027-0244-t1 | validation tool not available to the lane | system moved to done at 2026-10-27T05:21Z |
| 2026-10-27 02:56 | ORB-18848 | jrun-20261027-0250-c1 | validation tool not available to the lane | system moved to done at 2026-10-27T05:21Z |
| 2026-10-27 03:24 | ORB-18911 | jrun-20261027-0317-c1 | validation tool not available to the lane | system moved to done at 2026-10-28T00:53Z |
| 2026-10-27 06:16 | ORB-19033 | jrun-20261027-0606-t1 | validation tool not available to the lane | human:daniel moved to rejected at 2026-10-27T10:08Z |
| 2026-10-27 06:21 | ORB-19033 | jrun-20261027-0614-c1 | validation tool not available to the lane | human:daniel moved to rejected at 2026-10-27T10:08Z |
| 2026-10-27 23:43 | ORB-19240 | jrun-20261027-2337-c4 | validation tool not available to the lane | system moved to done at 2026-10-28T00:54Z |
| 2026-10-28 13:14 | ORB-19532 | jrun-20261028-1303-c1 | validation tool not available to the lane | system moved to done at 2026-10-28T21:52Z |
