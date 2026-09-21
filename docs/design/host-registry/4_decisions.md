---
summary: "Host Registry — Decisions"
type: design
title: "Host Registry — Decisions"
owner: codex
last_updated: 2026-09-21
last_validated: 2026-09-21
status: Accepted
feature: host-registry
doc_role: decisions
tags: [host-registry, machine-identity, workspace-catalog, runtime-composition]
paths: ["crates/orbit-types/src/identity/machine.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-registry/src/machine_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-config/src/registry.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-cli/src/command/config/**", "crates/orbit-cli/src/command/workspace/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-web/src/**"]
related_features: [host-registry, mcp-session-context, remote-access, federated-mcp]
related_artifacts: [ORB-11008, ORB-11009, ORB-12725]
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

## Machine identity lives in `[machine]` in the global config.toml

**Recorded:** 2026-09-21 · [ORB-12725].

**Supersedes** "persist identity in host.toml" and the renameable `host_id`
display name recorded elsewhere in this folder. The folder keeps its name for
history.

**Context.** `~/.orbit/host.toml` held three values — a generated `machine_id`,
a renameable display name, and an immutable `task_prefix` — in their own file,
behind their own command (`orbit host show` / `orbit host rename`), with their
own schema version and migration path. Under the per-user ownership model
[ORB-12718] that is the same kind of fact as this user's crews and delivery
settings: per-user, per-machine, and read on every runtime open. Two files and
two commands for one user's settings is a seam with nothing on either side of
it. The routine `hosts:` pin that justified a separately addressable display
name is already gone [ORB-12236], and the identity has been `machine_id`
(`hm_…`) since [ORB-10247] / [ORB-10721] — `host_id` was only ever the label.

**Decision.**

- The three values move into the **global** `~/.orbit/config.toml` as
  `[machine] id`, `name`, and `task_prefix`. They are ordinary registry
  settings: admitted, validated, and shown by `orbit config show` under a
  `Machine` section. `host.toml` is gone.
- `[machine]` is **global-only**. A workspace `config.toml` that supplies it is
  refused at load naming the file. This is the mirror image of the replace-only
  security keys: there a workspace may set the value and must restate it to
  keep it; here it may not set it at all, because a checkout must not be able
  to rename, renumber, or re-identify the machine it happens to sit on.
- `machine.name` is settable: `orbit config set --global machine.name <value>`
  replaces `orbit host rename`. `machine.id` and `machine.task_prefix` are
  read-only — `orbit config set` refuses both, naming why. A hand edit that
  makes either disagree with what the local task store or workspace registry
  recorded fails closed at the allocator and at registry validation, exactly as
  a contradictory `host.toml` did.
- `orbit host` is deleted. `orbit host show` is `orbit config show` (or
  `orbit config get machine.id`); `orbit host rename` is
  `orbit config set --global machine.name`.
- **Migration, one release.** On load, a `host.toml` beside the global config
  is folded into `[machine]` and removed, preserving the recorded `machine_id`
  and namespace. A `host.toml` that disagrees with an existing `[machine]`
  table is refused naming both paths rather than picking a winner: they are two
  different answers to "who is this machine", and either choice orphans the ids
  minted under the other.

Two spellings of *host* survive on purpose, because they are not Orbit's
vocabulary to change: the `hosts` / `host_aliases` tables inside shipped SQLite
migration v5, which are immutable history, and the `host/local` fallback audit
label pinned by mcp-bridge conformance v1, which other implementations match on.

**Consequences.** One file holds a user's machine-local settings, one command
shows them, and one validation pipeline admits them, so `orbit config show`,
`orbit config get machine.id`, and every runtime consumer cannot disagree.
Cost: the machine identity now shares a failure domain with the rest of the
global config — a `config.toml` that will not admit takes the identity with
it — and reading it resolves that whole document rather than parsing three
lines.

## `owner_host_ids` is removed, not renamed

**Recorded:** 2026-09-21 · [ORB-12725].

**Context.** `workspaces.json` carried `owner_host_ids`, a machine-id to
display-name map for owners named by a local workspace record. It existed to
render an owner's human name offline, and `orbit host rename` wrote through to
it. Nothing read it: the routine-pin diagnostics it was built for were removed
with the pin [ORB-12236], and the federated descriptor already carries a remote
machine's name in its own `machine_name` field.

**Decision.** Drop the map. The catalog records stable `owner_machine_id` only.
The local machine's display name is read from `machine.name` when it is needed;
a remote machine's comes from the federated descriptor that observed it. No
persisted cache is kept under another name.

**Consequences.** The rename side effect disappears with the map, so the
two-file non-atomic write `orbit host rename` performed — and the repair path
that reconciled it on the next validated load — is gone. `workspaces.json` uses
`deny_unknown_fields`, so the retired key is accepted and dropped for one
release rather than failing every command on an existing catalog; it is never
written back. Cost: an offline listing can name a remote owner only by its
stable id until a federated list observes it.

## Task References

- [ORB-11008] recorded the federated multi-host MCP policy
- [ORB-11009] moved the implementable contract to federated-mcp and left this entry as the host-registry standing constraint
- [ORB-12725] folded host.toml into `[machine]`, deleted `orbit host`, removed `owner_host_ids`, and renamed host -> machine across the codebase

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
