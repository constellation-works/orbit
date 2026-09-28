---
type: context
summary: Running log of why Orbit task runs failed or got blocked, one entry per distinct cause, with the fix that closed it.
incident_date: 2026-09-27
last_validated: 2026-09-27
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: ["scripts/test-validate-codex-plugin.sh", "scripts/test-validate-agent-plugin.sh", "crates/orbit-exec/src/macos_sandbox/**", "crates/orbit-core/src/adapter/engine_host/v2_host/pull/**", "crates/orbit-core/assets/activities/**"]
related_artifacts: [ORB-13605, ORB-13604, ORB-13606, ORB-13642, ORB-13639, ORB-13501, ORB-13492, ORB-13491, ORB-13486]
---

# Run failure log

A running record of why task runs failed or ended `blocked`. It has one entry per
distinct cause, not one per task. A task that fails for a cause already listed
goes into that entry's **Tasks** line. Full incident reviews still get their own
dated file in this folder. This log is for the everyday failures that don't need
one.

Each entry records:

- **Where**: the host (owner or follower) and the pipeline step.
- **Symptom**: what the run reported.
- **Cause**: the actual mechanism.
- **Fix**: the task or PR that closed it, or `open`.
- **Tasks**: the tasks that hit it.

Newest entries go first. When you rescue a blocked task, add its cause here before
you close it out.

## 2026-09-27: Owner and follower minted the same child run ID

- **Where:** Follower (macOS) distributed leaf and the owner's own local drain,
  in the `commit` step (`git_commit`).
- **Symptom:** `commit_batch_changes expected exactly one task for job_run_id
  'jrun-20260928-0230-c1', got 2`.
- **Cause:** Run IDs (`jrun-<YYYYmmdd-HHMM>-c<n>`) are unique only within one
  machine's store. The owner's drain and a Mac leaf both created
  `jrun-20260928-0230-c1` in the same minute, so the owner's task store bound two
  tasks to that ID. `commit_batch_changes` looks tasks up by run ID alone, not by
  run ID plus the machine that ran them, so both runs failed at commit.
  ORB-13599 (#2871) stops reuse within one store only.
- **Fix:** open (ORB-13649). Look tasks up by run ID plus machine in `commit_batch_changes`,
  `commit_finalize_artifact_changes` and the other batch lookups. Alternatively,
  make leaf run IDs unique across machines, or have the owner refuse a claim whose
  run ID is already bound to another active task.
- **Tasks:** ORB-13605 (rescue PR #2879), ORB-13604.

## 2026-09-27: Codex plugin validator test raced on fixed temp paths

- **Where:** Follower (macOS) distributed leaf, in the `claim_validate` step
  (`make ci-fast` → `scripts/test-validate-codex-plugin.sh`).
- **Symptom:** `shutil.Error: [... "[Errno 17] File exists:
  '/var/folders/.../T/codex-stale-latest-pin/plugin'"]`, then
  `make: *** [ci-fast] Error 1`.
- **Cause:** `clone_fixture()` writes each test case to
  `base_fixture.parent / name`, a fixed name in the shared `$TMPDIR`, outside the
  per-run `mktemp` directory. It checks whether the folder exists, deletes it,
  then copies, and never cleans the folder up afterwards. Concurrent leaves on one
  host raced on the same path, and the leftover folders stay behind.
- **Fix:** ORB-13650. The base fixture and every case folder now live under the
  per-run `fixture_root`, so the existing cleanup removes them.
  `scripts/test-validate-agent-plugin.sh` had the same pattern and got the same fix.
- **Tasks:** ORB-13606. Its change had already landed via #2867 before validation
  failed.

## 2026-09-27: Claimed Mac leaves could not reach the owner's task tools

- **Where:** Follower (macOS) distributed leaf, in the `implement` step
  (`agent_implement`).
- **Symptom:** The agent reported `task_store_unreachable`. The leaf settled as a
  failure and the owner task went to `blocked`. The finished code was left behind
  in `.orbit/state/worktrees/orbit-<run>`.
- **Cause:** In a claimed run, `agent_implement` told the agent to re-read the task
  and to persist `execution_summary` with `orbit.task.update`. On a follower those
  tools federate to the owner over `ssh <owner>`. The macOS worker sandbox denies
  reads of `~/.ssh`, which is deliberate (`credential_read_denies` in
  `crates/orbit-exec/src/macos_sandbox/compile.rs`). The owner was unreachable
  from inside the leaf by construction, so every claimed Mac leaf failed after
  doing its work.
- **Fix:** ORB-13642 (#2825). In claimed mode the agent returns `execution_summary`
  in its step output. `claim_handoff` delivers it with the settlement, and the
  agent never calls owner task tools. The sandbox deny stays as it is.
  `step_failure_recovery` reads the step output in claimed runs.
- **Tasks:** ORB-13501, ORB-13492, ORB-13491, ORB-13486, and the other Mac leaves
  rescued on 2026-09-27 (PRs #2806–#2844).
- **Rescue:** First recover the stranded owner claim to `backlog` or `blocked`
  (see [distributed-drain runbook](../runbooks/distributed-drain.md)). Then open a
  PR from the leaf's worktree.

## 2026-09-27: Owner-revoked claims stalled every follower drain pass

- **Where:** Follower drain, in `pull_refill` reconciliation.
- **Symptom:** The drain claimed nothing new, and every pass aborted.
- **Cause:** After an operator revoked or recovered a stranded claim on the owner,
  the follower's settle was answered with `stale_claim`. The follower treated that
  as a hard error, so the pass aborted before it could refill.
- **Fix:** ORB-13639 (#2797). The follower closes its local settlement when the
  owner has already ended the claim.
- **Tasks:** Every drain that ran after the ORB-13642 rescues, until the follower
  binary included #2797.

## 2026-09-27: Sandboxed macOS workers could not resolve their runtime binding

- **Where:** Follower (macOS) leaf, at worker start.
- **Symptom:** "managed worker runtime binding unavailable".
- **Cause:** The binding lookup ran the setuid `/bin/ps` to read process start
  identity, and the macOS worker sandbox cannot exec setuid binaries.
- **Fix:** #2792 reads the start identity from libproc instead.
- **Tasks:** The first Mac distributed leaves.

## 2026-09-27: Sandboxed workers could not read the recovery authority DB

- **Where:** Follower (macOS) leaf.
- **Symptom:** Reads of the recovery authority database failed inside the sandbox.
- **Cause:** The authority DB's WAL sidecar files were not kept, so the sandboxed
  reader could not open the database consistently.
- **Fix:** #2786 keeps the WAL sidecars.
- **Tasks:** The first Mac distributed leaves.

## Operational causes (no code defect)

- **Stale follower binary.** Follower-side fixes only take effect after the
  follower binary is rebuilt from `agent-main`. A leaf that fails for an already
  fixed cause usually means an old binary.
- **Cancelled drain strands settlements.** Settlements reach the owner only
  through a running drain. A cancelled drain leaves its tasks `in-progress` on the
  owner until the next drain for that owner runs. Stop drains with
  `orbit run auto --stop`.
- **Release bump without the follower.** Drain admission requires the exact owner
  version and protocol schema. After a release bump the follower is refused until
  it is upgraded.
