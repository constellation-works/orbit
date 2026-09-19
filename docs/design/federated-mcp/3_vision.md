---
title: Federated MCP — Vision
owner: grok
last_updated: 2026-09-04
last_validated: 2026-09-04
status: Draft
feature: federated-mcp
doc_role: vision
type: design
summary: Open questions for the proposed federated MCP mux: selector expiry, health freshness, and cloud coordination-store details. Transport authentication is resolved.
tags: [federated-mcp, mcp, host-registry, multi-host]
paths: ["crates/orbit-mcp/**", "crates/orbit-registry/**", "crates/orbit-core/**"]
related_features: [federated-mcp, host-registry, mcp-bridge, remote-access, mcp-session-context]
related_artifacts: [ORB-11184, ORB-11053, ORB-11052, ORB-11010, ORB-11009, ORB-11008]
---

# Federated MCP — Vision

Forward-looking only. The contract in [specs/federated-workspace-mcp.md](./specs/federated-workspace-mcp.md) is proposed and not shipped. These questions must not be fake-resolved in the spec; an implementation task answers them with evidence.

## 1. Open Questions

1. **Transport authentication.** ~~What authenticated principal does a destination Core receive on a mux-forwarded call?~~ **Resolved, then withdrawn as a question** by [ORB-12564]. The two-tier answer ([ORB-11052], [ORB-11053]) was built and removed: it made the destination declare each caller's ceiling, but the ceiling lived in a file that same caller could rewrite over the SSH login it already held, and the tier that would have made it real ([ORB-11053]'s `authorized_keys` forced command and setgid launcher) is the machinery a *multi-tenant* destination needs. Orbit has no such deployment. The standing answer is that a destination receives no authenticated principal and needs none: an SSH login to it is ownership of it, so it serves the authority the session's argv asks for, and `--operator` on the calling client propagates into every destination it opens. The caller-side guard is that a client running as an agent never propagates it. `--remote-caller-machine-id` stays an audit label. See [4_decisions.md](./4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it); [specs/caller-authorization.md](./specs/caller-authorization.md) is kept as superseded history. `orbit mcp listen` authenticates nobody and keeps its hardcoded `agent`, unchanged.

2. **Selector expiry.** Does a host-qualified selector remain valid across destination catalog edits, host re-init, and mux restarts, or does it expire? If it expires, what is the caller-visible error, and how does that differ from `stale_route`?
3. **Health freshness.** How old may host-reachability and checkout-health be when listed? Is the list a live probe, a cached projection with explicit freshness, or a last-known snapshot for unreachable hosts? The spec requires including unreachable hosts and requires routing to decide on live delivery rather than cached list health; it does not yet define probe cadence or staleness thresholds.
4. **Cloud coordination-store details.** The declared control-plane authority may later offload the coordination store. What is the persistence contract, how do execution-binding hosts refer to it, and how does destination Core still refuse task issuance locally without implying replication?

## 2. Prior Work

### Policy recorded in host-registry / remote-access

[ORB-11008] wrote the federated policy (one namespace, live descriptors, fail-closed routing, destination authority, no replica protocol) into host-registry and remote-access vision. That write-up is superseded as a contract home; those docs now cross-link here as a standing constraint. They no longer hold the five-point policy list.

### Host-registry catalog roles

Owner vs replica checkouts, `machine_id` vs `host_id`, and checkout-health as repo-root presence are live v1 catalog rules. This feature maps new nouns onto those roles instead of creating a parallel ownership vocabulary. See [host-registry 2_design.md](../host-registry/2_design.md). A workspace with absent `owner_machine_id` cannot advertise `control_plane`.

### mcp-bridge v1

v1 is one chosen destination, byte-transparent direct SSH stdio, and no Orbit process relaying onward. Current behavior stays in [mcp-bridge 2_design.md](../mcp-bridge/2_design.md). [mcp-bridge 3_vision.md §5](../mcp-bridge/3_vision.md) now admits this mux as a federated-namespace-only exception and still excludes automatic owner discovery, replication, relays-as-product, and fleet placement.

### Session authority is argv-asserted, on both sides of the wire

`orbit mcp serve --operator` decides an MCP session's authority from argv, resolved once at startup in `crates/orbit-mcp/src/remote/identity.rs`. On an SSH destination the caller writes that argv, and [ORB-12564] settled that this is correct: the caller reached the destination through an SSH login that already lets it run anything, so a second authorization statement on the far side buys nothing. A client started with `--operator` therefore composes an operator argv for every destination it opens. It is a separate axis from the `control_plane` / `execute` classes in [2_design.md §3](./2_design.md), which are destination-derived because they describe the *checkout*, not the caller.

### Remote access

Web SSH local-forward remains a different surface. Federated MCP must not reuse the Web tunnel, loopback dashboard, or attach/spawn lifecycle. See [remote-access](../remote-access/1_overview.md).

## 3. What May Be Distinctive

Nothing in the mux topology is novel: it is a configured reverse-proxy in front of existing MCP/SSH destinations. What may be distinctive for Orbit is refusing to turn that mux into a fleet product — selectors keyed on stable machine identity, capabilities mapped onto owner/replica, mutations split rather than blobbed, unreachable hosts listed rather than dropped, and no implicit failover to the owner.

## 4. References

**Orbit-internal**

- [2_design.md](./2_design.md)
- [specs/federated-workspace-mcp.md](./specs/federated-workspace-mcp.md)
- [host-registry 2_design.md](../host-registry/2_design.md)
- [mcp-bridge 2_design.md](../mcp-bridge/2_design.md)
- [mcp-bridge 3_vision.md](../mcp-bridge/3_vision.md)
- [mcp-session-context 3_vision.md](../mcp-session-context/3_vision.md)
- [remote-access 3_vision.md](../remote-access/3_vision.md)

**External**

- Model Context Protocol — stdio transport and `tools/call` dispatch (the mux still speaks MCP; it is not a new RPC).

## Task References

- [ORB-11008] — recorded the federated multi-host MCP policy
- [ORB-11009] — opened this vision folder as the contract home and left the questions above unresolved (PR #1139)
- [ORB-11010] — closed review holes in the spec without resolving these vision questions

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
