---
title: Federated MCP — Overview
owner: grok
last_updated: 2026-08-29
last_validated: 2026-09-19
status: Draft
feature: federated-mcp
doc_role: overview
type: design
summary: Mux that presents one MCP namespace over the accepting machine plus operator-configured SSH remotes, keyed by machine_id, without becoming a fleet registry.
tags: [federated-mcp, mcp, host-registry, multi-host]
paths: ["crates/orbit-mcp/**", "crates/orbit-registry/**", "crates/orbit-core/**"]
related_features: [federated-mcp, host-registry, mcp-bridge, remote-access, mcp-session-context]
related_artifacts: [ORB-11044, ORB-11016, ORB-11017, ORB-11015, ORB-11014, ORB-11013, ORB-11012, ORB-11011, ORB-11010, ORB-11009, ORB-11008]
---

# Federated MCP — Overview

Federated MCP is a caller-facing mux: one Orbit MCP namespace in front of the accepting machine plus operator-configured SSH stdio remotes. Local workspaces are included automatically and need no destination row. It is not v1 local/remote MCP, not a host-registry evolution, and not a fleet inventory. Direct SSH stdio to one chosen host remains the v1 remote path (`--mode remote`). The mux is `orbit mcp serve --mode federated` ([ORB-11014] list, [ORB-11015] routing, [ORB-11044] implicit local membership). [ORB-11008] recorded the policy; this folder is the implementable contract from [ORB-11009] (PR #1139), with review holes closed in [ORB-11010].

## 1. Motivation

The implementation keeps one ownership model, keys selectors on stable `machine_id`, and limits the relay exception to the federated namespace. The five-point policy recorded by [ORB-11008] is still right; it was not a routing contract, and it no longer lives as a list in host-registry vision. This feature names the mux, the selector, the capability/authority split, the list schema, and the mcp-bridge exception that the shipped path follows.

## 2. Core Concepts

- **Mux, not registry.** Remote destinations are operator-configured SSH remotes. The accepting machine is an implicit local destination, not a discovered fleet member. The gateway does not discover owners, register a fleet, or reinterpret a selector against its own local catalog.
- **Host-qualified selector.** Structured, caller-uninterpreted addressing token with normative encoding `hm_<id>/ws_*`. Keyed by stable `machine_id`, not renameable `host_id`. Callers copy the list `selector` field; they must not parse or construct the token.
- **Capabilities vs authority.** `control_plane` and `execute` map onto existing host-registry owner and replica checkout roles. The destination's local catalog role determines class; list advertisement is a hint that may lag. Destination Core refuses the other class; the gateway does not rewrite or fail over.
- **Split mutations.** The destination host is authoritative for runs, logs, and scheduler state. The declared control-plane authority is authoritative for task issuance and the coordination store. One control-plane per repository is operator configuration, not a mux check.
- **Live descriptors, fail closed.** Federated `orbit_workspace_list` is a new session-unbound shape (not a v1 extension) and includes unreachable hosts. Routing uses a single error precedence and live delivery.

Full definitions live in [references/glossary.md](./references/glossary.md). The prescriptive contract is [specs/federated-workspace-mcp.md](./specs/federated-workspace-mcp.md).

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Mux vs fleet registry; selector identity; list schema; fail-closed errors | [specs/federated-workspace-mcp.md](./specs/federated-workspace-mcp.md) | [ORB-11009], [ORB-11010] |
| Shipped mux design | [2_design.md](./2_design.md) | [ORB-11011]–[ORB-11017] |
| Open questions (auth, expiry, freshness, cloud store) | [3_vision.md](./3_vision.md) | [ORB-11009] |
| Standing rules and rejected alternatives | [4_decisions.md](./4_decisions.md) | [ORB-11009], [ORB-11010] |
| Standing constraint (mux is not a fleet registry) | [host-registry/3_vision.md](../host-registry/3_vision.md) (cross-link only; the five-point policy list no longer lives there) | [ORB-11008] |
| v1 no-relay / byte-transparent current behavior | [mcp-bridge/2_design.md](../mcp-bridge/2_design.md) | — |
| Federated-namespace exception to that v1 rule | [mcp-bridge/3_vision.md](../mcp-bridge/3_vision.md) §5 | [ORB-11009] |
| Owner vs replica catalog roles | [host-registry/2_design.md](../host-registry/2_design.md) | — |

## Task References

- [ORB-11008] — recorded the federated multi-host MCP policy inside host-registry and remote-access vision
- [ORB-11009] — turns that policy into this implementable design contract (PR #1139)
- [ORB-11010] — closes the PR #1139 review contract holes (INDEX row, tool class, advertisement vs catalog role, list shape, error precedence, competing authorities, selector wording)
- [ORB-11011] — federated MCP implementation epic
- [ORB-11012] — destination-Core `capability_refused`
- [ORB-11013] — destination config, selector type, and routing errors
- [ORB-11014] — federated workspace list
- [ORB-11015] — fail-closed workspace-scoped routing
- [ORB-11016] — federated client registration and shipped-design documentation
- [ORB-11017] — host-qualified federated `workspace` parameter
- [ORB-11044] — implicit local membership for federated serve

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
