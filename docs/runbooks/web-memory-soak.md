---
type: runbook
summary: Measure orbit web serve resident memory under the dashboard polling mix on a large disposable store (Linux).
tags: [operations, performance, dashboard]
paths: ["crates/orbit-web/**", "scripts/web-memory-soak.py", "Makefile"]
related_features: [orbit-web]
related_artifacts: [ORB-14723]
last_validated: 2026-10-08
---

# Soak-Test Dashboard Memory

Use this before and after a change that could alter how much memory the dashboard server
keeps resident: a new or heavier `/api` handler, a memo change, or a change to the heap
policy in `crates/orbit-web/src/heap.rs`.

## Prerequisites

- A Linux host. The harness reads `/proc/<pid>/status` and `/proc/<pid>/maps` and exits 2
  elsewhere.
- `python3` and `git` on `PATH`, and about 1 GB free disk for the fixture.
- The harness strips `ORBIT_*` and `GIT_*` variables and points `HOME` at the fixture, so
  it never reads or writes the operator's Orbit state.

## Procedure

Build the release binary and run the soak with defaults:

```sh
make web-memory-soak
```

Pass harness flags through `SOAK_FLAGS`. To keep the fixture and a JSON report for a
before/after comparison:

```sh
make web-memory-soak SOAK_FLAGS="--work-dir <fixture-dir> --report <report.json>"
```

- `<fixture-dir>` holds the generated store. A directory that already holds one is reused,
  so a second run against a different binary measures identical data. Generating it takes
  a few minutes.
- `<report.json>` receives every sample, the fixture counts, and the budget results.

To compare two binaries, run the script directly against each with the same fixture:

```sh
python3 scripts/web-memory-soak.py --bin <orbit-binary> --work-dir <fixture-dir> --report <report.json>
```

Other flags: `--rounds` (default 20), `--interval` seconds between rounds (default 16,
past the dashboard's 15 s memo TTL so every round recomputes), `--task-list-runs`,
`--server-env KEY=VALUE` (repeatable, extra environment for the server), and
`--report-only` (exit 0 even when a budget fails).

## What it measures

The fixture holds 4,000 tasks, 25,000 job runs, 300,000 audit rows in each audit table,
5,000 invocations, 750 frictions, 11,000 routine fires and a 40,000-line process log.

1. **Fresh-server growth.** A new server answers a warm-up request, then one
   `/api/scoreboard` request; a second new server does the same for `/api/routines`. The
   delta is VmRSS two seconds after the request minus VmRSS before it.
2. **Soak.** One server answers the whole dashboard GET mix concurrently each round.
   VmRSS, VmHWM, the thread count and the number of 64 MiB-aligned anonymous mappings
   (glibc arena heaps) are sampled two seconds after each round.
3. **CLI cost.** Median wall time of `orbit task list` on the same fixture.

## Verification

The run exits 0 when every budget holds, and prints one `PASS` or `FAIL` line per check:

| Check | Budget |
|---|---|
| `soak_drift_within_15pct` | VmRSS after round 20 within 15% of VmRSS after round 3 |
| `peak_rss_under_512mb` | Peak VmHWM under 512 MB |
| `single_request_under_20mb:<path>` | One request on a fresh server grows VmRSS by under 20 MB |

The summary line also prints the range VmRSS spans from round 3 onwards. A ratchet shows in
the per-round lines as VmRSS that keeps climbing. A swing within a steady range is
round-to-round variation in what the concurrent requests leave live, such as pooled SQLite readers and
memoized payloads. A swing like that can fail the two-sample drift check in either direction.

Compare the `orbit task list` median across binaries yourself; it varies with host load,
so interleave runs on a busy host.

## Interpreting a failure

- **Rising RSS with a rising arena-mapping count.** Memory freed on blocking-pool threads
  is staying in glibc arenas. Check that `heap::configure` still runs before the server's
  tokio runtime starts and that `heap::trim_after_requests` wraps the router.
- **One endpoint's fresh-server growth over budget.** That handler's working set is too
  large: look for whole-row hydration or JSON parsing of every record where a narrow
  projection would do.

## Related references

- [Build budget](build-budget.md) — the Makefile target builds through it.
- [`crates/orbit-web/src/heap.rs`](../../crates/orbit-web/src/heap.rs) — the server's
  arena cap, mmap threshold and post-request trim.
