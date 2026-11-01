---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18164, ORB-18539, ORB-18552, ORB-18555, ORB-19289]
---

# Required CI fails on a test outside the candidate's diff

## Signature

Final recovery finds the candidate reviewed and clean and the only failed required check is one test the diff does not touch: `lease_listener_notices::a_notice_stream_holds_a_connection_and_ends_at_shutdown` (hyper UnexpectedMessage), `host_forward::a_foreign_listener_on_the_reserved_port_is_never_the_tunnel` (AddrInUse), `web_serve_shutdown::dashboard_exits_promptly_on_sigterm...`, ci-guardrails `test-compiler-cache-namespaces.sh`, or `draft_handoff::follower_section_gap_draft_returns_for_repair_then_lands` (deterministic).

## Mechanism

Shared-runner flakes (port collisions, compiler-cache stages, shutdown timing) fail a shard for an unrelated PR. The `draft_handoff` case is different: a base test contradicts a landed policy (ORB-18535 screenshot policy, F2026-10-635), so it fails every PR until the base is fixed.

## Resolution

Rerun the failed jobs on the unchanged head (`gh run rerun <run> --failed`). If the rerun is green, resume the published candidate (ORB-18164 was requeued onto its candidate this way). If the same test fails again on an unchanged head, it is a base defect: record friction, do not edit the candidate. Triage recommended exactly this for ORB-18555 and ORB-18969.

## Fix task

No open fix task for the class. Landed fixes found by search for individual signatures, not verified to cover each occurrence: ORB-19037 (lease_listener), ORB-18849 (host_forward), ORB-19343 (web_serve_shutdown).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 08:38 | ORB-18164 | jrun-20261025-0811-c3 | compiler-cache ci-guardrails stage failed (F2026-10-587) | operator requeued at 2026-10-25T08:46Z |
| 2026-10-26 08:05 | ORB-18539 | jrun-20261026-0716-c10 | draft_handoff base test, deterministic (F2026-10-635) | merged outside Orbit at 2026-10-26T08:37Z, settled done |
| 2026-10-26 08:11 | ORB-18559 | jrun-20261026-0728-c10 | draft_handoff base test, deterministic (F2026-10-635) | merged outside Orbit at 2026-10-26T08:37Z, settled done |
| 2026-10-27 02:28 | ORB-18552 | jrun-20261027-0056-c5 | host_forward AddrInUse | system moved to done at 2026-10-27T03:42Z |
| 2026-10-27 05:23 | ORB-18555 | jrun-20261027-0405-c25 | lease_listener_notices UnexpectedMessage | system moved to done at 2026-10-27T06:08Z |
| 2026-10-27 05:26 | ORB-18969 | jrun-20261027-0421-c3 | lease_listener_notices UnexpectedMessage | merged outside Orbit at 2026-10-27T09:09Z, settled done |
| 2026-10-28 03:58 | ORB-19289 | jrun-20261028-0302-c4 | web_serve_shutdown second-shutdown flake | merged outside Orbit at 2026-10-28T05:03Z, settled done |
