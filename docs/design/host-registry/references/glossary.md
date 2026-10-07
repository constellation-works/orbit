---
summary: "Glossary — Host Registry"
type: design
title: "Glossary — Host Registry"
owner: codex
last_updated: 2026-10-07
last_validated: 2026-10-07
status: Accepted
feature: host-registry
doc_role: reference
tags: [host-registry, glossary]
paths: ["crates/orbit-types/src/identity/machine.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-registry/src/machine_identity.rs", "crates/orbit-registry/src/hosts.rs", "crates/orbit-cmd/src/hosts/**", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry/runtime/**", "crates/orbit-config/src/**"]
related_features: [host-registry, federated-mcp]
related_artifacts: [ORB-14448, ORB-14449]
---

# Glossary — Host Registry

| Term | Current meaning |
|---|---|
| Machine identity | The [machine] table in ~/.orbit/config.toml: id, name and task_prefix. A valid legacy ~/.orbit/host.toml is folded into it and retired. |
| machine_id | Generated, stable hm_-namespaced logical machine identifier; not an IP address, SSH target, path or credential. A forwarded label using it is audit metadata. |
| machine.name | Changeable human display name for the local machine, set through global config |
| machine.task_prefix | Immutable machine-local namespace projected into task allocation |
| Host | Operator-facing noun for one Orbit installation, identified by its `[machine] id`. The local host runs the command; a remote host is reached over SSH. See [host-commands](../specs/host-commands.md). |
| Host file | `~/.orbit/hosts.toml`, written by `orbit host`. It holds operator-registered remote hosts (name, ssh, machine_id, task_prefix) and nothing that can change on the host. Federated serve, pull drains and replica worktree GC read it. It replaces `mcp-destinations.toml`, which is read only while it is the sole file and is migrated by the first `orbit host` mutation. See [host-commands](../specs/host-commands.md). |
| Host facts | What a host's discovery envelope says about itself: `machine_id`, `machine_name`, `task_prefix`, `binary_version`, `protocol_fingerprint`. Read live by `orbit host` and `orbit doctor`; never persisted. |
| Skew | A registered host's `binary_version` or `protocol_fingerprint` differs from this machine's. `orbit host list` flags it; `orbit doctor` fails on it for a host this machine pulls from. |
| Host name | The operator's name for a host. For the local host it is `machine.name`; for a remote it is the entry's `name`, which defaults to the remote's own `machine.name`. `orbit host show|rename|remove` resolve it case-insensitively, or by exact `machine_id`. |
| Prefix table | Per-process map from task prefix to host, built from the local `machine.task_prefix` and the host file. It is the key for task-id routing. See [host-routing](../specs/host-routing.md). |
| Machine identity implementation | MachineIdentity lifecycle in orbit-registry; persistence-neutral machine ID and name validators in orbit-types |
| Workspace registry | The machine-local ~/.orbit/workspaces.json catalog owned by orbit-registry |
| Logical workspace | Path-independent workspace record containing identity, ownership and ship metadata |
| Local checkout | This machine's repo_root, orbit_dir, role and path overrides for one logical workspace |
| Owner checkout | Local checkout whose logical owner_machine_id equals this machine |
| Replica checkout | Local checkout that explicitly names another machine as the logical owner |
| owner_host_ids | Retired compatibility key accepted and dropped by the current workspace registry release |
| Checkout health | active or invalid derived only from whether a bound repo_root exists |
| Workspace selector | Registered name, logical ID or resolvable local checkout/worktree path used to address a direct runtime. Federated MCP additionally uses the host-qualified selector (`hm_…/ws_*`) specified in [federated-mcp](../../federated-mcp/specs/federated-workspace-mcp.md). |
| Runtime workspace ID | Workspace ID read from .orbit/config.yaml when Core's binding is built; it may differ from the logical catalog ID (`ws_*`) |
| RegisteredRuntimeFactory | orbit-cmd composition seam that selects registry state and opens a Core runtime |
| Caller machine label | The `caller_machine_id` claim forwarded by a direct SSH proxy or derived locally for audit. It is self-asserted and audit-only; it is not authorization. |
