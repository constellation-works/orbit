---
summary: "Host Registry — Vision"
type: design
title: "Host Registry — Vision"
owner: codex
last_updated: 2026-08-23
last_validated: 2026-09-07
status: Accepted
feature: host-registry
doc_role: vision
tags: [host-registry, machine-identity, workspace-catalog]
paths: ["crates/orbit-types/src/identity/host.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-registry/src/host_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs"]
related_features: [host-registry, mcp-session-context, remote-access, federated-mcp]
related_artifacts: [ORB-11008, ORB-11009]
---

# Host Registry — Vision

Keep the registry small: one durable local machine identity, one validated local
workspace catalog, and one shared composition seam for opening Core runtimes.

The proposed federated MCP mux is specified in
[federated-mcp](../federated-mcp/1_overview.md) ([ORB-11009], citing prior
policy [ORB-11008]). That folder is the contract home. This vision does not
restate the mux, selector, capability split, or list schema.

## Current v1 boundary

V1 exposes machine-local MCP discovery and resolves every workspace-scoped tool
on the accepting machine. Direct SSH stdio reaches a chosen remote server, but
there is no federation gateway, cross-host workspace list, or host-qualified
workspace selector. The local host registry remains neither a fleet inventory
nor a routing authority for another machine. Owner and replica checkout roles
stay the catalog vocabulary a federated surface must map onto; they are not a
fleet control plane.

## Questions that require evidence

### Old database cleanup

Remove obsolete registry tables and code only through a migration-safe change. Their historical presence does not justify reviving a fleet control plane.

### Host rename recovery

The current two-file rename is ordered but not transactional. If stale owner display names become operationally significant, add a deterministic repair/check command or one recoverable journal before expanding rename semantics.

### Legacy catalog repair

Identity-bearing legacy catalogs with no explicit checkout role fail safely but lack an automatic inference path. A migration tool is justified only if real installations still carry that shape and the intended owner can be established without guessing.

### Authenticated authorization

If Core later authorizes remote calls, it needs an authenticated principal or grant separate from machine_id, host_id, caller IP and SSH audit labels. Existing registry and session fields must not be promoted into credentials by implication.

### Federated routing contract

Moved. Open questions (transport authentication, selector expiry, health
freshness, cloud coordination-store) live in
[federated-mcp 3_vision.md](../federated-mcp/3_vision.md). The implementable
contract is [federated-workspace-mcp.md](../federated-mcp/specs/federated-workspace-mcp.md).

### Checkoutless operations

If a future operation truly needs no checkout, define a narrow server-owned API and persistence contract for it. Do not weaken RegisteredRuntimeFactory or make the SSH proxy perform placement and workspace logic.

### Schema evolution

New host or catalog schema versions need explicit forward migration, crash-safe writes, future-version rejection tests and rollback classification.

## Task References

- [ORB-11008] recorded the federated multi-host MCP policy later specified in federated-mcp
- [ORB-11009] moved that contract out of this vision and into `docs/design/federated-mcp/`

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
