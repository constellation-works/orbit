---
title: Set up a distributed drain
description: "Let a second machine pull work from the machine that owns your backlog: ask your agent to set it up, then start a pull drain on the replica."
sidebar:
  order: 4
---

A distributed drain spreads one backlog across machines. The **owner** keeps
the task store and decides what runs. A **replica** pulls one task at a time
over SSH, runs it locally, and hands back a pull request. The owner lands it.

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
- **Matching machines.** Every replica runs the same Orbit version and
  distributed-drain protocol revision and fingerprint as the owner. A leaf
  runs the owner's captured `workflow.required_validation_commands`; a
  difference from the replica's list warns when the drain starts. If the owner
  turns on before-PR review (`review.timing = "before_pr"`, deprecated; new setups use `before_landing`), every replica must be able to run
  the owner's pre-landing reviewer (`workflow.purpose_crews.pre_landing_review`,
  or `workflow.purpose_crews.review` while that is unset): each pulled task is then reviewed, and the
  reviewer's fixes committed, on the replica before its pull request opens.
  Crews may differ: a replica only receives tasks whose crew it can run. Operating systems may differ too: a
  task tagged `os:macos` (or `os:linux`, `os:windows`) only goes to a machine
  running that OS, and waits in the owner's backlog until one asks for work.
  The crew's executor can also restrict its OS. A task tagged `ui` additionally
  needs a verified browser kit on the executing host (`orbit config browser-kit status`).
  A task's `requires_labels` go only to a machine whose `machine.labels`, plus
  its OS, include every one; the replica declares its labels with each pull, so
  the owner knows them once it has pulled. Owner and replicas must run releases
  that agree on this field: an older peer is refused as protocol skew.
- **SSH is the access control.** Anyone who can `ssh` to the owner owns it. To
  shut a replica out, remove its key from the owner's `authorized_keys`.
- **Landing stays on the owner.** Approve each handoff on the owner's
  dashboard, or set `workflow.distributed_completion = "done"` on the owner to
  land every validated handoff, the way `--complete` lands its own tasks.

The owner can also require review after the replica opens its PR, with
`review.timing = "before_landing"`, or only for selected replica machine IDs
with `review.before_landing_hosts`. The replica runs the captured reviewer
and hands back evidence for the reviewed head; it never merges.

Admission uses a live executor census. Within a dispatch band, work only the
requesting host can run leads ahead of priority; among the remaining work,
scarcer tasks lead within a priority. This also applies to the owner's local drain.

## Set it up by hand

On the second machine, run `orbit init` and pick a task prefix different from
the owner's, then register the checkout as a replica:

```bash
orbit init
cd <repo>
orbit workspace init --role replica --owner <owner-machine-id>
```

`orbit config get machine.id` on the owner prints its machine ID. Then register
the owner on this machine, as in
[Connect Your Agent](../mcp-integration/#register-the-federated-mux):

```bash
orbit host add <owner-ssh-target>
# Confirm that the owner is reachable and reports its version and protocol.
orbit host list
```

Check that the two machines match. On each one:

```bash
orbit --version
# The owner's value decides. With review.timing at before_pr,
# the replica must be able to run the reviewer crew.
orbit config get review.timing
orbit config get workflow.purpose_crews.pre_landing_review
orbit config get workflow.purpose_crews.review
orbit doctor
```

Then probe the owner. On the owner, run this read-only check; it reports the
first reason a real pull would be refused. Read `protocol_schema` from the
installed binary instead of copying a revision from this page. Use that value
for `caller_schema` only after confirming the replica runs the matching
build; it is the distributed-drain revision, not the MCP protocol revision.
Replace `<replica-version>` with the replica's `orbit --version` value and
set `caller_before_pr` to `true` when its `review.timing` is `before_pr`, else
`false`:

```bash
DRAIN_SCHEMA=$(
  ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input '{}' |
    node -pe 'JSON.parse(require("fs").readFileSync(0))
      .protocol_schema'
)
ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input "{
  \"caller_version\": \"<replica-version>\",
  \"caller_schema\": $DRAIN_SCHEMA,
  \"caller_before_pr\": false
}"
```

## Start the pull drain

On the replica:

```bash
orbit run auto --pull <owner-machine>/<ws_id> --for 8h --concurrency 3
```

The selector is the owner workspace's `selector` from federated discovery.
`orbit run auto --pull <workspace> --host <owner>` resolves the same selector
from the owner's live workspace list.
Each task the replica claims runs as a local run that implements, validates,
pushes, and opens a pull request, then hands it to the owner, which moves the
task to `review`.

When the drain starts, it checks which crews this machine can run: enabled,
with the provider CLI installed. Tasks on other crews stay in the owner's
backlog for another machine. `orbit run show <drain-run>` lists the runnable
crews and why the others were left out. An authentication failure sends its
task back to `backlog` and excludes that provider account's crews. A crew with
its own `env` has its own account, so other accounts remain usable. Delayed
authentication probes can re-admit the affected account within the same drain.

## Stop, cancel, and settle

`orbit run auto --stop` on the replica stops new claims. Running tasks still
finish and hand off, and it delivers any result still waiting to reach the
owner. It is safe to repeat, and `orbit doctor` on the replica warns when
results are waiting.

`orbit run cancel <drain-run> --confirm` cancels a running pull drain
gracefully. It stops new claims immediately and returns claims it has not
launched to the owner's `backlog`. Launched leaves keep running until they
finish and their outcomes reach the owner; the drain reports `cancelling`
during that wait, then ends `cancelled`. The command returns immediately;
follow the wait with `orbit run show <drain-run>`.

Add `--force` to stop the drain and its running leaves without waiting for
them to finish. Their claims return to the owner's `backlog` with the reason.
A leaf whose stop cannot be confirmed keeps its claim on the owner, is
reported, and makes the command exit 1. Only that drain's leaves are affected.

Cancelling a task leaf directly returns its task to the owner's `backlog`
and keeps its candidate available to resume. Add `--block` to keep that task
blocked for manual recovery instead.

## Recover a failed task

A task that fails on a replica goes to `blocked` on the owner, with a summary
naming the replica's run. `orbit run show <run>` on the replica has the full
detail. After several failures in a row the drain stops asking for work. Fix
the cause, move the blocked tasks back to `backlog`, and start a new drain. Or
ask your agent on the replica what went wrong: the `orbit-orchestrate` skill
reads the run's evidence and names the cause.

Nothing reclaims a stuck claim on its own. On the owner's dashboard, open the
task, choose a target status in the Recovery panel and use **Recover
claim**. The picker offers only the statuses the task can move to from where it
stands: `blocked` to diagnose, `backlog` to retry, or `review`, `done`,
`archived`, `rejected` or `someday` where the task lifecycle allows them.
`done` appears only when the task meets the normal completion rules. Recovery
asks for a reason and a confirmation.

## Recover missing settlement evidence

A follower leaf runs on its own host, so the owner's run history never holds
it. What the owner knows of its job, build, and failing step is the claim's
`run_evidence` in `orbit run settlements --no-reconcile --json`. The JSON
document has a `settlements` array; each settlement carries its `claim_id` and
its `run_evidence` object. Read `run_evidence` in two parts.

- `provenance` says whether the owner holds the follower's run record at all.
  `null` means it holds none: the claim settled before the owner recorded
  leaf-run evidence, or its follower sent none, and no operator has hydrated
  it since. `settlement` means the follower sent the record with the
  settlement. `hydrated` means an operator observed it afterwards.
- `build_state` and `root_cause_state` say whether the held record has a build
  and a failing step. `unrecorded` there means that the record lacks the field.
  By itself it neither proves that evidence is missing nor proves that
  hydration never ran. A settlement or hydrated record can still have an
  unrecorded build or failing step.

Only a present record says what the run held. With `provenance` `null`, the
build and failing step are unknown on the owner: `build` is `null`, and both
states are `unrecorded` because there is no record to read. Hydration may
recover them, so a `null` build there does not show that the run held none.
The owner does not fill the gap by guessing: nothing is inferred from its own
runs, from any current build, or from the settlement's text. When a record is
present, a `null` build with `build_state` `unrecorded` does mean the run
record held no build.

Run this recovery on the owner, as operator. Agents and claimed workers are
refused; they read `orbit run settlements --no-reconcile --json` instead:

```bash
orbit run settlements-hydrate \
  --follower-workspace orbit \
  --limit 10 --json
orbit run settlements-hydrate \
  --follower-workspace orbit \
  --limit 10 \
  --after <next_after_claim_id> \
  --json
orbit run settlements-hydrate \
  --follower-workspace orbit \
  --task <task-id> \
  --recheck --json
```

The hydration JSON document has a `rows` array plus `remaining` and
`next_after_claim_id`. Each row includes its `claim_id` and `outcome`. When
`remaining` is greater than zero, pass the returned `next_after_claim_id` as
the next command's `--after` value. It is the last claim visited on that page,
regardless of its outcome. The CLI resolves it against the filtered settlement
list before excluding claims that already hold evidence, so it remains usable
when the last row was hydrated; there is no need to search the rows for a
particular outcome. `unreachable`, `mismatch`, and `absent` claims remain
eligible in a later default run because they gained no evidence. Claims that
already hold evidence are selected again with `--recheck`.

`--follower-workspace` names the workspace on each follower where its leaves
ran, by name or `ws_*` ID as that host lists it. Narrow the selection with
`--claim`, `--task`, `--machine` (ID or name), and `--since` (RFC 3339 time or
duration such as `7d` or `24h`). Without `--recheck` only claims that still
hold no evidence are visited; `--recheck` also revisits claims that already
hold evidence, to confirm an identical replay or report a conflict.
Selection is newest first. `--limit` bounds one run (default 25, at most
100); `remaining` counts selected claims after this page. `next_after_claim_id`
is set only when another page remains; pass it back as `--after` to visit that
page. Paging covers unsuccessful outcomes too, and an `--after` ID outside the
selection is refused.

Each visited claim reports one outcome:

- `hydrated`: the follower's run record was kept as the claim's immutable
  `hydrated` evidence.
- `replayed`: the claim already held the same fact; nothing was written.
- `conflict`: the claim holds different evidence and keeps it; nothing was
  written.
- `mismatch`: the follower answered a record that does not belong to this
  claim's tuple: another run, another machine, not a claimed leaf's job, not
  this claim's task, not terminal, or an admitting drain that does not pull
  from this owner.
- `absent`: the follower answered that it holds no such run. Only this
  outcome means the evidence is absent there.
- `unreachable`: the follower could not be asked or did not answer. This is
  not proof of absent evidence; retry later.
- `no_leaf_run`: the claim never bound a leaf run, so there is nothing to
  observe.

Recorded evidence is immutable: an identical replay is accepted, anything else
is a conflict, and the held fact stands. Hydration starts, retries, and
cancels no work and changes no task, claim, or run lifecycle; the only write
is the evidence row.

The full flag list is the `settlements-hydrate` row in the
[CLI reference](../../reference/cli/#run-inspection).

## What this is not

- **Not a fleet.** A replica works only while someone runs
  `orbit run auto --pull` on it; a clean probe starts nothing.
- **Not auto-recovery.** A dead replica holds its claims until you recover
  them on the owner.
- **Not follower merge.** Replicas never merge; landing is the owner's.

The [distributed drain runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/distributed-drain.md)
covers protocol details, receipts, and every refusal. For a drain on one
machine, see [Run a Delivery Window](../continuous-delivery/).
