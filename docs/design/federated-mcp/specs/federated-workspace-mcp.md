---
type: design
summary: "Spec: Federated workspace MCP mux, selector, capabilities, list schema, and fail-closed routing"
last_validated: 2026-08-29
title: Spec — Federated workspace MCP
owner: grok
status: Draft
feature: federated-mcp
tags: [federated-mcp, mcp, spec]
related_features: [federated-mcp, host-registry, mcp-bridge]
related_artifacts: [ORB-11044, ORB-11023, ORB-11017, ORB-11015, ORB-11014, ORB-11013, ORB-11010, ORB-11009, ORB-11008]
---

# Spec: Federated workspace MCP

The federated MCP surface is a **mux of operator-configured destinations**. It presents one caller-facing MCP namespace, lists every configured destination's workspaces as live descriptors, and routes a structured, caller-uninterpreted host-qualified selector to the encoded destination. It is **not** a host-registry evolution, **not** a fleet inventory, and **not** automatic owner discovery. Direct SSH stdio to one chosen host remains v1. The federated list is implemented in [ORB-11014]; fail-closed routing of copied `hm_*/ws_*` selectors is implemented in [ORB-11015]; federated `tools/list` workspace-param advertisement and `orbit.task.show` requiring that selector are implemented in [ORB-11017].

## Why This Exists

Without this contract, an implementation will key selectors on renameable `host_id`, invent a second ownership model, lump runs and task issuance as one "mutations" blob, omit down hosts from the list, or violate mcp-bridge's no-relay rule (or ship a mux the corpus still forbids). [ORB-11008] recorded the policy; [ORB-11009] (PR #1139) opened this spec; [ORB-11010] closes the contract holes that review named.

## Mux, not fleet registry

1. Remote destinations are operator-configured SSH remotes. The gateway does not register, retire, or enumerate a fleet of machines as host-registry records.
2. The accepting machine is an implicit local destination, using its existing stable `machine_id` and workspace registry. That is this host's own membership, not fleet discovery.
3. The gateway does not auto-discover the owner checkout of a repository and does not perform placement.
4. The gateway must not reinterpret a selector against its own local catalog. A copied `hm_*/ws_*` selector is delivered to the destination encoded in the token: the local in-process host when that destination is this machine, otherwise the configured SSH remote.
5. A caller that chooses one host and speaks v1 MCP (local stdio, direct SSH stdio, or `orbit mcp listen`) never enters this mux.

## Destination membership file

Remote federated membership is declared only in the machine-global operator file `~/.orbit/mcp-destinations.toml`. It is not part of workspace `config.toml`, `workspaces.json`, or host-registry. Local workspaces require no destination row: federated serve always includes the accepting machine. A missing file or an empty `destinations` list is a valid local-only configuration.

The v1 file shape is an array of additional SSH remotes:

```toml
[[destinations]]
ssh = "orbit-linux"
machine_id = "hm_alpha"

[[destinations]]
ssh = "operator@orbit-build"
machine_id = "hm_beta"
```

Each configured row has exactly two required keys: `ssh`, an SSH alias or `user@host` transport target, and `machine_id`, the destination's stable `hm_…` identity. A machine-id-only row is invalid and fails closed at config load with an actionable `invalid_input` (missing `ssh`). TCP/MCP destination rows are not a v1 file variant. A duplicate `machine_id` makes the entire file invalid with `ambiguous_destination` during config load, before the gateway advertises tools or accepts any `tools/call`.

If a valid configured row already names the accepting machine's `machine_id`, the mux exposes exactly one route for that machine — the implicit local in-process destination — rather than duplicating selectors or opening loopback SSH.

## Selector identity

1. The host-qualified selector is **structured, caller-uninterpreted**. Encoding `hm_<id>/ws_*` is normative (example: `hm_<id>/ws_orbit`). The stable key is `machine_id` (`hm_…`), not renameable `host_id`.
2. Callers must not parse the token and must not construct it from `host_id` or by concatenating remembered identifiers. The only caller-facing way to obtain a selector is to copy the `selector` field from federated `orbit_workspace_list`.
3. Display names such as `orbit-linux/ws_orbit` are not selectors.
4. The selector is addressing data, not a path, URL, logical-only workspace ID, or authorization credential. Possession of a selector is not authorization.
5. Every workspace-scoped federated tool accepts the selector. The gateway routes that call to the encoded destination. Federated `tools/list` advertises that callers must copy `selector` from federated `orbit.workspace.list` and must not treat cwd, a registered name, or a bare `ws_*` as valid. Federated `orbit.task.show` requires the host-qualified selector and does not inherit the v1 id-only default.
6. A token that is not uniquely host-qualified (a bare `ws_*`, a display host name, a v1 session-defaulted `ws_*`, or any other form that does not match the normative encoding) is `unknown_selector` **before forwarding**, not `ambiguous_destination`.
7. Duplicate `machine_id` across configured destinations is a **config-load** `ambiguous_destination`. The mux must not treat that collision as a per-call routing outcome.
8. Federated serve does not take `--workspace ws_*`. A bound session, if any, may only hold a host-qualified selector. v1 `orbit mcp serve --workspace` and the v1 `tools/list` snapshot stay unchanged.

## Tool classes

Capability class is assigned by **what the tool does**, not by a per-tool registry field and not by naming `orbit_task_add` alone.

| Class | Rule | Examples (not an exhaustive registry) |
|---|---|---|
| `control_plane` | Task issuance and coordination-store writes | `orbit_task_add`, `orbit.task.update` |
| `execute` | Anything that touches runs, logs, or scheduler state | job-run inspect/cancel, log read, routine/scheduler mutations |
| unclassified | Discovery and list tools | `orbit_workspace_list` (federated), other list/discovery tools |

Unclassified tools are **not** subject to `capability_refused`. A destination may still fail them for other routing classes (`unknown_selector`, `unreachable_destination`, `tool_not_on_this_host`, and so on).

## Capabilities vs checkout roles

Capabilities distinguish at least `control_plane` and `execute`. They map onto existing host-registry catalog roles; do not invent a parallel ownership vocabulary.

**The destination's local catalog role determines capability class.** Federated-list advertisement is a hint and may lag the destination. Destination Core refusal remains the correctness boundary. The gateway does **not** enforce by rewriting the destination, and "typically also `execute`" is not a refusal input.

| Catalog role (v1) | Classes the destination holds | Destination Core |
|---|---|---|
| Owner checkout | `control_plane`. May also hold `execute` when that checkout runs locally; that second class is independent of the owner role. | Control-plane authority for that workspace's coordination. Refuses `execute`-class tools only when it does not hold `execute` (for example a future control-plane-only store). |
| Replica checkout | `execute` | Execution binding. Refuses `control_plane` tools. |

A workspace whose logical record has absent `owner_machine_id` (`Option` on `Workspace` in `crates/orbit-types/src/workspace/registry.rs`) **cannot advertise `control_plane`**. Standalone registries that predate host identity may omit the field; they are not a control-plane authority.

1. Destination-host Core **refuses the other class** with named error `capability_refused`.
2. The gateway may advertise capabilities. Advertisement is not authorization and is not a second enforcement layer.
3. If a caller routes a `control_plane` tool (`orbit_task_add`, `orbit.task.update`, or any other coordination-store write) to an execution-binding host, the destination refuses with `capability_refused`. The gateway **must not** silently send it to the owner. No implicit failover.

## Split authority ("mutations" is not one blob)

Routing delivers a call; it does not move authority.

| Concern | Authority |
|---|---|
| Runs, logs, scheduler state | **Destination host** that received the call |
| Task issuance and the coordination store | **Declared control-plane authority** for that workspace (the owner checkout today; may later be cloud-offloaded) |

Do not specify a single "mutations" permission that covers both columns. A replica may accept execute-class work against its own runs/logs/scheduler and must still refuse task issuance.

Federated `orbit_workspace_list` is an aggregate of live descriptors, not an aggregate task or store query.

## Single control-plane per repository is operator configuration

A single control-plane per repository is an **operator configuration responsibility**, not a mux invariant. The mux does **not** detect competing owners and does **not** raise an error when two destinations each declare Owner.

The would-be signal is matching `git_remote` across destinations with differing `owner_machine_id`. Independently inited checkouts also have different `ws_*`, so the mux cannot observe the collision without the fleet discovery this design forbids. A violation surfaces as **two independent control planes**, not as a routing or config error.

## Federated `orbit_workspace_list`

Federated `orbit_workspace_list` is a **new session-unbound response shape**, not a compatible extension of v1 `orbit.workspace.list`.

Implemented in [ORB-11014] as `orbit mcp serve --mode federated`
(`crates/orbit-mcp/src/federated/`), with implicit local membership in
[ORB-11044]. Remote membership is loaded once at startup from the destinations
file; the accepting machine is always prepended. Every list call then probes
each destination live — local in-process, remotes over the v1 SSH argv — and
caches nothing. The response envelope is `{"workspaces": [...]}` — no envelope
`machine_id`. After [ORB-11015] the mux advertises the canonical 23-tool
surface: this list stays session-unbound and answered by the mux, and every
workspace-scoped tool is delivered to the destination encoded in the copied
selector.

v1 (`crates/orbit-mcp/src/remote/discovery.rs`) returns `{"machine_id": "hm_…", "workspaces": […]}` with `machine_id` on the **envelope**, and filters to `Active` workspaces that are locally checked out on the accepting machine.

Federated list does **not** inherit that envelope or that filter:

1. **Session-unbound.** The tool must not require a workspace selector. Session/initialize workspace metadata is not an input and not a filter.
2. **`machine_id` on each descriptor**, not on the envelope. Each descriptor keeps today's v1 workspace fields (`id`, `name`, `ship_mode`, `owner_machine_id`, `git_remote`, `base_branch`, `status`, `created_at`, `updated_at`) and adds:

   | Field | Meaning |
   |---|---|
   | `selector` | Structured, caller-uninterpreted host-qualified route token (`hm_<id>/ws_*`). Copy this field; do not parse it. |
   | `host` | Destination display identity (renameable `host_id`; display only) |
   | `machine_id` | Destination stable identity (`hm_…`) |
   | `reachability` | Whether the configured destination answers: `reachable` or `unreachable` |
   | `checkout_health` | Repo-root presence at that destination: `active`, `invalid`, or `unknown` if the host cannot be probed |
   | `capabilities` | Classes the destination currently **advertises** for that workspace (a hint; see Capabilities vs checkout roles) |

   `host` is the accepting machine's `host_id` for the implicit local destination. For configured remotes it is the operator's `ssh` target: the v1 discovery envelope carries no `host_id`, so that alias is the only display identity the mux can honestly attribute to a remote.

   The federated-only keys are exactly `selector`, `host`, `machine_id`, `reachability`, `checkout_health`, and `capabilities`. `capabilities` is an array whose values are `control_plane` and/or `execute`. These names are protocol keys; implementations must not substitute a combined `health` key or the prose labels used to describe them.

3. **Do not overload one `health` field** with SSH/MCP reachability and repo-root presence.
4. **Include unreachable and inactive destinations.** Configured workspaces on unreachable or inactive destinations are included, not omitted. A down destination appears with an explicit unreachable (and, if checkout cannot be probed, unknown/unhealthy) projection. Omission makes every later call a stale-route surprise.

   Every configured destination therefore contributes at least one row. A destination that fails, times out, or answers under a `machine_id` other than its config pin yields one row with `reachability=unreachable`, `checkout_health=unknown`, `selector=null`, and no v1 workspace fields — there is no observed workspace to name, and inventing one would let a caller address a route that was never seen. A destination that answers with no workspaces yields the same row shape with `reachability=reachable`, which is a different fact from silence.
5. v1 `orbit.workspace.list` on a single accepting machine is unchanged: it remains machine-local, envelope-keyed, and Active-and-locally-checked-out, and is documented in mcp-bridge / host-registry current-behavior docs.

## Fail-closed routing

The gateway delivers a workspace-scoped federated call to the destination encoded in the selector. It does not fall back to a local workspace, another host with a matching `ws_*`, a default workspace, or a cached host-local runtime.

Routing decides on **live delivery**, not cached list health. Probe cadence and list freshness remain a vision open question; they do not change which error a live call returns.

| Class | Error identity | When |
|---|---|---|
| unknown | `unknown_selector` | Token never valid: it does not uniquely name a configured destination+workspace, including a token that is not uniquely host-qualified (bare `ws_*`) |
| ambiguous | `ambiguous_destination` | Duplicate `machine_id` among configured destinations, raised at **config load**, not per call |
| unreachable | `unreachable_destination` | Configured destination does not answer, up to and including a `tools/call` request that could not be written |
| stale-route | `stale_route` | Destination is configured; a live probe shows that workspace is absent |
| unhealthy | `unhealthy_checkout` | Destination answers but the checkout is not usable (repo-root missing / invalid) |
| tool not advertised | `tool_not_on_this_host` | Destination is identified; the tool is not on that host's advertised surface |
| capability refuse | `capability_refused` | Destination Core holds the workspace but refuses the tool's capability class |

`stale_route` vs `unknown_selector`: destination configured but workspace absent after a live probe → `stale_route`; selector never valid → `unknown_selector`.

`tool_not_on_this_host` is distinct from `unknown_selector`. `capability_refused` is distinct from both: the tool may be advertised elsewhere on that host, but this checkout role will not execute it.

### Caller-facing precedence

When more than one class could apply, return the first that matches:

`unknown_selector` → `ambiguous_destination` (config) → `unreachable_destination` → `stale_route` → `unhealthy_checkout` → `tool_not_on_this_host` → `capability_refused`

Unreachable wins over capability and stale because those are undecidable without the host. Cached list health must not reorder this list.

### Delivery budget and post-dispatch outcome

Classification and delivery are budgeted separately [ORB-11023]. SSH setup, the handshake, discovery, and `tools/list` share one probe budget; the routed `tools/call` is stamped with its own, larger budget when its request is written. Routed tools include long-running mutating ones, so the time spent choosing a destination must not be deducted from the time the tool gets to run.

Once that request is written the call may already have executed and committed on the destination, and killing the transport does not undo it. A lost answer there is therefore `outcome_unknown`, never `unreachable_destination`: the latter means a delivery miss and invites the retry that would duplicate the write. This is a post-dispatch outcome and does **not** enter the precedence ladder above — everything in that ladder is decided before the destination sees the call.

| Class | Error identity | When |
|---|---|---|
| outcome unknown | `outcome_unknown` | The routed `tools/call` request was written and its answer never arrived (budget exceeded, or the session ended mid-call) |

## mcp-bridge invariant exception

The mux replaces v1 "byte-transparent / no Orbit process relays onward" **for the federated namespace only**. v1 current-behavior text in [mcp-bridge 2_design.md](../../mcp-bridge/2_design.md) stays. `orbit mcp serve --mode remote` is unchanged and remains byte-transparent; the federated mux is a separate mode that speaks MCP as a client, reusing that same `ssh -T -- <host> orbit mcp serve --remote-caller-machine-id …` argv so a destination sees a session shape it already supports.

Still excluded (not implied by the exception):

- automatic owner discovery;
- replication of tasks or stores;
- relays-as-product (a general Orbit relay outside the federated namespace);
- fleet placement.

See [mcp-bridge 3_vision.md §5](../../mcp-bridge/3_vision.md).

## Non-goals

The following remain out of this surface:

- task or store replication;
- synchronization;
- quorum election;
- detecting or rejecting competing control-plane authorities (operator configuration; see above);
- implicit failover;
- silent merging of host-local state.

A disconnected or failed host therefore removes the affected route from useful service. Another host cannot answer in its place unless a separate, explicit authority design — not this mux — says so.

## Agent Signature

Specified by grok in [ORB-11009] (PR #1139), with contract holes closed in [ORB-11010], citing prior policy [ORB-11008]. Destination config, selector, and error identities implemented in [ORB-11013]; the federated list implemented by claude in [ORB-11014]; fail-closed routing of host-qualified selectors implemented in [ORB-11015]; federated workspace-param advertisement implemented in [ORB-11017]; implicit local membership implemented in [ORB-11044].
