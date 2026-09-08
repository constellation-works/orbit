---
summary: "Host Registry — Design"
type: design
title: "Host Registry — Design"
owner: codex
last_updated: 2026-09-08
last_validated: 2026-09-07
status: Accepted
feature: host-registry
doc_role: design
tags: [host-registry, machine-identity, workspace-catalog, runtime-composition]
paths: ["crates/orbit-types/src/identity/host.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-registry/src/host_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-cli/src/command/init/**", "crates/orbit-cli/src/command/host/**", "crates/orbit-cli/src/command/workspace/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-web/src/lib.rs", "crates/orbit-web/src/state.rs", "crates/orbit-mcp/src/remote/identity.rs", "crates/orbit-mcp/src/remote/discovery.rs"]
related_features: [host-registry, mcp-session-context, remote-access]
related_artifacts: []
---

# Host Registry — Design

## 1. Boundary and dependency direction

The live implementation has four layers.

| Layer | Owns | Must not own |
|---|---|---|
| orbit-types | Host and workspace identity DTOs, identifier validation, lifecycle enums, schema constants | Files, runtime construction, transport |
| orbit-registry | Machine identity lifecycle; workspace catalog parsing, mutation, validation, health and file I/O | CLI orchestration, MCP framing, Core execution |
| orbit-cmd | Registry-aware selection and Core runtime construction | Registry schemas or persistence |
| CLI, Web and MCP server | User/API inputs, presentation, refresh timing and request dispatch | Alternate catalog semantics |

HostIdentity and host.toml I/O live in orbit-registry. Shared primitives such as validate_machine_id, validate_host_id and the machine-ID namespace constants live in orbit-types so identity validation remains persistence-neutral.

## 2. Machine identity

The current file is:

    schema_version = 2
    machine_id = "hm_0123456789abcdef"
    host_id = "build-host"
    task_prefix = "BH"

### Field rules

- machine_id is generated once, starts with hm_, and accepts only an ASCII alphanumeric, underscore or hyphen suffix. Paths, hostnames, SSH targets and URIs are not valid substitutes.
- host_id is a human display name. It is renameable and must be non-empty, trimmed, path-free and control-character-free.
- task_prefix is chosen once for task allocation. Fresh values are two to five uppercase ASCII letters and cannot use reserved artifact namespaces. Existing migrated installations may retain ORB.
- Schema v2 has no machine mode. Topology is not a persisted identity decision.

orbit init is the creation and migration surface. A fresh non-interactive initialization requires both host name and task prefix. Repeated initialization returns the existing identity without rewriting it.

An unversioned host-id-only file and schema v1 can migrate in place. Migration preserves a schema-v1 machine ID when present, generates one otherwise, retains the host name, seeds task_prefix as ORB, and drops the old mode field. Writes are staged, reparsed, and atomically replaced.

Strict consumers call load_host_identity. Absent, legacy, malformed, incomplete, blank, invalid-ID and future-schema files are errors and are never silently repaired. inspect_host_identity exposes absent and legacy as explicit states for bootstrap and compatibility callers. MCP identity presentation is intentionally more tolerant: absent or legacy local identity may be represented for audit as host/local, but that fallback is not a persisted machine identity.

### Rename behavior

orbit host rename is local-only. It verifies the current name, holds the host file lock, changes host.toml while preserving machine_id and task_prefix, and updates owner_host_ids entries for locally owned workspaces.

Each file write is atomic, but the two files are not one transaction: host.toml is written before workspaces.json. A failure on the second write can leave the durable identity renamed while the local display-name projection is stale. A later validated registry load repairs the local owner's display name when that machine is represented in owner_host_ids.

### Task-prefix composition

RegisteredRuntimeFactory projects task_prefix into the global task allocator before opening a runtime. A pristine legacy allocator may adopt the configured prefix. Once allocation or task bindings have begun, a conflicting prefix fails closed rather than renaming issued IDs.

## 3. Workspace catalog

~/.orbit/workspaces.json has schema version 1 and three distinct parts:

- workspaces are logical records: stable ID, name, owner_machine_id, Git/ship metadata, lifecycle status and timestamps;
- checkouts are machine-local bindings: workspace ID, repo_root, orbit_dir, role, optional replica owner and path overrides;
- owner_host_ids maps owner machine IDs referenced by local workspace records to display names. It is a local presentation projection, not a fleet inventory.

A logical workspace may exist without a local checkout. Runtime callers require both. The catalog allows at most one local checkout per logical workspace and rejects duplicate workspace IDs or names. Load and save reject a `repo_root` or `path_override` claimed by more than one checkout. `register_checkout` refuses a reused `repo_root`; `set_path_override` refuses a path already claimed as another checkout's `repo_root` or override. Distinct checkouts may share an `orbit_dir`.

### Owner and replica roles

An identity-bearing machine must use explicit ownership data:

- an owner checkout has no checkout-level owner_machine_id, and the logical owner must equal the local machine_id;
- a replica checkout names a non-local owner_machine_id, and that value must equal the logical workspace owner;
- all persisted machine IDs pass the same orbit-types validator;
- ownership is never inferred from paths, Git remotes, SSH destinations or caller audit labels.

Installations without host identity retain a narrow standalone compatibility path: a missing checkout role may canonicalize to owner. Once host identity exists, missing or contradictory roles and missing logical owners fail closed.

### Parsing, migration and writes

- A missing workspaces.json loads as an empty schema-v1 registry.
- Malformed JSON, unknown fields, invalid role tokens, duplicate identities, broken checkout references, contradictions and unsupported future schemas fail without rewriting the file.
- An unversioned legacy catalog can be split into logical workspaces and local checkouts and written back atomically when its role is unambiguous. Identity-bearing legacy data with no explicit checkout role is rejected rather than guessed.
- Successful saves validate a clone, serialize canonical JSON, and use atomic replacement. Rejected mutations leave the prior file intact.
- Canonicalization sorts and deduplicates path overrides and may refresh the local machine's owner display name.

## 4. Checkout-path health

validate_workspaces is deliberately narrow. For each logical workspace with a local checkout:

- an existing repo_root yields active;
- a missing repo_root yields invalid;
- a checkoutless logical workspace keeps its existing catalog status.

This is path presence, not a repository, database, network or owner-health probe. CLI workspace list and run sweep persist status changes. Orbit Web derives the same status while loading a new in-memory snapshot.

## 5. Registered runtime composition

RegisteredRuntimeFactory is the application seam between Registry and Core.

### Selection

CLI selectors may be a registered name, logical ws_* ID, or a resolvable local checkout/worktree path. Name and ID matches must be unique. Path selection must resolve to a registered checkout, path override, or matching Git common directory. Unknown, ambiguous, inactive and checkoutless selections fail.

MCP uses the same server-side registry selection for workspace-scoped calls. It never falls back to the MCP server's process cwd. The client-provided workspace value is addressing input; the server writes the resolved logical ID into the tool session context.

### Binding

The Core WorkspaceRuntimeBinding contains:

- workspace_id read from the selected checkout's .orbit/config.yaml;
- the registered repo_root;
- the effective ship mode from the logical workspace record.

The config workspace ID can differ from the logical registry ID on legacy installations. ResolvedWorkspaceBinding preserves both instead of silently rewriting either identity.

RegisteredRuntimeFactory also carries replica ownership into Core's coordination-write guard. Registry selects and describes local state; Core remains authoritative for command/tool validation and mutation.

## 6. Outer callers

### CLI

The main CLI opens ordinary runtimes through RegisteredRuntimeFactory using cwd, --root and optional --workspace. `task show` is the one exception, on both the human subcommand and `orbit tool run orbit.task.show`: without `--workspace` or a tool-input `workspace` it opens the checkout the coordination task registry names as the task ID's owner, so it works from a foreign checkout, a linked worktree, and from a directory that is no workspace at all, and it reports the owning workspace name and logical ID. With an explicit workspace selector it is the ordinary registered bootstrap, and the selector filters. Linked-worktree runtime identities are not selectors. Workspace init, role, list, show, remove and teardown call orbit-registry directly for catalog operations. The only active host command is local rename.

There is no active v1 CLI surface for fleet host registration, enumeration or retirement, and no workspace owner-link command.

### Web

Orbit Web loads local workspaces from orbit-registry, derives checkout-path health, and opens active runtimes lazily through orbit-cmd. Each request pins one immutable registry generation. A successful refresh swaps the complete snapshot and evicts incompatible cached runtimes; a failed refresh retains the last valid snapshot. An invalid initial load fails startup.

### MCP

orbit-mcp reads orbit-registry identity state to describe the accepting process and exposes machine-local discovery definitions. The CLI MCP server resolves each workspace-scoped call against the accepting machine's registry, composes the runtime, and dispatches through Core. `orbit.task.show` is the exception: an id-only call follows the globally unique task ID through the host task registry. Ambient initialize metadata — including a linked-worktree runtime identity — is not a filter. An explicit per-call `workspace` remains fail-closed.

orbit.workspace.list returns active logical workspaces that have a checkout registered on the accepting machine. This includes locally registered replicas and excludes active checkoutless catalog entries; owner_machine_id is not a discovery filter. Remote MCP uses direct SSH stdio, and the local proxy does not resolve workspaces, inspect checkouts or read the remote registry.

caller_machine_id and caller_ip are audit metadata. Neither participates in catalog ownership or authorization.

## 7. Database compatibility

Older databases may retain tables and migration records from the removed fleet-registry implementation. Current v1 identity, catalog, health, runtime selection, routing and authorization do not read them. Their presence must never be treated as current authority. Any cleanup still has to preserve supported database upgrades and the immutability of already-shipped Store migrations.

## 8. Failure summary

| Condition | Result |
|---|---|
| host.toml absent on strict path | Actionable initialization error |
| host.toml legacy on strict path | Migration-required error |
| host.toml malformed or future | Error; original bytes retained |
| workspaces.json absent | Empty registry |
| workspaces.json malformed, contradictory or future | Error; original bytes retained |
| selector unknown, ambiguous, inactive or checkoutless | Runtime construction refused |
| task prefix conflicts after allocation | Runtime construction refused |
| Web refresh cannot load registry | Last valid in-memory snapshot retained |
| initial Web registry load fails | Server startup fails |
