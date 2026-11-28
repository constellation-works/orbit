# Distributed drain: start and verify a follower

Contents:

- [Start a follower's drain](#start-a-followers-drain)
- [Verify](#verify)

## Start a follower's drain

On the follower, from the replica checkout, with the owner registered by
`orbit host add <owner-ssh-target>` (check `orbit host list` shows it reachable
with this machine's `binary_version` and `protocol_fingerprint`) and the same
`workflow.required_validation_commands` and `review.baseline_commands` the owner
declares (an empty required list on both sides runs no required check; the
drain starts and notes it).

Per-host `.orbit/` config is untracked, so these
lists can drift. `orbit run auto --pull` prints a `Warning:` naming each list
that differs from the owner's, and `orbit doctor` reports it as `pull-gates`:

```bash
orbit run auto --host <owner-name> --pull <workspace-name-or-ws_id> --for 8h --concurrency 3
orbit run auto --pull <selector> --for 8h --concurrency 3
```

`--host` takes the registered owner's exact name or `machine_id`; `--pull`
then takes a workspace name or `ws_*` ID from that host's live workspace list.
Orbit copies the matching descriptor's selector. Without `--host`, `<selector>`
must be the full owner-qualified selector from federated `orbit.workspace.list`;
copy it unchanged rather than spelling it by hand.

A bare workspace without
`--host` is refused with `unknown_selector`, and the local host is refused as
a pull owner. Host-resolution errors and remedies are in
[tool-surface-routing.md](../tool-surface-routing.md#routing-failures-and-remedies).
The command refuses before submitting
unless the checkout is a replica of that owner and workspace and the owner's
probe admits this executor. It prints a `workspace_pull_pipeline` run ID and
returns.

- Without `--for` (or `--for 0s`) the drain makes one admission pass up to
  `--concurrency`, admits no replacements, and ends once those claims settle.
- `--allow-crew <crew-a>,<crew-b>` limits the crews this drain declares, on
  every pass and on resume; unknown or blank names refuse before submission.
  The owner's before-PR review crew still has to run here but need not be
  named. Task crews and configuration are unchanged.

- Each claim runs locally as `task_claimed_pr_pipeline` and ends at a pull
  request handed to the owner, which moves the task to `review`. The owner
  approves before anything lands; the follower never merges. With
  `workflow.distributed_completion = "done"` on the owner, acceptance
  authorizes the handoff and the owner's `task_landing_pipeline` merges and completes it,
  as `--complete` does for owner tasks.
- The drain keeps settling claims after `--for` expires, until none is left.
  `orbit run auto --stop` closes the window early; live leaves keep running.
  Each leaf also delivers its own handoff or failure when it ends, so
  cancelling the drain (`orbit run cancel <run-id> --confirm`) strands nothing.
  - Cancel is
    graceful: the drain stops requesting, returns unlaunched claims to the
    owner's backlog, and ends `cancelled` once its running leaves have settled;
    `--force` stops that drain's leaves too and returns their tasks to the
    backlog. A leaf it cannot confirm stopped keeps its claim, is listed under
    `unstopped_leaves`, and fails the command. The MCP stop
    (`orbit.workflow` action `auto`, `mode: "stop"`) takes the same `force`.
  - `run auto --stop --json`, the MCP stop and the dashboard's stop return one
    result: `outcome`, `pull_settlements`, and per coordinator its `run_id`,
    `job_id`, `outcome`, `remaining_children` (`run_id`, `job_name`, `phase`,
    `child_status`) and `forced_runs`.
    Prefer `--stop`, which ends nothing.
  - The OS clock sweep retries any
    settlement still recorded on the follower (for example after the owner was
    unreachable) once no drain or leaf worker is left to; `orbit run auto --stop`
    flushes it by hand, with or without an active drain. If the drain and its
    leaves died together (a host OOM took the user manager and the clock), the
    follower's `orbit run history` or `orbit run show` interrupts the dead runs
    and settles their claims with the owner; no cancel is needed.
- An unreachable or refusing owner is reported in each iteration's output.
  Protocol skew ends the drain failed immediately with `protocol_skew`; its
  durable settlements remain available to leaf workers and the settle-only
  pass. Other failed passes are retried until three in a row degrade the drain,
  where settlement retries continue. A request the owner refused and holds no
  receipt for closes as `Refused`; a committed one is carried forward.
- A settlement the owner refuses while it still holds the claim (for example a
  footprint widening onto a path the owner protects) stays
  recorded and is retried with a backoff of at most 15 minutes rather than
  every pass.
  - A non-delivery refusal holds new requests; a delivery refusal,
    such as a changed PR head, holds only its own settlement and permits
    unrelated pulls. `orbit run show <drain-run>` prints it once
    as `Settlement refused:` with the owner's reason and remedy; fix the owner,
    and the next due attempt settles it, or `orbit run auto --stop` retries it
    at once.
- The leaf's agent runs in claimed mode: the sandbox denies `~/.ssh`, so it
  has no direct route to the owner. The run broker carries only the scoped
  claimed-task read and other owner calls documented in the [owner surface
  reference](distributed-drain-owner-surface.md); the
  agent is denied `orbit.task.update` and returns its execution summary as
  step output. `claim_handoff` carries it to the owner's `execution_summary`.
  Never loosen the sandbox to give an agent direct owner access.
- A leaf that fails before handing off settles its claim as a failure: the
  owner's task moves to `blocked` with the leaf run, failed step and error in
  its summary. Inspect the run itself on the follower (`orbit run show <run>`).
  A later claimed retry is judged on its own implementer's summary, so that
  stale `Outcome: failed` needs no hand-clearing before re-dispatch.
- `orbit run concurrency <run-id> --set N` retunes the slot ceiling live. It is
  the only ceiling: the claimed leaf jobs declare no active-run limit of their
  own. Before each claim, the pull allocator counts `occupied - min(reviewing,
  N)` against N. A leaf counts as reviewing after the owner's persisted
  before-landing admission has `applies=true`; validation and publishing before
  that boundary count as implementing, while later review revalidation and
  handoff remain reviewing.
  - Review credit is capped at N, but reviewing leaves
    can exceed N as replacements reach review. With an unchanged N and no
    pre-existing excess, total occupied leaves stay at most 2N and implementing
    leaves at most N. Lowering N preserves running leaves, so existing excess can
    remain temporarily.
  - Owner/local drains continue counting every in-flight
    child against their ceiling. `orbit run show <run-id>` splits `Claimed
    leaves:` by stage; review rebuilds still compete for repository build-budget
    slots.
- `orbit run extend <run-id> --for <d> [--concurrency N]` lengthens a live
  pull drain's window on the same run, even in its settling tail after the
  window closed; a second `orbit run auto --pull` would only queue behind it.
  A stopped, cancelling or single-pass (`--for` omitted) drain refuses it, as
  does one whose last pass already closed its window with nothing left to
  settle.
- Each iteration reclaims the `target/` build output of every settled leaf
  whose worker has exited (`reclaimed_build_bytes`) and keeps the checkout.
  `orbit gc worktrees --confirm` on the follower removes settled leaves'
  checkouts without asking the owner; see
  [maintenance-worktree-gc.md](../../../orbit-setup/references/maintenance-worktree-gc.md).

## Verify

A probe with `admits: true`, matching versions and protocol, a replica role
and, when the owner's `review.timing` is `before_pr` or `before_landing`, a
follower that can run the captured reviewer from its effective pool
(`workflow.purpose_crews.pre_landing_review` when non-empty, otherwise
`workflow.purpose_crews.review`) means the hosts are **installed**.

Start a drain
only when the user asked for one, and leave schedules untouched. After the
first claim, confirm on the owner that `orbit.drain.claims` shows it on the
follower's machine, and that after handoff the task is in `review` with its PR
and nothing merged.

With before-PR review captured, the owner accepts a handoff
only with the leaf's passing review of exactly the handed-off head, by the
captured reviewer and contract; otherwise acceptance is refused
(`review_evidence_missing`, `review_not_passed`, `reviewed_head_mismatch`, and
related `review_*` reasons). The task's `review-gate.json` records the review.
