---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18099, ORB-19265, ORB-20040]
---

# Nested sandbox profile cannot be applied in the worker

## Signature

`/usr/bin/sandbox-exec -p '(version 1) (allow default)' /usr/bin/true` exits 71 with `sandbox-exec: sandbox_apply: Operation not permitted` (macOS), or `bwrap --unshare-user --ro-bind / / -- /bin/true` is not verified on the owning Linux host. Cargo SKIP results stand in for the confined tests.

## Mechanism

A worker already inside a sandbox cannot nest the profile the confined tests need, so validation is unavailable rather than failed. Skipped tests are not passing tests.

## Resolution

Run the named command outside the worker sandbox on a host where nesting works and attach the result; retrying in the same lane fails the same way. For a security-tagged task only an operator may requeue (ORB-20040).

## Fix task

No open fix task found.

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 15:14 | ORB-18099 | jrun-20261025-1451-c1 | sandbox-exec exit 71, confined tests unexecuted | merged outside Orbit at 2026-10-25T15:44Z, settled done |
| 2026-10-28 02:00 | ORB-19265 | jrun-20261028-0149-c1 | Seatbelt block, environment unchanged | human forced blocked->review at 2026-10-28T02:16Z |
| 2026-10-30 04:33 | ORB-20040 | jrun-20261030-0423-c4 | bwrap unavailable on owning host | merged outside Orbit at 2026-10-30T05:00Z, settled done |
