---
title: "Remote Access — Decisions"
owner: codex
last_updated: 2026-10-08
last_validated: 2026-10-08
status: Accepted
feature: remote-access
doc_role: decisions
type: design
summary: "Current choices for Orbit Web workspace state, loopback security, and SSH local-forward lifecycle."
tags: [remote-access, orbit-web, ssh]
paths: ["crates/orbit-web/**", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry/runtime/mod.rs"]
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

**Context.** Workspace add, remove, status, and binding changes must become visible without returning a runtime for an old checkout. A checkout can also disappear or be repaired without rewriting `workspaces.json`.

**Decision.** At each request boundary, retain the `workspaces.json` mtime/length fast path but also fingerprint each registered checkout's `repo_root`, `.orbit`, and `.orbit/config.yaml`. A changed registry or checkout fingerprint reloads and validates the registry, atomically publishes a generation, pins the request to one snapshot, and validates cached runtimes by exact binding. Construct runtimes through orbit-cmd RegisteredRuntimeFactory outside state locks.

**Consequences.** Requests observe a coherent old or new registry view, binding changes evict stale runtimes, vanished checkouts are routed as inactive client errors, and repaired checkouts recover on the next request. Cost: each request stats the registry and registered checkout paths; registry parsing occurs only after a fingerprint change, and in-flight requests finish against their pinned generation.

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

## Forward dashboard requests to registered hosts

**Context.** An operator with several registered hosts had to open one `web connect` session per host to see or act on each. The [Long-lived connectivity](./3_vision.md#long-lived-connectivity) gate required explicit lifecycle, port ownership, failure reporting and authority boundaries before one dashboard could hold tunnels to several machines.

**Decision.** Cross that gate with `/api/on/<host>/<path>` ([specs/host-forward.md](./specs/host-forward.md)). The serving dashboard forwards the request to the named host's own dashboard through an SSH local forward it opens on demand, under these rules:

- **Lifecycle.** One tunnel per host. Attach first, spawn otherwise. A dead child is replaced by the next request; there is no reconnect loop or heartbeat. Idle tunnels close after five minutes, and shutdown and the update handover stop every child this process started.
- **Port ownership.** The local listener is `127.0.0.1` on an ephemeral port owned by that tunnel. The remote port is the default dashboard port.
- **Failure reporting.** Typed `{error, code, host}` bodies; a bounded establish and request time; SSH runs with `BatchMode` so it never waits on a prompt; a slow host holds only its own requests.
- **Identity.** Each new tunnel reads the remote's own `machine_id` and refuses a mismatch or a dashboard too old to report one.
- **Authority.** Unsafe methods need the serving dashboard's operator session, through the governed dashboard operation `host.forward`. A spawned remote gets `--operator` only when that session has it. Refusals happen before any SSH process starts.

**Consequences.** One dashboard reaches every registered host live, and each host stays authoritative for what it serves. Cost:

- **An operator session on the serving dashboard can act as operator on every registered host the serving host's SSH identity can reach.** Whoever holds that session holds the operator capability fleet-wide, limited only by SSH access and each remote dashboard's own gates.
- An attached remote dashboard keeps whatever capability it was started with; the forward cannot raise or lower it.
- Each host adds an SSH child and a remote dashboard process while its tunnel is open.
- A host that needs an interactive SSH prompt cannot be reached this way.
