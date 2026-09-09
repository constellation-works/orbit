---
summary: "MCP Session Context — Vision"
type: design
title: "MCP Session Context — Vision"
owner: codex
last_updated: 2026-09-09
last_validated: 2026-09-09
status: Accepted
feature: mcp-session-context
doc_role: vision
tags: ["mcp-session-context", "mcp", "workspace", "audit"]
paths: ["crates/orbit-types/src/tool/**", "crates/orbit-mcp/src/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-core/src/adapter/tool_host/**"]
related_features: ["mcp-session-context", "federated-mcp"]
related_artifacts: [ORB-11009]
---

# MCP Session Context — Vision

Session context should remain a small provenance and correlation envelope. Tool-domain inputs belong in tool schemas; security authority belongs in Core.

## Evolution gates

### Authorization

Authorization is enforced behind Core dispatch. Session capability policy comes from the accepting server and its local or destination-owned SSH policy; caller_machine_id, caller_ip, hostname, and SSH target remain audit evidence rather than credentials. Any future grant source must preserve that separation.

### Additional transports

The current TCP listener and federated SSH mux construct process and transport facts at their accepting boundaries, preserve MCP framing, isolate mutable session state, and create one trace per call. Any new transport must follow the same rules. Because the TCP listener authenticates no client, its safe default remains loopback-only; a non-loopback deployment must provide access control outside the listener.

### Additional context fields

Add a field only when all three are true:

1. a trusted Orbit boundary can derive it;
2. it has session or invocation lifetime;
3. Core dispatch or audit has a concrete consumer.

## Stable principles

- External workspace values address server state; they do not prove identity.
  Federated mode uses host-qualified (`hm_…/ws_*`) selectors, while v1 still
  resolves a local selector on the accepting machine. See
  [federated-mcp](../federated-mcp/specs/federated-workspace-mcp.md).
- The accepting machine describes itself.
- Caller machine and network labels are useful for audit correlation but remain fallible.
- The MCP adapter owns call correlation.
- Every tools/call, including an unknown raw name, crosses one Core audit boundary.
- Core remains the execution, audit, and authorization authority.

## Task References

- [ORB-11009] established the separation between v1 local addressing and the host-qualified federated selector.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
