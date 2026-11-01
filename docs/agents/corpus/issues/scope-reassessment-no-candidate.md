---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: [ORB-19066, ORB-20598, ORB-20646]
---

# Implementer stops for scope reassessment with no candidate

## Signature

The implementer stops with a clean worktree and a comment such as `Implementation scope needs reassessment before changes`; final recovery confirms `no implementation or committed candidate` and escalates with `human_action: Re-pilot` or `Re-scope in one decision`.

## Mechanism

The task needs data or breadth the pilot did not see: 152 cited ids across 19 docs for 10 files assessed (ORB-19066), or a per-task denominator the store does not keep because multi-task landings store aggregate counts over one shared range (ORB-20598, `diff_stats.rs` and `landed_diff_stats.rs`).

## Resolution

Do not resume unchanged; it fails the same way. A human decides: split, narrow or archive. Observed: Daniel archived ORB-19066 via orchestrator ORB-19008, and split ORB-20598 into ORB-20646 with "ok C".

## Fix task

No fix task: a legitimate stop. ORB-20646 (done) owns the per-line denominator.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-27 07:28 | ORB-19066 | jrun-20261027-0723-c3 | scope exceeds pilot footprint | human:daniel moved to archived at 2026-10-27T07:52Z |
| 2026-10-31 23:32 | ORB-20598 | jrun-20261031-2330-c5 | per-task denominator data not stored | claude moved to backlog at 2026-11-01T00:30Z |
