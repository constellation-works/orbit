---
summary: "Glossary — Host Registry"
type: design
title: "Glossary — Host Registry"
owner: codex
last_updated: 2026-09-07
last_validated: 2026-09-07
status: Accepted
feature: host-registry
doc_role: reference
tags: [host-registry, glossary]
paths: ["crates/orbit-types/src/identity/host.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-registry/src/host_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs"]
related_features: [host-registry, federated-mcp]
related_artifacts: []
---

# Glossary — Host Registry

| Term | Current meaning |
|---|---|
| Host identity | The schema-v2 machine declaration in ~/.orbit/host.toml: machine_id, host_id and task_prefix |
| machine_id | Generated, stable hm_-namespaced logical machine identifier; not an IP address, SSH target, path or credential by itself. A destination-issued SSH acceptance can bind it to an authenticated key. |
| host_id | Renameable human display name for the local machine |
| task_prefix | Immutable machine-local namespace projected into task allocation |
| Host primitives | Persistence-neutral host DTOs, lifecycle enums and validators owned by orbit-types |
| Workspace registry | The machine-local ~/.orbit/workspaces.json catalog owned by orbit-registry |
| Logical workspace | Path-independent workspace record containing identity, ownership and ship metadata |
| Local checkout | This machine's repo_root, orbit_dir, role and path overrides for one logical workspace |
| Owner checkout | Local checkout whose logical owner_machine_id equals this machine |
| Replica checkout | Local checkout that explicitly names another machine as the logical owner |
| owner_host_ids | Local machine-ID-to-display-name projection for owners referenced by this catalog; not a fleet inventory |
| Checkout health | active or invalid derived only from whether a bound repo_root exists |
| Workspace selector | Registered name, logical ID or resolvable local checkout/worktree path used to address a direct runtime. Federated MCP additionally uses the host-qualified selector (`hm_…/ws_*`) specified in [federated-mcp](../../federated-mcp/specs/federated-workspace-mcp.md). |
| Runtime workspace ID | Workspace ID read from .orbit/config.yaml when Core's binding is built; it may differ from the logical catalog ID (`ws_*`) |
| RegisteredRuntimeFactory | orbit-cmd composition seam that selects registry state and opens a Core runtime |
| Caller machine label | The `caller_machine_id` claim forwarded by a direct SSH proxy or derived locally for audit. It is self-asserted unless the destination's forced-command SSH acceptance supplies a key-bound caller identity; the label alone is not authorization. |
