---
title: Set Up a Distributed Drain
description: "Run a second machine as a replica that pulls work from one owner. Installation is not rollout; the owner keeps landing authority; recovery is manual."
sidebar:
  order: 4
---

Use this guide when you want a second machine to execute the same Orbit
workspace. One host owns the task store. Other hosts register as replicas,
reach that owner over SSH, and run `orbit run auto --pull`: the owner orders
the work and admits one claim at a time, and each claim runs on the replica
until it hands a pull request back. Matching binaries and a working preflight
are **installation**; starting the pull drain is the rollout.

Automatic reclamation, automatic review, and follower merges are not
available. The owner approves every handoff before it lands.

## 1. Keep one owner

A repository checkout, a logical workspace, and a machine's live task store
are different things. Sharing Git history does not synchronize Orbit.

On the host that will stay authoritative:

```bash
orbit --version
orbit config get machine.id
orbit workspace init --role owner --base-branch <integration-branch>
orbit workspace show
```

On a second machine, pick a **different** task prefix at `orbit init`, then
register the checkout as a replica of the owner's machine id:

```bash
orbit init --non-interactive --machine-name <name> --task-prefix <PREFIX>
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica
```

`workspace role` reasserts the role; it is not a takeover. If both hosts were
already independent owners of the same repo, stop their drains, finish or
export leftover work, then re-register. Move tasks with `orbit task export` /
`orbit task import` — do not copy the database.

## 2. Match binaries, crews, and review policy

Every participant must run the same Orbit version, resolve the same crews and
toolchains, and use review policy `none`:

```bash
orbit --version
orbit doctor
orbit config get operation.review_policy
```

v1 refuses `before-pr` and `after-landing` for distributed admission rather
than silently downgrading them. Empty required-validation configuration is
fail-closed for a claimed handoff. Compiler caches and build slots stay
per host; they are not a shared fleet.

## 3. SSH login is owner access

There is no destination callers file and no per-caller identity registry.
Whoever can `ssh` to the owner already owns that machine. Start federation
from the follower with the authority you intend:

```bash
orbit mcp init --federated --client <client>
orbit mcp serve --mode federated --operator
```

Without `--operator` on the calling side, remote sessions hold `agent`.
A managed agent session cannot promote itself. To deny a caller, remove its
key from the owner's `~/.ssh/authorized_keys`. Leftover
`~/.orbit/mcp-callers.toml` files are ignored; `orbit doctor` names them so
you can delete them.

See [Connect Your Agent](../mcp-integration/) for client registration. Machine labels
forwarded over SSH are audit attribution, not credentials.

## 4. Probe the owner

From a session that reaches the **owner**:

```bash
orbit tool run orbit.drain.probe --input '{
  "caller_version": "<this-binary-version>",
  "caller_schema": 1,
  "caller_review_policy": "none"
}'
```

The probe reports the owner, binary and protocol versions, this session's
capabilities, ship configuration, and review policy. If you declare your
version and policy, it also reports the first refusal a real admission would
raise. It creates no task, reservation, or claim. Treat it as a preflight,
not a health check — and never call pull itself as one, since a pull is a real
admission.

Reconcile a past request the same way — still read-only:

```bash
orbit tool run orbit.drain.receipt.lookup --input '{"request_id":"<request-id>"}'
```

`not_found` does not mean you may mint a replacement request id.

## 5. Start the pull drain

On the replica, with the owner in `~/.orbit/mcp-destinations.toml` and the
same `workflow.required_validation_commands` the owner declares:

```bash
orbit run auto --pull <owner-machine>/<ws_id> --for 8h --concurrency 3
```

The selector is the owner's `selector` from federated discovery. The command
refuses before submitting anything unless this checkout is a replica of that
owner and workspace and the owner's probe admits this executor. Each claim
runs as a local leaf that implements, validates, pushes and opens a pull
request, then hands off; the owner reads the pull request itself and moves the
task to `review`. Approve it on the owner's dashboard to land it.

The drain keeps settling its claims after the window closes. Stop it early
with `orbit run auto --stop`; running leaves finish and still hand off.

## 6. Inspect claims; recover by hand

List claims on the owner (operator shell):

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
```

Age, an expired reservation, and a missing local run are diagnostics. Nothing
in that listing reclaims work. `orbit job resume` refuses a claimed leaf;
deliberate recovery inspects the recorded run, reconciles any uncertain
merge, and only then revokes the old attempt. Followers never merge. Review
status is not an automated review, and no heartbeat reassigns a dead worker.

## 7. Leave schedules as they are

Seeded ship-sweep routines, `workspace_ship_pipeline`, and
`orbit run ship-sweep` stay at whatever enablement you already chose.
Distributed-drain setup does not opt them in.

```bash
orbit routine list
orbit run ship-sweep --dry-run
```

Enable a routine only by editing its versioned definition. Replica hosts
refuse owner-only sweeps.

## What this is not

- **Not a fleet.** There is no host list to enable.
- **Not auto-recovery.** A dead replica can hold a footprint until you
  inspect and revoke it.
- **Not follower merge.** Landing stays on the owner after explicit
  completion authority.
- **Not automatic.** A clean probe means the hosts are installed. A replica
  pulls only while an operator-started `orbit run auto --pull` is running.

For ordinary single-host drains, stay on
[Run Continuous Delivery](../continuous-delivery/).
