---
title: Set Up a Distributed Drain
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
  distributed-drain protocol revision as the owner and declares the same
  `workflow.required_validation_commands`. If the owner turns on before-PR
  review (`review.before_pr = true`), every replica must be able to run the
  owner's `operation.review_crew`: each pulled task is then reviewed, and the
  reviewer's fixes committed, on the replica before its pull request opens.
  Crews may differ: a replica only receives tasks whose crew it can run. Operating systems may differ too: a
  task tagged `os:macos` (or `os:linux`, `os:windows`) only goes to a machine
  running that OS, and waits in the owner's backlog until one asks for work.
- **SSH is the access control.** Anyone who can `ssh` to the owner owns it. To
  shut a replica out, remove its key from the owner's `authorized_keys`.
- **Landing stays on the owner.** Approve each handoff on the owner's
  dashboard, or set `workflow.distributed_completion = "done"` on the owner to
  land every validated handoff, the way `--complete` lands its own tasks.

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
# The owner's value decides. With it on, the replica must
# be able to run this crew.
orbit config get review.before_pr
orbit config get operation.review_crew
orbit doctor
```

Then probe the owner. On the owner, run this read-only check; it reports the
first reason a real pull would be refused. Read `protocol_schema` from the
installed binary instead of copying a revision from this page. Use that value
for `caller_schema` only after confirming the replica runs the matching
build; it is the distributed-drain revision, not the MCP protocol revision.
Replace `<replica-version>` with the replica's `orbit --version` value and
set `caller_before_pr` to its `review.before_pr` value:

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
crews and why the others were left out. A provider that turns out not to be
signed in sends its task back to `backlog` and is dropped for the rest of the
drain.

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
task and use **Recover claim → blocked** or **Recover claim → backlog**; both
ask for a reason.

## What this is not

- **Not a fleet.** A replica works only while someone runs
  `orbit run auto --pull` on it; a clean probe starts nothing.
- **Not auto-recovery.** A dead replica holds its claims until you recover
  them on the owner.
- **Not follower merge.** Replicas never merge; landing is the owner's.

The [distributed drain runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/distributed-drain.md)
covers protocol details, receipts, and every refusal. For a drain on one
machine, see [Run a Delivery Window](../continuous-delivery/).
