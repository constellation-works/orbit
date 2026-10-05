---
summary: "Host Registry — Vision"
type: design
title: "Host Registry — Vision"
owner: codex
last_updated: 2026-09-27
last_validated: 2026-09-27
status: Accepted
feature: host-registry
doc_role: vision
tags: [host-registry, machine-identity, workspace-catalog]
paths: ["crates/orbit-types/src/identity/machine.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-registry/src/machine_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry/runtime/**", "crates/orbit-config/src/**"]
related_features: [host-registry, mcp-session-context, remote-access, federated-mcp]
related_artifacts: [ORB-11008, ORB-11009]
---

# Host Registry — Vision

Keep the registry small: one durable local machine identity, one validated local
workspace catalog, and one shared composition seam for opening Core runtimes.

The implemented federated MCP mux is specified in
[federated-mcp](../federated-mcp/1_overview.md) ([ORB-11009], citing prior
policy [ORB-11008]). That folder is the contract home. Federated MCP is a
separate stdio mode and an explicit exception to the direct v1 boundary below.
This vision does not restate its selector, capability split, or list schema.

## Current v1 boundary

Direct v1 exposes machine-local MCP discovery and resolves every
workspace-scoped tool on the accepting machine. Direct SSH stdio reaches one
chosen remote server; this surface has no cross-host workspace list or
host-qualified selector. The implemented federated stdio mode is a separate
exception: it lists and routes to the accepting machine and configured SSH
destinations through host-qualified selectors. The local host registry remains
neither a fleet inventory nor a general routing authority. Owner and replica
checkout roles stay catalog vocabulary, not a fleet control plane.

## Questions that require evidence

### Old database cleanup

Remove obsolete registry tables and code only through a migration-safe change. Their historical presence does not justify reviving a fleet control plane.

### Machine display-name changes

`machine.name` changes through global config, while `machine.id` and
`machine.task_prefix` remain fixed. The workspace catalog no longer stores an
owner display-name projection, so changing the name does not require a second
registry write or a repair path.

### Legacy catalog repair

Identity-bearing legacy catalogs with no explicit checkout role fail safely but lack an automatic inference path. A migration tool is justified only if real installations still carry that shape and the intended owner can be established without guessing.

### Authenticated authorization

Direct MCP sessions currently receive authority from the server argv, and Core
enforces the resulting capabilities. The forwarded `caller_machine_id` and
observed caller IP remain audit metadata. If future remote policy needs to
distinguish principals, it needs an authenticated principal or explicit grant
separate from machine IDs, display names and audit labels.

### Federated routing contract

Moved. Open questions (transport authentication, selector expiry, health
freshness, cloud coordination-store) live in
[federated-mcp 3_vision.md](../federated-mcp/3_vision.md). The implementable
contract is [federated-workspace-mcp.md](../federated-mcp/specs/federated-workspace-mcp.md).

### Checkoutless operations

If a future operation truly needs no checkout, define a narrow server-owned API and persistence contract for it. Do not weaken RegisteredRuntimeFactory or make the SSH proxy perform placement and workspace logic.

### Schema evolution

New persisted identity or catalog formats need explicit forward migration,
crash-safe writes, future-version rejection checks and rollback classification.

## Task References

- [ORB-11008] recorded the federated multi-host MCP policy later specified in federated-mcp
- [ORB-11009] moved that contract out of this vision and into `docs/design/federated-mcp/`

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
