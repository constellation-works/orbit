# Distributed drain setup and recovery

One owner checkout, replica checkouts on other machines, and a pull drain on
each replica. Use this when the user wants a second machine to execute the same
logical workspace, or when a claimed attempt needs inspection. Installing
matching binaries is not a rollout; starting `orbit run auto --pull` is.

The owner serves probe, receipt lookup, task admission, bind and settlement
through a deterministic internal RPC selected by Orbit's runtime SSH launch.
These five operations are absent from ordinary MCP discovery and refused by
public `tools/call`, including their formerly advertised spellings. Client names
and initialize metadata grant no internal access. Both endpoints must support
the internal protocol; preflight fails closed without a public fallback. A
replica runs `orbit run auto --pull <selector>` to use that route. Supported
operator diagnostics remain owner-side CLI commands; approval, revocation and
recovery remain owner-dashboard actions. Do not invent tools for them, write a
callers file, or start a second owner store.

Owner/replica registration lives in
[multi-host.md](../../../orbit-setup/references/multi-host.md). SSH federation
and session authority live in
[remote-access.md](../../../orbit-setup/references/remote-access.md). Tool
routing is in [tool-surface.md](../tool-surface.md).

## What v1 does not do

- No heartbeat, automatic reclamation, fleet registry, or follower merge.
- No automatic review. v1 admits only `review_policy = none`. Status `review`
  means a delivery handoff is waiting, not that a reviewer ran.
- Age, reservation TTL, and a missing local run are diagnostics, not proof of
  death.
- Seeded `ship_sweep`, `workspace_ship_pipeline`, and `orbit run ship-sweep`
  stay at their current enablement. This setup does not turn them on.
- Remote machine labels (`--remote-caller-machine-id`) are attribution, not
  credentials. SSH login is owner access. There is no destination callers
  file, forced-command acceptance, KeyBound proof, or replacement identity
  registry.

## Prerequisites

On every participating host:

```bash
orbit --version
orbit config get machine.id
orbit workspace show
orbit doctor
orbit config get operation.review_policy
```

Require one owner per repository, matching binaries, matching distributed-drain
protocol schema `1`, equivalent crew and toolchain resolution, and
`operation.review_policy = none`. Empty
`workflow.required_validation_commands` is fail-closed for a claimed handoff.
A leftover `~/.orbit/mcp-callers.toml` or `~/.orbit/mcp-ssh-acceptance/` is
ignored: `orbit doctor` warns; delete the files. Deny a caller by removing its
key from `~/.ssh/authorized_keys`.

Per-host compiler capacity is independent of drain slots. Keep the shared
build-budget defaults (two heavy slots, four Cargo jobs) unless the operator
raises them.

Managed agents never propagate `--operator` or `ORBIT_OPERATOR`. Claim,
machine, bound run, and phase checks fence attempt ownership from trusted
runtime invocation context, not from a payload label.

## Collapse two owners before any replica work

If both hosts initialized the same repo as owners, stop competing drains on the
host that will become a replica, reconcile in-flight runs and reservations,
export tasks that must move, then re-register. An owner checkout is never
rebound in place, so drop its registration first (registry only; `.orbit` stays):

```bash
ORBIT_OPERATOR=1 orbit workspace remove <workspace-id>
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica --owner <owner-machine-id>
```

`workspace role` reasserts; it is not a takeover. Copying Git history does not
move the control plane.

```bash
orbit task export --ids <task-id> -o <archive>
orbit task import <archive> --on-conflict=renumber
```

`--on-conflict=owner-wins` is for a repeatable mirror of IDs the owner already
minted. Do not rsync the store. Restore pruned `context_files` only from
history:

```bash
orbit task lint <task-id>
orbit task lint <task-id> --restore-pruned
```

Missing files are valid declarations. `--restore-pruned` never guesses.
Reservation TTL on a pulled claim is 14,400 seconds and does not revoke the
claim or shrink the frozen footprint.

## Start a follower's drain

On the follower, from the replica checkout, with the owner in
`~/.orbit/mcp-destinations.toml` and the same
`workflow.required_validation_commands` the owner declares:

```bash
orbit run auto --pull <selector> --for 8h --concurrency 3
```

`<selector>` is the owner's host-qualified selector (`<owner-machine>/<ws_id>`)
from federated `orbit.workspace.list`. The command refuses before submitting
unless the checkout is a replica of that owner and workspace and the owner's
probe admits this executor. It prints a `workspace_pull_pipeline` run ID and
returns.

- Each claim runs locally as `task_claimed_pr_pipeline` and ends at a pull
  request handed to the owner, which moves the task to `review`. The owner
  approves before anything lands; the follower never merges. With
  `workflow.distributed_completion = "done"` on the owner, acceptance
  authorizes the handoff and the owner's landing job merges and completes it,
  as `--complete` does for owner tasks.
- The drain keeps settling claims after `--for` expires, until none is left.
  `orbit run auto --stop` closes the window early; live leaves keep running.
  Each leaf also delivers its own handoff or failure when it ends, so
  cancelling the drain (`orbit run cancel`) strands nothing: its unlaunched
  claims end as failures and live leaves settle themselves. Prefer `--stop`,
  which wastes nothing. `orbit run auto --stop` also flushes any settlement
  still recorded on the follower (for example after the owner was
  unreachable), with or without an active drain.
- An unreachable or refusing owner is reported in each iteration's output and
  retried. A request the owner refused and holds no receipt for closes as
  `Refused`; a committed one is carried forward.
- The leaf's agent runs in claimed mode: the sandbox denies `~/.ssh`, so it
  has no route to the owner and needs none. It is denied `orbit.task.show` /
  `orbit.task.update` and returns its execution summary as step output;
  `claim_handoff` carries it to the owner's `execution_summary`. Never loosen
  the sandbox to give an agent the owner.
- A leaf that fails before handing off settles its claim as a failure: the
  owner's task moves to `blocked` with the leaf run, failed step and error in
  its summary. Inspect the run itself on the follower (`orbit run show <run>`).
  A later claimed retry is judged on its own implementer's summary, so that
  stale `Outcome: failed` needs no hand-clearing before re-dispatch.
- `orbit run concurrency <run-id> --set N` retunes the slot ceiling live. It is
  the only ceiling: the claimed leaf jobs declare no active-run limit of their
  own, so size it to what the follower can carry.
- Each iteration reclaims the `target/` build output of every settled leaf
  whose worker has exited (`reclaimed_build_bytes`) and keeps the checkout.
  `orbit gc worktrees --confirm` on the follower removes settled leaves'
  checkouts without asking the owner; see
  [maintenance.md](../../../orbit-setup/references/maintenance.md).

## Read-only owner surface

Both probe and receipt lookup are owner-served `control_plane` tools. A replica
destination refuses them. They need an identified caller (`agent` or
`operator`); a non-interactive shell uses an agent envelope or
`ORBIT_OPERATOR=1`. They create no claim.

```bash
orbit tool run orbit.drain.probe --input '{
  "caller_version": "<this-binary-version>",
  "caller_schema": 1,
  "caller_review_policy": "none"
}'
```

Declaring version, schema, or review policy reports the first refusal
admission would raise. The probe is never a health check for admission.

```bash
orbit tool run orbit.drain.receipt.lookup --input '{"request_id":"<request-id>"}'
```

Returns `found`, `expired` (permanent tombstone), or `not_found`. `not_found`
does not license a replacement request ID. Naming another machine's namespace
needs operator capability. Idle receipts compact immediately; unsettled claims
keep full receipts. Do not delete tombstones by age.

Claim listing is operator-only and off MCP:

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
```

It inspects. It does not reclaim.

The owner's dashboard reads the same claim state, plus the accepted handoff,
inside the task detail (`orbit web serve --operator`, then
`GET /api/distributed/claims`). There is no distributed tab; the panel appears
only for a task this workspace holds a claim for, and a replica reports that
the owner machine holds that state.

## Recovery

Generic resume of a claimed leaf is refused:

```bash
orbit job resume <run-id>
```

Inspect the claim, task, locks, and the run on the execution machine named by
`job_run_machine`. Reconcile an uncertain merge intent before any reassignment — a
revocation or recovery is refused with `uncertain_merge_intent` until it is.

Approve, revoke and recover are owner-operator actions on the owner's dashboard
(`handoff.approve`, `handoff.revoke`, `claim.recover`). They carry the exact
candidate or phase the operator was shown and are refused with `stale_claim`
when the owner moved on. There is still no registered tool and no CLI verb for
them: do not invent one, do not ship the same task again, and do not treat
`--complete` on a follower as landing. Followers never merge, and a replica
refuses all three with `replica_checkout`. A returning worker after revocation
receives `stale_claim`.

## Leftover epic and review state

There is no epic execution path. Gather evidence with ordinary commands:

```bash
orbit run history -j epic_pipeline --limit 50
orbit task list --tag epic
orbit task show <task-id> --fields status,context_files,job_run_id,job_run_machine
orbit task locks list
orbit doctor
```

Do not treat the workspace as migrated while an old epic/child run, reservation,
or uncertain landing is live, even if the root is already `review`. Inherited-only
epic roots (no own context, descendants that have some) need operator-supplied
own context; nothing inherits the old union.

Failed-run triage is gone. Re-backlog is a deliberate status write after
someone inspects `blocked` and `job_run_machine`.

## Verify

A clean probe, matching versions, `review_policy = none`, and a replica role
mean the hosts are **installed**. Start a drain only when the user asked for
one, and leave schedules untouched. After the first claim, confirm on the owner
that `orbit.drain.claims` shows it on the follower's machine, and that after
handoff the task is in `review` with its PR and nothing merged.
