---
type: issue
applies_to: [agent_implement, task_pilot]
status: draft
links: [ORB-18297, ORB-18298]
---

# Criterion is evidenced only in the owner host's live settings

## Signature

Pilot `host_operational` hold, then `pilot_warning_triage disposition=human_only`: `Criteria 1 and 2 can only be evidenced on the owner machine dk-server-2 ... the implementation lane also has no auto-task update tool`. Both tasks asked to change an auto-task cadence (`every_minutes`) that lives in the workspace settings layer, not in tracked source.

## Mechanism

An auto-task schedule set in the owner workspace overrides the shipped definition (`crates/orbit-core/assets/auto_tasks/doc-duties.yaml`, `qa-sweep.yaml`). No code diff can satisfy a criterion that reads the live schedule or later mint history, and no implementation lane holds the auto-task update verb. The pilot's `host_operational` disposition is the designed hold for this case. The two tasks were spawned from ORB-18282 by the same session.

## Resolution

Route the change to the operator on the owner host: run the auto-task update there and confirm with the show verb and a later mint, or record a keep-cadence decision on the task. Do not promote it into a managed delivery lane. ORB-18297 was rejected by `human:daniel` at 2026-10-25T20:06Z and archived 2026-10-29. ORB-18298's later disposition was not read.

## Fix task

No fix task: two occurrences from one filing session, below the recurrence threshold, and the pilot hold behaved as designed.

## Occurrences

Proposed 2026-11-04 by the pilot-block-patterns pass (ORB-22099). Each hold and its triage marker share one assessment, so each task counts once.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 19:50 | ORB-18297 | jrun-20261025-1944-c1 (pilot) | cadence criteria need owner-host evidence | human:daniel rejected at 2026-10-25T20:06Z |
| 2026-10-25 19:50 | ORB-18298 | jrun-20261025-1944-c1 (pilot) | cadence criteria need owner-host evidence | no clearing event read in this pass |
