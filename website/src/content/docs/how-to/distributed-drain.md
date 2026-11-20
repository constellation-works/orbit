---
title: Set up a distributed drain
description: "Let a second machine pull tasks from the machine that owns your backlog, run them, and hand back pull requests."
---

A distributed drain spreads one backlog across machines. The **owner** keeps
the task store and decides what runs. A **replica** pulls tasks over SSH, runs
each one locally, and hands back a pull request. The owner lands it.

:::tip[Let your agent do it]
On the second machine, ask your agent to **set this repository up as a replica
of `<owner host>`**. The `orbit-setup` skill registers the replica, checks that
both machines match, and probes the owner. Then ask it to **start a pull
drain**. The `orbit-orchestrate` skill runs the drain and follows up on any
task that fails.
:::

## How it fits together

- **One owner per workspace.** Sharing Git history does not share Orbit state.
  If two machines already own the same repository, collapse them to one first;
  the [runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/distributed-drain.md)
  shows how.
- **Matching builds.** Every replica runs the same Orbit version and
  distributed-drain protocol as the owner. A mismatch is refused.
- **The owner's checks.** A replica runs the owner's
  `workflow.required_validation_commands`. If the replica's own list differs,
  the drain warns when it starts.
- **SSH is the access control.** Anyone who can `ssh` to the owner owns it. To
  shut a replica out, remove its key from the owner's `authorized_keys`.
- **Landing stays on the owner.** Replicas never merge. Approve each handoff
  on the owner's dashboard, or set `workflow.distributed_completion = "done"`
  on the owner to land every validated handoff.

The owner sends a replica only the tasks it can run. The rest wait in the
owner's backlog until a machine that qualifies asks for work.

| The task has | It goes to a machine that |
|---|---|
| A crew | Can run that crew: enabled, with the provider CLI installed. |
| An `os:macos`, `os:linux` or `os:windows` tag | Runs that OS. A crew's executor can also restrict its OS. |
| A `ui` tag | Has a verified browser kit (`orbit config browser-kit status`). |
| `requires_labels` | Has every label in its `machine.labels`. The machine's OS counts as a label. |

When several tasks qualify, work that only the asking machine can run goes
first. After that, scarcer work leads within a priority. The owner's local
drain follows the same order.

**Review runs on the replica.** If the owner sets
`review.timing = "before_landing"`, or lists the replica's machine ID in
`review.before_landing_hosts`, the replica reviews each pull request it opens
and hands back the evidence. The replica must be able to run the owner's
reviewer crew: `workflow.purpose_crews.pre_landing_review`, or
`workflow.purpose_crews.review` while that is unset. The deprecated
`before_pr` value has the same requirement and reviews before the pull
request opens.

## Set it up by hand

On the second machine, run `orbit init` and pick a task prefix different from
the owner's. Then register the owner as a host; see
[Run Orbit across hosts](/how-to/multi-host/) for what registration stores.

```bash
orbit host add <owner-ssh-target>
orbit host list    # the owner is reachable, with no SKEW
```

Register the checkout as a replica. `orbit config get machine.id` on the
owner prints its machine ID.

```bash
cd <repo>
orbit workspace init --role replica --owner <owner-machine-id>
orbit doctor
```

Optionally, probe the owner before the first pull. Run this read-only check
on the owner. It reports the first reason a real pull would be refused.

```bash
# Read protocol_schema from the owner's build.
ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input '{}'

# Probe as the replica would call.
ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input '{
  "caller_version": "<replica-version>",
  "caller_schema": <protocol_schema>,
  "caller_before_pr": false
}'
```

`<replica-version>` is the replica's `orbit --version`. Set `caller_before_pr`
to `true` when the replica's `review.timing` is `before_pr`.

## Start the pull drain

On the replica, from the replica checkout:

```bash
orbit run auto --pull <workspace> --host <owner> --for 8h --concurrency 3
```

`<workspace>` is the owner's workspace name or `ws_*` ID, and `<owner>` is the
host's name or machine ID from `orbit host list`. Without `--host`, pass the
full `<owner-machine>/<ws_id>` selector that federated discovery reports.

Each claimed task runs locally: it implements, validates, pushes, and opens a
pull request. The replica hands the pull request to the owner, which moves
the task to `review`.

`orbit run show <drain-run>` lists the crews this machine can run and why the
others were left out. If a provider's sign-in fails, the task returns to
`backlog` and the drain stops taking tasks for that provider account's crews.

## Stop, cancel, and settle

| You want to | Run on the replica |
|---|---|
| Stop claiming; let running tasks finish and hand off | `orbit run auto --stop` |
| Cancel the drain; wait for launched tasks | `orbit run cancel <drain-run> --confirm` |
| Cancel the drain and stop its running tasks now | `orbit run cancel <drain-run> --confirm --force` |

`--stop` also delivers any result still waiting to reach the owner. It is
safe to repeat. `orbit doctor` on the replica warns while results are waiting.

`run cancel` returns immediately. Claims the drain has not launched go back to
the owner's `backlog`. The drain reports `cancelling` until its launched tasks
finish, then `cancelled`; follow it with `orbit run show <drain-run>`.

With `--force`, the stopped tasks' claims return to the owner's `backlog`. If
a task's stop cannot be confirmed, its claim stays on the owner and the
command exits 1.

Cancelling one task's run returns that task to the owner's `backlog` and
keeps its candidate available to resume. Add `--block` to leave the task
`blocked` for manual recovery.

## Recover a failed task

A task that fails on a replica goes to `blocked` on the owner, with a summary
naming the replica's run. `orbit run show <run>` on the replica has the
detail. After three failed claims in a row the drain stops asking for work, and
`orbit run show <drain-run>` prints a `Breaker:` line. The drain does not stay
stopped: after a cooldown (15 minutes by default) it asks for one probe task,
and a probe that hands off closes the breaker. Fix the cause and move the
blocked tasks back to `backlog`; if the drain's window has ended, start a new
drain.

Nothing reclaims a stuck claim on its own, and a dead replica holds its claims
until you recover them. On the owner's dashboard, open the task, choose a
target status in the Recovery panel, and use **Recover claim**. The picker
offers only the statuses the task can move to, such as `blocked` to diagnose
or `backlog` to retry. Recovery asks for a reason and a confirmation.

## Recover missing settlement evidence

A replica's runs never appear in the owner's run history. The owner knows a
failed run's build and failing step only from the evidence the replica sent
when it settled the claim:

```bash
orbit run settlements --no-reconcile --json
```

Each entry in `settlements` has a `claim_id` and a `run_evidence` object. Read
its `provenance` first:

| `provenance` | Meaning |
|---|---|
| `null` | The owner holds no run record. The build and failing step are unknown, not absent. |
| `settlement` | The replica sent the record with the settlement. |
| `hydrated` | An operator fetched the record afterwards. |

When a record is present, `build_state` or `root_cause_state` of `unrecorded`
means the record itself lacks that field.

To fetch missing records, run this on the owner as operator. Agents are
refused.

```bash
orbit run settlements-hydrate --follower-workspace <workspace> --limit 10 --json
```

`--follower-workspace` is the workspace name or `ws_*` ID on the replica where
the runs happened. Narrow the selection with `--claim`, `--task`, `--machine`
or `--since`. The command visits claims newest first, 25 per run by default
and at most 100. When the output's `remaining` is above zero, pass
`next_after_claim_id` as `--after` to continue.

Each row in `rows` reports one `outcome`:

| Outcome | Meaning |
|---|---|
| `hydrated` | The record was stored as the claim's evidence. |
| `replayed` | The claim already held the same record. |
| `conflict` | The claim holds a different record and keeps it. |
| `mismatch` | The replica answered with a record that does not belong to this claim. |
| `absent` | The replica holds no such run. |
| `unreachable` | The replica did not answer. Retry later. |
| `no_leaf_run` | The claim never started a run. |

Stored evidence never changes. A default run skips claims that already hold
evidence; add `--recheck` to revisit them. Hydration starts, retries and
cancels no work.

For a drain on one machine, see
[Run a delivery window](/how-to/continuous-delivery/).
