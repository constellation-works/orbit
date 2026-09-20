---
title: Set Up a Distributed Drain
description: "Prepare one owner and replica checkouts for a future pull-based drain. Installation is not rollout; recovery is manual."
sidebar:
  order: 4
---

Use this guide when you want a second machine to execute the same Orbit
workspace later. One host owns the task store. Other hosts register as
replicas and reach that owner over SSH. Matching binaries and a working
preflight are **installation**. They do not turn pull on, enable a fleet, run
a reviewer, or merge from a follower.

Public pull, `orbit run auto --pull`, automatic reclamation, automatic review,
and follower merges are not available. Do not look for dashboard controls for
those; they are not shipped.

## 1. Keep one owner

A repository checkout, a logical workspace, and a machine's live task store
are different things. Sharing Git history does not synchronize Orbit.

On the host that will stay authoritative:

```bash
orbit --version
orbit host show
orbit workspace init --role owner --base-branch <integration-branch>
orbit workspace show
```

On a second machine, pick a **different** task prefix at `orbit init`, then
register the checkout as a replica of the owner's machine id:

```bash
orbit init --non-interactive --host-name <name> --task-prefix <PREFIX>
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

See [Set Up MCP](../mcp-integration/) for client registration. Machine labels
forwarded over SSH are audit attribution, not credentials.

## 4. Probe the owner, do not pull

The live surface is read-only. From a session that reaches the **owner**:

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
not a health check that "enables" pull.

Reconcile a past request the same way — still read-only:

```bash
orbit tool run orbit.drain.receipt.lookup --input '{"request_id":"<request-id>"}'
```

`not_found` does not mean you may mint a replacement request id.

## 5. Inspect claims; recover by hand

List claims on the owner (operator shell):

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
```

Age, an expired reservation, and a missing local run are diagnostics. Nothing
in that listing reclaims work. `orbit job resume` refuses a claimed leaf;
deliberate recovery inspects the recorded run, reconciles any uncertain
merge, and only then revokes the old attempt. Followers never merge. Review
status is not an automated review, and no heartbeat reassigns a dead worker.

## 6. Leave schedules as they are

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
- **Not pull, yet.** A clean probe means the hosts are installed. Turning
  pull on is a later, explicit operator step once that surface ships.

For ordinary single-host drains, stay on
[Run Continuous Delivery](../continuous-delivery/).
