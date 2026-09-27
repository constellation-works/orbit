---
title: Orbit MCP — Vision
owner: codex
last_updated: 2026-09-27
last_validated: 2026-09-27
status: Draft
feature: mcp-bridge
doc_role: vision
type: design
summary: Evidence-gated extensions beyond the deliberately small direct-SSH MCP v1, including a federated-namespace-only mux exception.
tags: [mcp, ssh, authorization, audit]
paths: ["crates/orbit-mcp/**", "crates/orbit-core/**", "crates/orbit-registry/**"]
related_features: [host-registry, mcp-session-context, federated-mcp]
related_artifacts: [ORB-11009, ORB-11008]
---

# Orbit MCP — Vision

Direct v1 provides local stdio, direct SSH stdio, server-side resolution,
destination-side caller policy, one Core dispatch boundary, and audit context.
The separately selected federated mode adds an operator-configured mux and
host-qualified routing. Future work should preserve the direct shape and add
complexity only for a demonstrated requirement.

## 1. Direct SSH authority

For direct SSH, the accepting server serves the authority requested by the
`--operator` argument in its startup argv, and Core enforces the resulting
capabilities. The proxy propagates that argument only when it runs with operator
authority; an agent-run proxy never propagates it. The forwarded
`caller_machine_id` and observed `caller_ip` are audit attribution only, not
credentials. Any future authentication mechanism needs a concrete trust model
before it is selected.

## 2. Better provenance

Audit records may eventually need the authenticated SSH principal, key fingerprint,
connection identifier, or proxy version. Each added field should have a clear
source of trust and retention purpose. Audit enrichment must not silently turn an
observational field into an authorization key.

## 3. Contract skew

Direct SSH can connect different Orbit versions. Standard MCP discovery already
lets the accepting server advertise its actual surface. Add explicit compatibility
negotiation only if mixed-version failures cannot be explained cleanly through
normal discovery and errors.

## 4. Other transports

SSH is appropriate while every supported remote has a shell and the same operator
controls both ends. The TCP listener exists for socket deployments and remains
unauthenticated, with loopback as its default; a network MCP service with remote
authentication would require Orbit-owned authentication, session lifecycle,
exposure hardening, and operational support. Do not add one merely to avoid
launching SSH.

## 5. Multi-host routing

V1 remains one chosen destination answering only from its own state. The
client-side proxy stays byte-transparent and does not relay a call onward.
That rule continues to describe **current behavior** in [`2_design.md`](./2_design.md);
this section does not claim a mux is implemented.

The implemented federated MCP surface is an **explicit exception** to those v1
no-relay / byte-transparent rules, **for the federated namespace only**. A mux
in front of operator-configured destinations inspects a host-qualified selector
and forwards the call to the encoded destination. Direct SSH stdio to one chosen
host is unchanged.

The exception does **not** admit automatic owner discovery, replication,
relays-as-product, or fleet placement. Those stay out. The contract is
[federated-mcp](../federated-mcp/specs/federated-workspace-mcp.md)
([ORB-11009], citing [ORB-11008]).

## 6. Evaluation gates

Any direct-mode extension should preserve these properties unless the change
explicitly replaces them:

1. one canonical tool schema;
2. one authoritative accepting server for the selected direct destination;
3. one Core dispatch and audit boundary per tools/call, including unknown names;
4. no policy in the direct byte-forwarding transport;
5. no client-side check required for correctness; and
6. a focused end-to-end test for local and remote context propagation. Federated
   tests additionally cover live discovery, copied selectors, and destination
   capability classes.

Current behavior is described in [`2_design.md`](./2_design.md). The direct
transport and kernel contract is [`references/conformance-v1.yaml`](./references/conformance-v1.yaml).

## Task References

- [ORB-11008] recorded federated MCP policy that this vision now excepts from v1 no-relay
- [ORB-11009] admitted the federated-namespace mux exception in §5 without claiming it is implemented

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
