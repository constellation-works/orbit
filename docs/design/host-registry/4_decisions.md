---
summary: "Host Registry — Decisions"
type: design
title: "Host Registry — Decisions"
owner: codex
last_updated: 2026-08-23
last_validated: 2026-09-07
status: Accepted
feature: host-registry
doc_role: decisions
tags: [host-registry, machine-identity, workspace-catalog, runtime-composition]
paths: ["crates/orbit-types/src/identity/host.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-registry/src/host_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-cli/src/command/host/**", "crates/orbit-cli/src/command/workspace/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-web/src/**"]
related_features: [host-registry, mcp-session-context, remote-access, federated-mcp]
related_artifacts: [ORB-11008, ORB-11009]
---

# Host Registry — Decisions

These choices describe current code and a standing constraint that federation
must not become a replica protocol. The federated MCP contract itself lives in
[federated-mcp](../federated-mcp/1_overview.md).

## Shared primitives, owned persistence, separate composition

**Context.** Host and workspace values are shared across Store, Registry, Core and presentation crates, but file lifecycle needs one owner.

**Decision.** orbit-types owns persistence-neutral host/workspace identity DTOs and validators. orbit-registry owns host.toml and workspaces.json. orbit-cmd owns the join from selected registry state to Core runtime state. CLI, Web and MCP remain outer callers.

**Consequences.** Dependency direction stays acyclic and lookup semantics are reusable. Cost: adding a registry field may require coordinated DTO, persistence and composition changes.

## Machine identity is not a transport address

**Context.** Hostnames, IP addresses and SSH destinations can change and can be supplied by an untrusted caller.

**Decision.** Persist a generated hm_ machine_id as stable identity, a renameable host_id for display, and an immutable task_prefix for allocation. Reject path- or transport-shaped machine IDs. Do not persist a machine-wide topology mode in schema v2.

**Consequences.** Renames and transport changes do not redirect stable bindings. Cost: initial identity must be created explicitly, and human-readable remote owner names are only local projections.

## Caller labels remain audit-only

**Context.** MCP can receive a caller machine label and a best-effort SSH source IP, but neither proves an Orbit principal.

**Decision.** Do not use caller_machine_id, caller_ip, SSH host or process cwd to establish registry ownership or authorization.

**Consequences.** Catalog decisions use durable server-local state. Cost: future authorization requires a separate authenticated identity design.

## Logical workspaces and local checkouts are separate

**Context.** Workspace identity and machine-local paths have different lifetimes, and a catalog may know a workspace that cannot execute on this machine.

**Decision.** workspaces.json stores logical workspaces separately from local checkout bindings. Runtime construction requires an active logical record and a local checkout. Owner and replica roles use explicit machine IDs.

**Consequences.** Paths can change without replacing logical identity, and checkoutless entries remain representable. Cost: callers must join and validate two records before opening a runtime.

## Durable registry input fails closed

**Context.** Guessing through malformed, contradictory or newer state can select the wrong workspace or owner.

**Decision.** Validate schema versions, role tokens, IDs, uniqueness, references and ownership relationships before use or write. Preserve invalid or future input bytes. Use atomic replacement for individual file writes.

**Consequences.** Corruption and incompatible upgrades are visible. Cost: ambiguous legacy state requires explicit repair rather than automatic inference.

## Checkout health means repo-root presence

**Context.** The catalog needs a cheap way to avoid opening a checkout whose root disappeared.

**Decision.** validate_workspaces toggles active or invalid from repo_root existence only. It does not claim repository, database, network or owner health.

**Consequences.** CLI, sweep and Web share one inexpensive rule. Cost: deeper failures surface when runtime construction or commands use the checkout.

## Runtime identity preserves legacy configuration

**Context.** The logical catalog ID and the workspace ID already stored in .orbit/config.yaml may differ on valid older installations.

**Decision.** RegisteredRuntimeFactory selects by logical catalog identity, then builds Core's runtime binding with the config file's workspace ID. Keep both identities in ResolvedWorkspaceBinding.

**Consequences.** Existing workspaces open without silent identity rewrites. Cost: diagnostics and APIs must name which identity they report.

## Runtime composition is shared, authority remains in Core

**Context.** CLI, Web and MCP need the same local checkout binding without moving runtime execution into Registry.

**Decision.** orbit-cmd owns RegisteredRuntimeFactory. It resolves catalog state, syncs the task prefix, creates the neutral Core binding and carries replica-owner metadata into Core. Core performs domain validation and mutations.

**Consequences.** Presentation layers do not duplicate runtime assembly. Cost: registry and runtime changes must preserve this explicit join.

## V1 has no fleet control plane

**Context.** Older databases can contain fleet-registry tables, but their command, publication and cache paths are absent from the live application.

**Decision.** Do not treat those tables as identity, catalog, routing, health or authorization authority. V1 has no host register/list/retire, workspace-link, presence, durable fleet execution-profile publication, snapshot, cache-refresh, placement or lease workflow.

**Consequences.** Current behavior is not inferred from dead persistence. Cost: cleanup must still preserve supported database migration compatibility.

## Remote MCP resolves on the accepting machine

**Context.** A local proxy cannot safely decide which checkout or runtime exists on another machine.

**Decision.** Carry raw MCP over direct SSH stdio. The remote CLI server loads its own registry and uses RegisteredRuntimeFactory for each workspace-scoped call.

**Consequences.** The machine holding the data remains authoritative and the proxy stays checkout-free. Cost: checkoutless or cross-machine routing behavior must be added explicitly on the server if it is ever required.

## Federated routing is not a replica protocol

**Recorded:** 2026-08-23 · [ORB-11008] proposed the policy; [ORB-11009] moved the contract to federated-mcp.

**Context.** A single MCP namespace can make several reachable hosts look like
one workspace catalog. Without an authority rule, that presentation could be
mistaken for a synchronized task store or turn multiple checkouts of one
repository into competing control planes.

**Decision.** Host-registry catalog roles stay owner and replica; this registry
is not a routing authority. A federated MCP namespace must not become a replica
protocol or merged store. The implementable mux, selector, capability, list, and
fail-closed contract lives in [federated-mcp](../federated-mcp/specs/federated-workspace-mcp.md).
This rule continues to apply to future federation work that touches catalog
roles.

**Consequences.** Owner/replica remain the only ownership vocabulary in this
folder. Cost: readers of this file no longer find the full routing contract
here and must follow the federated-mcp link.

## Task References

- [ORB-11008] recorded the federated multi-host MCP policy
- [ORB-11009] moved the implementable contract to federated-mcp and left this entry as the host-registry standing constraint

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
