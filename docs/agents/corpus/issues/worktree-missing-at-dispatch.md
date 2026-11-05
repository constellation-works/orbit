---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-18269, ORB-18270, ORB-18271, ORB-22100]
---

# Run dispatch finds no worktree directory

## Signature

Before ORB-22100, dispatch failed with `snapshot Git state in '<worktree>': execution failed: failed to spawn git: No such file or directory (os error 2)`. With ORB-22100, the first failure is `worktree integrity violation worktree_mismatch` whose reason reads `worktree is missing: workspace_path '<worktree>' is not a directory`. Final recovery then escalates with `did not return a decision: worktree integrity violation worktree_mismatch ... workspace_path '<worktree>' is not a directory`.

## Mechanism

The run's worktree directory did not exist when dispatch took its Git snapshot. Git is spawned with the worktree as its current directory, so a missing directory returns ENOENT, which reads as a missing `git` binary (`crates/orbit-engine/src/activity_job/workspace/fingerprint.rs`, `git_run_bytes` in `crates/orbit-engine/src/executor/automation/vcs/git.rs`). Final recovery's declared-pair guard names the same condition correctly (`declared_pair.rs`). Why the directory was absent is **not established**: the three runs began at `workflow admission: worktree_setup` and failed within 22 seconds, on dk-server-2 and mac-mini-1 within five minutes. Hourly `worktree-gc-orbit` runs start at minute :01, outside that window, so garbage collection is not shown to be the cause.

## Resolution

Do not read the escalation text as unconcluded checks or as a missing binary. Check that the run's worktree path exists on the executing host. When it does not, the retained candidate holds nothing; requeue for a fresh attempt. ORB-18269 and ORB-18270 and ORB-18271 each left the blocked state by an operator move (see Occurrences).

## Fix task

ORB-22100 (tagged `run-failure-patterns`) makes the snapshot check the worktree directory before it spawns Git, so the first failure names a missing worktree instead of a missing binary. It does not identify why the directory was absent; that cause stays open until a later occurrence has a readable record.

## Occurrences

Proposed 2026-11-04 by the pilot-block-patterns pass (ORB-22099). These three rows were first filed under `final-recovery-no-decision-checks-unconcluded` by ORB-21385; their text shares the `did not return a decision` prefix but the diagnosis is `worktree_mismatch`, so they are moved here.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-25 18:36 | ORB-18270 | jrun-20261025-1832-c3 | git spawn ENOENT at dispatch, then worktree_mismatch not a directory | human:daniel moved to proposed (forced) at 2026-10-25T19:02Z |
| 2026-10-25 18:36 | ORB-18271 | jrun-20261025-1836-c3 | git spawn ENOENT at dispatch, then worktree_mismatch not a directory | human:daniel moved to backlog at 2026-10-25T19:02Z |
| 2026-10-25 18:37 | ORB-18269 | jrun-20261025-1832-c1 | git spawn ENOENT at dispatch on mac-mini-1, then worktree_mismatch | human:daniel moved to backlog at 2026-10-25T19:03Z |
