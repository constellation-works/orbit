---
title: "Remote Access — Overview"
owner: codex
last_updated: 2026-08-15
last_validated: 2026-09-07
status: Accepted
feature: remote-access
doc_role: overview
type: design
summary: "Multi-workspace Orbit Web serving and loopback-safe remote access over an SSH local forward."
tags: [remote-access, orbit-web, ssh]
paths: ["crates/orbit-web/**", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-cli/src/command/web.rs", "crates/orbit-cli/src/command/operation.rs"]
related_features: [remote-access, user-interface, host-registry]
related_artifacts: []
---

# Remote Access — Overview

Remote access has two Web-owned surfaces:

- orbit web serve exposes one loopback dashboard over the machine's registered local workspaces.
- orbit web connect <ssh-host> opens a local SSH forward to a remote machine's loopback dashboard.

Both commands are runtime-free CLI entry points, so they can start outside a workspace. The remote machine remains authoritative for every workspace and mutation it serves.

## Multi-workspace Web state

Orbit Web loads workspace and checkout state from orbit-registry. Each request selects a workspace with ?workspace=<id>, or uses the configured default. Aggregate endpoints expose the registered workspace list and a bounded task view across active local workspaces.

Workspace runtimes are opened lazily through orbit-cmd's RegisteredRuntimeFactory and executed by Core. Orbit Web caches runtimes only as a performance aid; a pinned registry snapshot and exact runtime binding remain authoritative.

The registry is refreshed at request boundaries. A successful refresh atomically publishes a new generation and evicts stale bindings. A malformed refresh retains the last valid snapshot; a malformed initial load fails startup.

## Remote Web connection

Web's SSH transport is a local port forward. It first probes for an existing remote dashboard through a commandless forward. If /healthz answers, connect attaches without touching that server. Otherwise it starts orbit web serve --no-open through a second SSH process, waits for health, and owns that process's lifetime.

This tunnel belongs to orbit-web. It is separate from MCP remote mode: MCP uses direct non-PTY SSH stdio and no local-forward listener.

## Security boundary

Orbit Web refuses non-loopback binds and has no application authentication. Its Origin check is browser-CSRF mitigation, not access control. SSH supplies remote authentication, encryption, and host verification.

The Web API includes mutations. Anyone who can reach the forwarded local port can act with the authority of the remote Orbit process. Remote access is live access to one machine's state, not cross-machine synchronization, replication, or an offline shared store.

See [2_design.md](./2_design.md) for mechanics, [3_vision.md](./3_vision.md) for evolution gates, [4_decisions.md](./4_decisions.md) for current choices, and [specs/ssh-tunnel.md](./specs/ssh-tunnel.md) for the tunnel contract.
