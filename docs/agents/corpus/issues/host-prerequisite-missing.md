---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18438, ORB-20007]
---

# Criterion needs a host tool or funded policy the host lacks

## Signature

Environmental blocker unchanged: `command -v podman` and `command -v docker` find nothing (ORB-18438), or `orbit config get assistance --json` shows policies=[], unfunded budget, zero ceilings (ORB-20007).

## Mechanism

The acceptance criterion needs a prerequisite no reachable host provides, so a retry fails identically while the implementation sits complete and uncommitted.

## Resolution

Do not requeue. Name the missing prerequisite and its owner in the escalation. Daniel moved ORB-20007 to someday on 2026-10-30.

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-26 04:02 | ORB-18438 | jrun-20261026-0356-c3 | podman and docker absent | claude moved to review at 2026-10-26T05:13Z |
| 2026-10-30 05:15 | ORB-20007 | jrun-20261030-0500-c1 | unfunded assistance policy, labelers missing | codex moved to backlog at 2026-10-30T05:33Z |
| 2026-10-30 05:41 | ORB-20007 | jrun-20261030-0533-c3 | unfunded assistance policy | human forced blocked->someday at 2026-10-30T07:33Z |
