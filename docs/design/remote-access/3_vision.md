---
title: "Remote Access — Vision"
owner: codex
last_updated: 2026-08-23
last_validated: 2026-09-07
status: Accepted
feature: remote-access
doc_role: vision
type: design
summary: "Evolution gates for remote Orbit Web access without weakening the loopback and registry boundaries."
tags: [remote-access, orbit-web, ssh]
paths: ["crates/orbit-web/**", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs"]
related_features: [remote-access, user-interface, host-registry, federated-mcp]
related_artifacts: [ORB-11008, ORB-11009]
---

# Remote Access — Vision

Remote access should stay a thin way to reach a machine's existing Orbit Web state. It should not become an accidental synchronization service or an unauthenticated network daemon.

The proposed federated MCP mux is specified in
[federated-mcp](../federated-mcp/1_overview.md) ([ORB-11009], citing
[ORB-11008]). It is not a change to current Web or direct-SSH MCP behavior,
and it does not make Remote Access a replication feature.

## Evolution gates

### Network exposure

Any routable listener needs a real authentication and authorization design before the loopback guard can change. Origin checking, caller IP, SSH hostname, and possession of a URL are not substitutes.

### Long-lived connectivity

Background reconnect, tunnel multiplexing, or several remote machines in one browser may be useful only with explicit lifecycle, port ownership, failure reporting, and authority boundaries. The foreground one-tunnel model remains the baseline.

### Cross-machine state

An offline view, a merged task result, or any synchronized dashboard state
remains a data-replication problem with an authoritative store, conflict
behavior, and write semantics of its own. Live multi-host workspace
descriptors belong to the federated MCP mux, not to Remote Access; see
[federated-mcp](../federated-mcp/specs/federated-workspace-mcp.md).

### Federated MCP authority

Contract home: [federated-mcp](../federated-mcp/1_overview.md). Remote Access
does not own that mux and must not grow a second copy of its routing or
authority rules.

### Performance

Measure registry parse cost, runtime-cache churn, and aggregate endpoint latency before adding watchers or indexes. Any cache must remain subordinate to orbit-registry state and exact RegisteredRuntimeFactory bindings.

## Stable principles

- Web binds loopback unless a separate authenticated deployment boundary is designed.
- SSH local forwarding remains Web-specific; MCP direct stdio remains independent.
- The remote machine is authoritative for reads and writes served through its dashboard.
- Registry snapshots select workspaces; orbit-cmd constructs registered runtimes; Core executes domain behavior.
- Failed or stale workspace bindings are visible and isolated rather than silently redirected.

## Task References

- [ORB-11008] recorded federated MCP policy while preserving destination-host authority
- [ORB-11009] moved the implementable contract to federated-mcp; this vision now cross-links

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
