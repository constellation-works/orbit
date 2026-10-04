---
type: context
summary: Running log of why Orbit task runs failed or got blocked, one entry per distinct cause, with the fix that closed it.
incident_date: 2026-09-27
last_validated: 2026-10-03
tags: [incident, rca, operations, distributed-drain, sandbox]
paths: ["scripts/test-validate-codex-plugin.sh", "scripts/test-validate-agent-plugin.sh", "crates/orbit-exec/src/macos_sandbox/**", "crates/orbit-core/src/adapter/engine_host/v2_host/pull/**", "crates/orbit-core/assets/activities/**", "crates/orbit-core/assets/executors/claude.yaml", "crates/orbit-agent/src/providers/claude/**"]
related_artifacts: [ORB-13843, ORB-13851, ORB-13852, ORB-13663, ORB-13664, ORB-13612, ORB-13463, ORB-13649, ORB-13605, ORB-13604, ORB-13606, ORB-13642, ORB-13639, ORB-13501, ORB-13492, ORB-13491, ORB-13486]
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

## 2026-10-03: Owner moved to a new host without Bubblewrap

- **Where:** Owner (`hm_9ca6004473492f06`, newly on dk-server-2, Ubuntu 26.04),
  every agent step (`implement_one`, `pilot`) under `linux-bwrap`.
- **Symptom:** `cli invocation failed (permanent): trusted Bubblewrap not
  available at /usr/bin/bwrap; declare allow_fallback: true to permit bare exec`.
  The first sweep's drain (`jrun-20261004-0123-t1`) failed all three leaves, and
  both task-pilot runs failed.
- **Cause:** The owner was migrated by copying `~/.orbit` and the checkouts to a
  fresh OS install, so `orbit init` never ran there and nothing installed the
  `bubblewrap` package or loaded the `bwrap-userns-restrict` AppArmor profile.
  `orbit doctor` (without `providers`) did not flag it. Running `orbit init` would
  not have fixed it either: automatic host preparation covers Ubuntu 24.04, not
  26.04.
- **Fix:** No code defect. The operator installed the package and loaded the
  profile (`sudo apt install bubblewrap && sudo apparmor_parser -r
  /etc/apparmor.d/bwrap-userns-restrict`). `orbit doctor providers` then reported
  every provider `sandbox_ready`. When you move a host by copying it, run
  `orbit doctor providers` before you enable the clock.
- **Rescue:** stopped the drain with `orbit run auto --stop`, then moved the
  blocked tasks back to `backlog`.
- **Tasks:** ORB-13843, ORB-13851, ORB-13852.

## 2026-10-03: `release_locks` rejected its own input after a gate failure

- **Where:** Owner, `task_gate_pipeline` failure cleanup (`gate_invoke` →
  deterministic `release_locks`).
- **Symptom:** `deterministic action release_locks failed: invalid input: unknown
  fields 'run_id', ...` on `jrun-20261004-0123-c5` (and its parent
  `task_auto_pipeline` run `-c1`), after the child `task_pr_pipeline` failed on
  the Bubblewrap cause above.
- **Cause:** Not yet diagnosed. The action input carries fields its schema does
  not accept, so a failed gate run may leave its locks for the next GC.
  `orbit task locks list` showed no leftover locks afterwards.
- **Fix:** open.
- **Tasks:** the same three as the entry above.

## 2026-09-28: Stopping a follower drain strands its live leaves

- **Where:** Follower (macOS) drain coordinator (`workspace_pull_pipeline`) and
  the leaves it admitted.
- **Symptom:** Drain `jrun-20260928-0242-t1` was stopped (it ended `cancelled`)
  at 04:14Z. Four leaves that later succeeded stay in local phase `settling` with
  an AcceptHandoff that was never delivered. Two that failed stay in `launched`
  with no settlement at all. On the owner, the claims stay `running` and the tasks
  stay `in-progress`, so neither the PRs nor the tasks reach completion.
- **Cause:** Only the coordinator that admitted a leaf records and delivers its
  settlement (`pull_refill` → `reconcile_pending`). Cancelling the coordinator
  removes the only settlement path. Live leaves keep running, but nothing reports
  their result to the owner.
- **Fix:** ORB-13663 (#2932). Settlement no longer belongs to the
  admitting coordinator: the admission record is the outbox, each leaf's worker
  records and delivers its own handoff or failure as it terminalizes, and
  `orbit run cancel` / `orbit run auto --stop` (and their dashboard buttons) run
  a settle-only pass that delivers anything still recorded and ends a dead
  drain's unlaunched claims as failures. Cancelling a drain no longer kills or
  strands its live leaves. See the design decision "Settlement belongs to the
  admission record, not to the drain that admitted it".
- **Rescue:** after the follower runs a build with the fix, run
  `orbit run auto --stop` in its replica checkout. It delivers the four recorded
  handoffs (tasks to `review`) and records and delivers the two failures (tasks
  to `blocked`). Claims the owner already revoked close locally as
  `closed_obsolete`.
- **Tasks:** the six leaves of drain `jrun-20260928-0242-t1`: ORB-13636,
  ORB-13658, ORB-13655, ORB-13657 (handed off), ORB-13271 and ORB-13622 (failed).

## 2026-09-27: Headless claude crew hands off while its validation gates run in the background

- **Where:** Follower (macOS) distributed leaf, claude crew (resolved opus), in the
  `implement_one` step.
- **Symptom:** `cli subprocess reported declared envelope status="failed" despite
  exit 0: error.code=validation_incomplete` ("make goldens was still running at
  handoff"). The task goes to `blocked` although the code is complete, and
  `step_failure_recovery` declines.
- **Cause:** The agent starts `make ci-fast`, `ci-lint` and `goldens` with Bash
  `run_in_background`, waits on a `Monitor`, and ends its turn. In
  `claude -p --json-schema` mode, Claude Code immediately forces the
  StructuredOutput call, so the agent must report before the gates finish. The
  executor (`crates/orbit-core/assets/executors/claude.yaml`) passes
  `--tools default`, which includes background Bash and Monitor.
- **Fix:** ORB-13664. The claude provider pins
  `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` on every child it spawns. Verified on
  Claude Code 2.1.283 in `-p --json-schema` mode: Bash then rejects
  `run_in_background`, and `Monitor` is no longer offered, so gates run in the
  foreground.
- **Tasks:** ORB-13612 (rescue PR #2895), ORB-13463 (rescue PR #2896), ORB-13271 (rescue PR #2929).

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
- **Fix:** ORB-13649. Every run-keyed task lookup (commit, merge, blocking on
  run failure, resume) now matches the run ID plus the machine that executed the
  run, so a shared ID resolves to each machine's own task. Recorded in
  [distributed-drain decisions](../design/distributed-drain/4_decisions.md#a-run-is-its-id-plus-the-machine-that-executes-it).
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
- **Release bump without the follower.** Drain admission requires the exact owner
  version and protocol schema. After a release bump the follower is refused until
  it is upgraded.
