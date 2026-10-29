---
summary: "Host Registry — Overview"
type: design
title: "Host Registry — Overview"
owner: codex
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Implemented
stage: shipped
next: "—"
feature: host-registry
doc_role: overview
tags: [host-registry, machine-identity, workspace-catalog]
paths: ["crates/orbit-types/src/identity/machine.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-config/src/registry/settings/**", "crates/orbit-registry/src/machine_identity.rs", "crates/orbit-registry/src/hosts.rs", "crates/orbit-cmd/src/hosts/**", "crates/orbit-cli/src/command/host.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry/runtime/**", "crates/orbit-cli/src/command/init/**", "crates/orbit-cli/src/command/config/**", "crates/orbit-cli/src/command/workspace/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-web/src/lib.rs", "crates/orbit-web/src/state/**", "crates/orbit-mcp/src/remote/identity.rs", "crates/orbit-mcp/src/remote/discovery.rs"]
related_features: [host-registry, mcp-session-context, remote-access, federated-mcp]
related_artifacts: [ORB-11009, ORB-14448, ORB-14449, ORB-14451]
---

# Host Registry — Overview

The live host-registry feature is a machine-local identity and workspace catalog. It tells an Orbit process who the accepting machine is, which logical workspaces this installation knows, and which local checkout may be opened for each workspace.

It is not a fleet router. The operator registers remote hosts with `orbit host add`; the CLI-owned host file stores the operator's name and SSH target plus each host's stable `machine_id` and `task_prefix`. It can also store a registration-time `host_os` snapshot for crew selection, added by [ORB-16763]; probes do not refresh that snapshot. By default, `orbit host list|show` use live probes for changing details such as reachability, version, protocol and workspaces ([specs/host-commands.md](./specs/host-commands.md), [ORB-14448]). Task ids route to the host their prefix names, and `--host` names a host for `--workspace` and `--pull` ([specs/host-routing.md](./specs/host-routing.md), [ORB-14449]). The dashboard's Settings › Hosts view exposes the same operations and is implemented ([specs/host-commands.md#dashboard](./specs/host-commands.md#dashboard), [ORB-14451]). This catalog has no presence, placement, lease, workspace-link or registry-cache workflow. The implemented federated MCP mux is specified separately in [federated-mcp](../federated-mcp/1_overview.md); [ORB-11009] established its separate contract home. It consumes registered hosts for MCP routing but remains a distinct serving mode, separate from this catalog.

## Ownership

| Layer | Current responsibility |
|---|---|
| orbit-types | Persistence-neutral host and workspace primitives: identifier validators and constants, workspace/catalog DTOs, roles, status, and schema constants |
| orbit-registry | machine identity lifecycle and legacy migration, workspaces.json catalog operations, the hosts.toml host file, validation, atomic file persistence, and checkout-path health |
| orbit-cmd | RegisteredRuntimeFactory and the composition that joins a selected registry checkout to a Core runtime; the `orbit host` operations and their live identity probe |
| orbit-cli | Global initialization, workspace mutations and display, `orbit host`, machine-name configuration, and MCP server bootstrap |
| orbit-web | Registry-backed workspace snapshots, health projection, lazy runtime caching, and HTTP selection |
| orbit-mcp plus the CLI MCP server | Server identity presentation, local workspace discovery, and authoritative per-call workspace resolution |

orbit-types does not read machine files. orbit-registry does not dispatch Core tools or own MCP or Web transport. orbit-cmd does not own the catalog schema.

## Live artifacts

- `~/.orbit/config.toml` stores the `[machine]` identity: a generated stable
  `id`, a renameable operator-chosen `name`, and an immutable `task_prefix`.
  `orbit init` creates it. A legacy `~/.orbit/host.toml` is migrated into
  `[machine]` on first identity load and removed once that write succeeds.
- ~/.orbit/workspaces.json is schema v1. It separates logical workspace records from this machine's checkout paths and owner or replica role.
- `~/.orbit/hosts.toml` lists operator-registered remote hosts by name, SSH target, `machine_id` and `task_prefix`. When it is absent, Orbit still reads `~/.orbit/mcp-destinations.toml` as legacy routes; the next `orbit host` mutation migrates those routes and removes the legacy file. If both files exist, Orbit refuses to choose between them ([specs/host-commands.md](./specs/host-commands.md)).
- A workspace runtime is opened only from a local checkout binding. A checkoutless logical catalog entry can be listed but cannot execute.

The accepting machine is authoritative for its files and runtime. Remote MCP simply carries MCP stdio over SSH to that machine; it does not copy or interpret registry state locally.

Older Orbit databases may still contain fleet-registry tables and migration records. No live v1 path reads them as identity, workspace, routing, health, or authorization authority.

See [2_design.md](./2_design.md) for exact schemas and failure behavior, [3_vision.md](./3_vision.md) for evolution gates, [4_decisions.md](./4_decisions.md) for current choices, and [references/glossary.md](./references/glossary.md) for terminology.

## Delivery at a Glance

The design was explicitly accepted in [ORB-10245]. Its committed local-catalog and
remote-host scope is shipped; the questions in the vision are future work.

| Slice | Current code | Delivery |
|---|---|---|
| Local identity and workspace catalog | `crates/orbit-registry/src/machine_identity.rs`, `crates/orbit-registry/src/workspace_registry/` | Done — [ORB-10247], [ORB-10248]; identity moved into global config by [ORB-12725] |
| Operator host file and commands | `crates/orbit-registry/src/hosts.rs`, `crates/orbit-cmd/src/hosts/` | Done — [ORB-14448] |
| Prefix routing and host selection | `crates/orbit-registry/src/hosts.rs::TaskPrefixTable`, `crates/orbit-mcp/src/federated/` | Done — [ORB-14449] |
| Settings › Hosts | `crates/orbit-web/src/api/host.rs`, `crates/orbit-web/src/state/hosts.rs` | Done — [ORB-14451] |
| Registration-time OS snapshot | `crates/orbit-registry/src/hosts.rs::HostEntry` | Done — [ORB-16763] |

## Task References

- [ORB-10245] accepted the host-registry design and froze its implementation contract.
- [ORB-10247] added stable machine identity and initialization.
- [ORB-10248] split logical workspaces from local checkout bindings.
- [ORB-11009] established the separate implementable federated MCP contract.
- [ORB-12725] moved local machine identity into global config and removed owner display-name projections.
- [ORB-14448] implemented the host file and orbit host operations.
- [ORB-14449] implemented task-prefix routing and --host selection.
- [ORB-14451] implemented dashboard Settings › Hosts through shared registry operations.
- [ORB-16763] added registration-time host OS facts for serving-host crew selection.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
