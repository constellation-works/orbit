---
summary: "Host Registry — Overview"
type: design
title: "Host Registry — Overview"
owner: codex
last_updated: 2026-08-23
last_validated: 2026-09-05
status: Accepted
feature: host-registry
doc_role: overview
tags: [host-registry, machine-identity, workspace-catalog]
paths: ["crates/orbit-common/src/types/host.rs", "crates/orbit-common/src/types/workspace.rs", "crates/orbit-registry/src/host_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-cli/src/command/init.rs", "crates/orbit-cli/src/command/host/**", "crates/orbit-cli/src/command/workspace/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-web/src/lib.rs", "crates/orbit-web/src/state.rs", "crates/orbit-mcp/src/remote/identity.rs", "crates/orbit-mcp/src/remote/discovery.rs"]
related_features: [host-registry, mcp-session-context, remote-access, federated-mcp]
related_artifacts: [ORB-11009]
---

# Host Registry — Overview

The live host-registry feature is a machine-local identity and workspace catalog. It tells an Orbit process who the accepting machine is, which logical workspaces this installation knows, and which local checkout may be opened for each workspace.

It is not a fleet router. V1 has no host-registration, host-list, host-retirement, workspace-link, presence, placement, lease, or registry-cache workflow. A proposed federated MCP mux is specified separately in [federated-mcp](../federated-mcp/1_overview.md); that surface is not this catalog and is not current v1 behavior.

## Ownership

| Layer | Current responsibility |
|---|---|
| orbit-common | Persistence-neutral host and workspace primitives: identifier validators and constants, workspace/catalog DTOs, roles, status, and schema constants |
| orbit-registry | host.toml lifecycle, workspaces.json catalog operations, validation, atomic file persistence, and checkout-path health |
| orbit-cmd | RegisteredRuntimeFactory and the composition that joins a selected registry checkout to a Core runtime |
| orbit-cli | Global initialization, workspace mutations and display, local host rename, and MCP server bootstrap |
| orbit-web | Registry-backed workspace snapshots, health projection, lazy runtime caching, and HTTP selection |
| orbit-mcp plus the CLI MCP server | Server identity presentation, local workspace discovery, and authoritative per-call workspace resolution |

orbit-common does not read machine files. orbit-registry does not dispatch Core tools or own MCP or Web transport. orbit-cmd does not own the catalog schema.

## Live artifacts

- ~/.orbit/host.toml is schema v2. It stores a generated stable machine_id, a renameable host_id, and an immutable task_prefix.
- ~/.orbit/workspaces.json is schema v1. It separates logical workspace records from this machine's checkout paths and owner or replica role.
- A workspace runtime is opened only from a local checkout binding. A checkoutless logical catalog entry can be listed but cannot execute.

The accepting machine is authoritative for its files and runtime. Remote MCP simply carries MCP stdio over SSH to that machine; it does not copy or interpret registry state locally.

Older Orbit databases may still contain fleet-registry tables and migration records. No live v1 path reads them as identity, workspace, routing, health, or authorization authority.

See [2_design.md](./2_design.md) for exact schemas and failure behavior, [3_vision.md](./3_vision.md) for evolution gates, [4_decisions.md](./4_decisions.md) for current choices, and [references/glossary.md](./references/glossary.md) for terminology.
