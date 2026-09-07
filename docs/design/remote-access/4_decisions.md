---
title: "Remote Access — Decisions"
owner: codex
last_updated: 2026-08-15
last_validated: 2026-09-07
status: Accepted
feature: remote-access
doc_role: decisions
type: design
summary: "Current choices for Orbit Web workspace state, loopback security, and SSH local-forward lifecycle."
tags: [remote-access, orbit-web, ssh]
paths: ["crates/orbit-web/**", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs"]
related_features: [remote-access, user-interface, host-registry]
related_artifacts: []
---

# Remote Access — Decisions

These choices describe the current implementation.

## Serve all registered local workspaces

**Context.** Web must work outside any one checkout and represent the machine's current workspace catalog.

**Decision.** orbit web serve loads local workspace entries from orbit-registry and exposes them through one workspace-keyed DashboardState. --root scopes the server by choosing which registry is loaded, through the same orbit-cmd resolution every other command uses; --workspace chooses the default selection. --global remains a compatibility no-op.

**Consequences.** One process serves current local workspaces from any launch directory, and an explicit root is a real isolation boundary for the dashboard as it is for the CLI. Cost: each request must select a workspace or use a default, aggregate work is bounded rather than exhaustive, and preselection needed its own option once --root stopped doubling as one.

## Registry snapshots are authoritative; runtimes are cached

**Context.** Workspace add, remove, status, and binding changes must become visible without returning a runtime for an old checkout.

**Decision.** Refresh the registry at request boundaries, atomically publish a generation, pin each request to one snapshot, and validate cached runtimes by exact binding. Construct runtimes through orbit-cmd RegisteredRuntimeFactory outside state locks.

**Consequences.** Requests observe a coherent old or new registry view, and binding changes evict stale runtimes. Cost: registry parsing occurs frequently and in-flight requests finish against their pinned generation.

## Web remains loopback-only

**Context.** The dashboard exposes an unauthenticated API with mutating operations. The Origin check is only browser-CSRF mitigation.

**Decision.** Refuse every non-loopback Web bind and explicitly bind the SSH local-forward listener to 127.0.0.1. Reach a remote dashboard through SSH rather than adding Orbit Web credentials or a routable listener.

**Consequences.** Network authentication, encryption, and host verification use the operator's SSH configuration. Cost: remote use requires SSH access and grants the forwarded client the remote dashboard's full authority.

## Connect attaches before spawning

**Context.** A remote dashboard may already be listening; starting another would fail its bind and disconnecting must not stop someone else's process.

**Decision.** Probe /healthz through a commandless SSH forward first. Keep that forward when healthy. Spawn orbit web serve through a PTY-backed forward only when the probe times out.

**Consequences.** Existing dashboards can be shared safely, while a spawned dashboard is tied to its SSH session and reaped on teardown. Cost: an empty remote port adds a short probe delay and the attach/spawn check remains racy.

## Web and MCP use different SSH transports

**Context.** Web needs HTTP reachability and health probing; MCP needs a byte-faithful stdio protocol stream.

**Decision.** orbit-web owns its -L local-forward implementation. orbit-mcp independently uses direct non-PTY SSH stdio. No common tunnel abstraction or TCP MCP listener connects them.

**Consequences.** Each protocol has the smallest appropriate lifecycle and PTY posture. Cost: shared SSH process details are intentionally limited to generic shell helpers rather than one transport framework.

## Remote access is live access, not synchronization

**Context.** Tunnelling a machine's dashboard does not create shared durable state.

**Decision.** Treat the remote machine and its registered workspaces as authoritative for everything shown or mutated through that connection.

**Consequences.** No merge, replication, or offline model is implied. Cost: state disappears from view when the target or tunnel is unavailable.
