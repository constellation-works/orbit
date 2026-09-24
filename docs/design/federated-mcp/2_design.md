---
title: Federated MCP — Design
owner: grok
last_updated: 2026-09-24
last_validated: 2026-09-24
status: Draft
feature: federated-mcp
doc_role: design
type: design
summary: Federated MCP mux, selector, capability split, list schema, and fail-closed routing. V1 current behavior stays in mcp-bridge.
tags: [federated-mcp, mcp, host-registry, multi-host]
paths: ["crates/orbit-mcp/**", "crates/orbit-registry/**", "crates/orbit-core/**"]
related_features: [federated-mcp, host-registry, mcp-bridge, remote-access, mcp-session-context]
related_artifacts: [ORB-11184, ORB-11044, ORB-11023, ORB-11016, ORB-11017, ORB-11015, ORB-11014, ORB-11013, ORB-11012, ORB-11011, ORB-11010, ORB-11009, ORB-11008]
---

# Federated MCP — Design

This document describes the federated MCP surface. Current v1 behavior — one chosen destination, byte-transparent direct SSH stdio, no Orbit process relaying onward — remains [mcp-bridge 2_design.md](../mcp-bridge/2_design.md) and is unchanged by `--mode remote`. The federated mux is a separate `--mode federated` server: list in [ORB-11014], routing in [ORB-11015].

The prescriptive invariants live in [specs/federated-workspace-mcp.md](./specs/federated-workspace-mcp.md). This file explains how those pieces fit.

## 1. Operator-configured remotes, implicit local destination

The shipped gateway is a mux in front of the accepting machine plus the SSH stdio remotes the operator configured in `~/.orbit/mcp-destinations.toml`. Local workspaces need no destination row; a missing or empty file is a useful local-only federated server. An explicit SSH row that names this machine's `machine_id` is collapsed to the single in-process local route. The mux does not:

- grow host-registry into a fleet inventory;
- auto-discover the owner checkout of a repository;
- place work, elect a leader, or pick a healthy substitute;
- detect or reject competing control-plane authorities (see §7).

Direct SSH stdio to one chosen host remains the v1 remote path and is unchanged by the mux. A caller that does not use the federated namespace never hits it.

## 2. Host-qualified selector

Every workspace-scoped federated call carries a **structured, caller-uninterpreted** host-qualified selector. Encoding `hm_<id>/ws_*` is normative. The token is addressing data. Callers copy the `selector` field from federated `orbit_workspace_list`; they must not parse it, construct it from `host_id`, or concatenate remembered identifiers. The gateway must not reinterpret it against its own local catalog.

The stable key is `machine_id` (`hm_…`), not renameable `host_id`. Example form: `hm_<id>/ws_orbit`. A display host name such as `orbit-linux/ws_orbit` is not a valid selector.

A token that is not uniquely host-qualified (a bare `ws_*`, including a v1 session-defaulted form) is `unknown_selector` before the mux opens a destination session. Duplicate `machine_id` across configured destinations is config-load `ambiguous_destination`, not a per-call routing outcome.

Federated `tools/list` advertises that callers copy `selector` from federated `orbit.workspace.list`. It does not present cwd, a registered name, or a bare `ws_*` as valid. Federated `orbit.task.show` requires the host-qualified selector; the v1 id-only default does not apply in this namespace. `orbit mcp serve --mode federated` does not take `--workspace ws_*`. v1 bound sessions (`orbit mcp serve --workspace ws_orbit`) and the v1 `tools/list` snapshot stay unchanged.

## 3. Capabilities mapped onto owner and replica

This feature does not invent a second ownership model. Host-registry already distinguishes:

- **owner checkout** — local checkout whose logical `owner_machine_id` equals this machine;
- **replica checkout** — local checkout that names another machine as the logical owner.

**The destination's local catalog role determines capability class.** List advertisement is a hint and may lag destination Core. Destination Core refusal is the correctness boundary; the gateway is not a second authorization layer.

| Catalog role | Classes held | Meaning |
|---|---|---|
| Owner checkout | `control_plane`. May also hold `execute` when that checkout runs locally; that second class is independent of the owner role and is not a refusal input. | Control-plane authority for that workspace's coordination |
| Replica checkout | `execute` | Execution binding only |

A workspace with absent `owner_machine_id` cannot advertise `control_plane`.

A destination Core that does not hold a class **refuses** tools of that class with a named error (`capability_refused`). The gateway may advertise the destination's capabilities; it does not enforce by rewriting the destination or failing over to the owner.

Tool class is assigned by what the tool does, not by a per-tool registry field:

- task issuance, coordination-store writes, and task reads (`orbit_task_add`, `orbit.task.update`, `orbit.task.list`, `orbit.task.show`, …) → `control_plane`
- anything touching runs, logs, or scheduler state → `execute`
- discovery / list tools (`orbit_workspace_list`, …) → unclassified, not subject to `capability_refused`

If a caller routes a `control_plane` tool to an execution-binding host, that destination refuses. The gateway must not silently send the call to the owner.

## 4. Split authority — "mutations" is not one blob

Routing changes where a call is delivered; it does not move authority.

- **Destination host** is authoritative for **runs, logs, and scheduler state** on that host.
- **Declared control-plane authority** is authoritative for **task issuance and the coordination store**. That store may later be cloud-offloaded; that is an open question, not a hidden replica protocol.

Do not lump those as one "mutations" blob. A replica can accept execute-class work against its own run/log/scheduler state and must still refuse control-plane task issuance.

`orbit_workspace_list` (federated) is an aggregate of live descriptors, not an aggregate task or store query.

## 5. Federated workspace list

Federated `orbit_workspace_list` is **session-unbound**: it must not require a workspace selector. It is a **new response shape**, not a compatible extension of v1 `orbit.workspace.list`.

v1 puts `machine_id` on the envelope (`{"machine_id", "workspaces":[…]}`) and filters to Active workspaces that are locally checked out. Federated list puts `machine_id` on **each descriptor** and does **not** inherit that filter. Configured workspaces on unreachable or inactive destinations are included, not omitted.

Each descriptor keeps today's v1 workspace fields (`id`, `name`, `ship_mode`, `owner_machine_id`, `git_remote`, `base_branch`, `status`, timestamps) plus:

- `selector` — structured, caller-uninterpreted host-qualified route token (`hm_<id>/ws_*`);
- `machine_name` — destination display identity (the local machine name, or the remote's configured SSH target);
- `machine_id` — destination stable identity;
- `reachability` — SSH/MCP reachability of the configured destination;
- `checkout_health` — repo-root presence at that destination, or `unknown` when it could not be probed;
- `capabilities` — classes the destination currently advertises for that workspace (a hint).

Do not overload one `health` field with both SSH reachability and repo-root presence. A down or unreachable host is **included** with an explicit unreachable/unhealthy projection, not omitted. Omission makes every later call a stale-route surprise.

## 6. Fail-closed routing

The gateway delivers a workspace-scoped call to the destination encoded in the selector. Routing decides on **live delivery**, not cached list health.

Caller-facing precedence when more than one class could apply:

`unknown_selector` → `ambiguous_destination` (config load) → `unreachable_destination` → `stale_route` → `unhealthy_checkout` → `tool_not_on_this_host` → `capability_refused`

- `unknown_selector` — token never valid, including a bare `ws_*`.
- `ambiguous_destination` — duplicate configured `machine_id`, raised at config load, not per call.
- `unreachable_destination` — configured destination does not answer. Unreachable wins over capability and stale because those are undecidable without the host.
- `stale_route` — destination configured; a live probe shows the workspace is absent.
- `unhealthy_checkout` — destination answers but the checkout is not usable.
- `tool_not_on_this_host` — destination identified; the tool is not advertised there.
- `capability_refused` — destination holds the workspace but refuses the tool's class. Unclassified discovery/list tools are not subject to this error.

It must not fall back to a local workspace, another host with a matching `ws_*`, a default workspace, or a cached host-local runtime.

### Delivery budget and post-dispatch ambiguity

The precedence above covers everything decidable **before** the destination sees the call. Two rules cover what happens after [ORB-11023].

**Two budgets, not one.** SSH setup, the MCP handshake, discovery, and `tools/list` share one probe budget, because a caller cannot tell those phases apart. The routed `tools/call` gets its own, larger budget, stamped when its request is written. A single session-wide deadline would spend the tool's execution time on the round trips that merely chose the destination, and would cap every routed tool — including `orbit.command.exec` and `orbit.workflow.ship` — at whatever classification left over.

**A lost answer after dispatch is `outcome_unknown`, not `unreachable_destination`.** Once the request is on the wire the destination may have run and committed it, and killing the SSH child does not undo remote work. `unreachable_destination` means a delivery miss, which invites a retry; retrying a possibly-committed `orbit.task.add` or `orbit.workflow.ship` duplicates it. A loss *before* the request is written — including a failed write — is still `unreachable_destination`, because nothing was delivered. `outcome_unknown` is a post-dispatch outcome and does not enter the precedence ladder.

## 7. Operator-configured control-plane uniqueness

A single control-plane per repository is an operator configuration responsibility, not a mux invariant. The mux does not check it. The would-be signal is matching `git_remote` across destinations with differing `owner_machine_id`. Independently inited checkouts have different `ws_*`, so the mux cannot observe the collision without fleet discovery. A violation surfaces as two independent control planes, not an error.

## 8. Relationship to v1 MCP

v1 local stdio, direct SSH stdio, and `orbit mcp listen` stay as specified in mcp-bridge. The mux is an explicit exception to v1 "byte-transparent / no Orbit process relays onward", **for the federated namespace only**. Automatic owner discovery, replication, relays-as-product, and fleet placement stay out. See [mcp-bridge 3_vision.md §5](../mcp-bridge/3_vision.md).

## 9. Concerns & Honest Limitations

- v1 `--mode remote` is unchanged. Federated list and routing live only in `--mode federated`.
- The mux performs no placement and does not detect competing Owner declarations. Those remain operator configuration responsibilities.
- Availability is bounded by the chosen destination. Callers must handle visible routing failures; there is no transparent substitute result.
- Task reads remain owner-only because the coordination store is owner-authoritative. Use the owner selector for `orbit.task.list` and `orbit.task.show`; a replica selector receives `capability_refused`.
- Including unreachable hosts makes the list honest and larger; clients must read reachability rather than treating presence as liveness.
- Capability advertisement can lag destination Core. The destination refuse is the correctness boundary; the gateway is not a second authorization layer.
- Session authority (`agent` / `operator`) is resolved from argv at server start, for a remote-originated session exactly as for a local one [ORB-12564]. A federated or remote-proxy client started with `--operator` emits `orbit mcp serve --operator --remote-caller-machine-id <id>` for every destination it opens, and the destination serves that authority: Orbit is a single-user tool, and an SSH login to a destination is ownership of it, so a second authorization statement on the far side would sit in a file the caller can rewrite. This is a different axis from the capability classes above; a call must clear both, and those classes stay destination-derived because they describe the *checkout*. The caller-side guard is the one that matters: a client that declares itself an agent (managed run, agent envelope) never propagates `--operator`, and `child_env` strips `ORBIT_OPERATOR` from agent children. `--remote-caller-machine-id` remains an audit label: it marks the transport as `ssh-mcp` and names the calling machine in the destination's `authorization` rows, and authorizes nothing. A destination carrying a leftover `~/.orbit/mcp-callers.toml` or `~/.orbit/mcp-ssh-acceptance/` ignores it with one startup warning and an `orbit doctor` row. See [the decision](./4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it). The removed caller-authorization spec is in git history.
- Transport authentication, selector expiry, health freshness, and cloud coordination-store details are deliberately unresolved. See [3_vision.md](./3_vision.md). Probe cadence stays a vision open question; it does not change live-delivery error precedence.
- Mixed Orbit versions across destinations can advertise different surfaces; `tool_not_on_this_host` and `capability_refused` must remain distinguishable from "the mux is confused."
- Two destinations that each declare Owner for the same `git_remote` are a configuration mistake the mux will serve as two control planes.

## Task References

- [ORB-11008] — recorded the federated multi-host MCP policy
- [ORB-11009] — specified the implementable mux contract (PR #1139)
- [ORB-11010] — closed the PR #1139 review contract holes in this folder
- [ORB-11011] — sequenced the shipped federated MCP mux
- [ORB-11012] — mapped destination checkout role to `capability_refused`
- [ORB-11013] — implemented destination config, selectors, and routing errors
- [ORB-11014] — federated `orbit.workspace.list`
- [ORB-11015] — fail-closed routing of host-qualified selectors
- [ORB-11016] — registered the federated serve path and aligned current docs
- [ORB-11017] — federated workspace param is the host-qualified selector
- [ORB-11044] — implicit local membership: local workspaces listed and routed without SSH

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
