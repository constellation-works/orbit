---
summary: "Host Registry — Design"
type: design
title: "Host Registry — Design"
owner: codex
last_updated: 2026-10-07
last_validated: 2026-10-07
status: Accepted
feature: host-registry
doc_role: design
tags: [host-registry, machine-identity, workspace-catalog, runtime-composition]
paths: ["crates/orbit-types/src/identity/machine.rs", "crates/orbit-types/src/workspace/registry.rs", "crates/orbit-registry/src/machine_identity.rs", "crates/orbit-registry/src/hosts.rs", "crates/orbit-cmd/src/hosts/**", "crates/orbit-cli/src/command/host.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-cmd/src/registry/runtime/**", "crates/orbit-config/src/**", "crates/orbit-cli/src/command/init/**", "crates/orbit-cli/src/command/workspace/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-web/src/lib.rs", "crates/orbit-web/src/state/**", "crates/orbit-mcp/src/remote/identity.rs", "crates/orbit-mcp/src/remote/discovery.rs"]
related_features: [host-registry, mcp-session-context, remote-access]
related_artifacts: [ORB-14448, ORB-14449, ORB-14451]
---

# Host Registry — Design

## 1. Boundary and dependency direction

The live implementation has five layers.

| Layer | Owns | Must not own |
|---|---|---|
| orbit-types | Workspace identity DTOs, machine/workspace identifier validation, lifecycle enums and schema constants | Files, runtime construction, transport |
| orbit-registry | Machine identity lifecycle and legacy host.toml migration; workspace catalog parsing, mutation, validation, health and file I/O; the host file's schema, validation, legacy fallback and atomic writes | CLI orchestration, MCP framing, Core execution |
| orbit-config | Global machine settings schema and config.toml I/O | Workspace catalog persistence and runtime composition |
| orbit-cmd | Registry-aware selection and Core runtime construction; the `orbit host` operations, which join the host file to the federated identity probe | Registry schemas or persistence |
| CLI, Web and MCP server | User/API inputs, presentation, refresh timing and request dispatch | Alternate catalog semantics |

MachineIdentity and the one-release host.toml migration live in orbit-registry. The machine settings schema and config.toml I/O live in orbit-config. Shared primitives such as validate_machine_id, validate_machine_name and the machine-ID namespace constants live in orbit-types so identity validation remains persistence-neutral.

## 2. Machine identity

The current file is:

    [machine]
    id = "hm_0123456789abcdef"
    name = "build-host"
    task_prefix = "BH"

### Field rules

- machine.id is generated once, starts with hm_, and accepts only an ASCII alphanumeric, underscore or hyphen suffix. Paths, hostnames, SSH targets and URIs are not valid substitutes.
- machine.name is a human display name. It is changeable and must be non-empty, trimmed, path-free and control-character-free.
- machine.task_prefix is chosen once for task allocation. Fresh values are two to five uppercase ASCII letters and cannot use reserved artifact namespaces. Existing migrated installations may retain ORB.
- The [machine] table has no machine mode. Topology is not a persisted identity decision.

orbit init is the creation and migration surface. A fresh non-interactive initialization requires both machine name and task prefix. Repeated initialization returns the existing identity without rewriting it.

The legacy ~/.orbit/host.toml file is folded into the [machine] table in ~/.orbit/config.toml, then removed when the write succeeds. Its host_id becomes machine.name; a missing machine_id is generated, and a missing task_prefix becomes the legacy ORB namespace. If host.toml disagrees with an existing [machine] table, loading fails and asks the operator to reconcile the files rather than choosing one.

Strict consumers call load_machine_identity. An absent identity, or a malformed or incomplete [machine] table, is an error rather than a hostname fallback. inspect_machine_identity exposes an absent identity for bootstrap callers and attempts the legacy host.toml migration. MCP identity presentation is intentionally more tolerant: an absent local identity may be represented for audit as host/local, but that fallback is not persisted. A valid legacy identity is used from memory if the migration cannot write.

### Display-name changes

machine.name is the changeable display name. `orbit config set --global machine.name <value>` changes it while leaving machine.id and machine.task_prefix intact. `orbit host rename` renames remote host entries only and refuses the local host, and workspaces.json no longer maintains an owner_host_ids display-name projection.

### Task-prefix composition

RegisteredRuntimeFactory projects task_prefix into the global task allocator before opening a runtime. A pristine legacy allocator may adopt the configured prefix. Once allocation or task bindings have begun, a conflicting prefix fails closed rather than renaming issued IDs.

The prefix is also the unit of task *authority* across hosts: the host whose prefix an id carries is that task's sole writer, and copies on other hosts are read-only mirrors. That model, and the export/import consequences, live in [task-migration](../task-migration/4_decisions.md). The prefix is also specified as the routing key: an id-addressed task call goes to the host its prefix names ([specs/host-routing.md](./specs/host-routing.md), [ORB-14449], not yet implemented). The host file already records each registered host's prefix and refuses duplicates, so that table exists before the routing does.

## 2a. Remote hosts

`~/.orbit/hosts.toml` (schema version 1) is the operator's record of remote Orbit hosts, beside workspaces.json. The full contract is [specs/host-commands.md](./specs/host-commands.md); this section summarises what the code does.

- An entry has exactly `name`, `machine_id`, `ssh` and `task_prefix`. `machine_id` and `task_prefix` are read from the remote itself by `orbit host add`; nothing that can change on the remote (reachability, version, pull protocol, workspaces) is stored.
- Load validates every entry: `name` passes the machine-name validator and is unique case-insensitively across entries and the local `machine.name`; `machine_id` is valid, unique (`ambiguous_destination`) and never the local id (`host_is_local`); `task_prefix` is a valid stored prefix, unique, and never the local prefix (`task_prefix_conflict`); `ssh` is an SSH alias or `user@host` with no leading `-`, whitespace or shell metacharacters. Unknown keys and any other `schema_version` fail the load and keep the bytes.
- Writes validate first, serialize canonically sorted by name, and replace the file atomically under a lock. The commit re-reads both host files under that lock and refuses if either changed since the command loaded them.
- `orbit_registry::hosts::load_host_routes` is the one loader for federated serve, follower pull drains and replica worktree GC.
- For one release, when only the legacy `~/.orbit/mcp-destinations.toml` exists its rows are read as routes with no name or prefix. The first `orbit host add`, `rename` or `remove` probes every retained legacy row, writes them to the host file (named after the remote's `machine.name`, or its SSH target when that is taken) and deletes the legacy file. Adding a host already in the legacy file commits and reports the migration; removing a legacy row never contacts that host, with or without `--force`. If a retained row does not answer, the mutation refuses with `legacy_host_unreachable` and changes neither file. Doctor names a concrete `orbit host add <existing-ssh-target>` command to migrate the file. Both files at once fail every load with `host_file_conflict`.

The `orbit host` operations live in orbit-cmd so the CLI and the dashboard share them. They probe a host with the federated MCP session (`ssh -T -- <target> orbit mcp serve --remote-caller-machine-id <local id>`, agent authority, federated probe budget) and read the discovery envelope's `machine_id`, `machine_name`, `task_prefix`, `binary_version` and `protocol_fingerprint`. A probe that answers with a different `machine_id` or `task_prefix` than the entry fails closed with `host_identity_mismatch`; the entry is never rewritten from a probe.

## 3. Workspace catalog

~/.orbit/workspaces.json has schema version 1 and three distinct parts:

- workspaces are logical records: stable ID, name, owner_machine_id, Git/ship metadata, lifecycle status and timestamps;
- checkouts are machine-local bindings: workspace ID, repo_root, orbit_dir, role, optional replica owner and path overrides;
- publication_bindings are optional owner-local bindings to dedicated task-publication repositories.

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
- An unversioned legacy catalog can be split into logical workspaces and local checkouts and written back atomically when its role is unambiguous. Identity-bearing legacy data with no explicit checkout role is rejected rather than guessed. A legacy owner_host_ids key is accepted and dropped during the current compatibility release.
- Successful saves validate a clone, serialize canonical JSON, and use atomic replacement. Rejected mutations leave the prior file intact.
- Canonicalization sorts and deduplicates path overrides.

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

The main CLI opens ordinary runtimes through RegisteredRuntimeFactory using cwd, --root and optional --workspace. `task show` is the one exception, on both the human subcommand and `orbit tool run orbit.task.show`: without `--workspace` or a tool-input `workspace` it opens the checkout the coordination task registry names as the task ID's owner, so it works from a foreign checkout, a linked worktree, and from a directory that is no workspace at all, and it reports the owning workspace name and logical ID. With an explicit workspace selector it is the ordinary registered bootstrap, and the selector filters. Linked-worktree runtime identities are not selectors. Workspace init, sync, role, list, show, source-remote, publication, remove and teardown are the current workspace CLI surface. `orbit host add|list|show|rename|remove` manages remote hosts ([§2a](#2a-remote-hosts), [specs/host-commands.md](./specs/host-commands.md), [ORB-14448]). It opens no workspace runtime, leaves `[machine]` alone, and lists the local host first, read in-process; change the machine display name with `orbit config set --global machine.name <value>`. `orbit doctor` adds a `hosts` row that probes every registered host. `workspace init --role replica` reports whether the owner has a host entry and never adds one. There is no workspace owner-link command.

### Web

The specified Settings › Hosts view and `/api/hosts` routes manage the serving host's host file through the same registry operations as `orbit host` ([specs/host-commands.md](./specs/host-commands.md#dashboard), [ORB-14451]).

Orbit Web loads local workspaces from orbit-registry, derives checkout-path health, and opens active runtimes lazily through orbit-cmd. Each request pins one immutable registry generation. A successful refresh swaps the complete snapshot and evicts incompatible cached runtimes; a failed refresh retains the last valid snapshot. An invalid initial load fails startup.

### MCP

orbit-mcp reads orbit-registry identity state to describe the accepting process and exposes machine-local discovery definitions. The CLI MCP server resolves each workspace-scoped call against the accepting machine's registry, composes the runtime, and dispatches through Core. `orbit.task.show` is the exception: an id-only call follows the globally unique task ID through the host task registry. Ambient initialize metadata — including a linked-worktree runtime identity — is not a filter. An explicit per-call `workspace` remains fail-closed.

orbit.workspace.list returns active logical workspaces that have a checkout registered on the accepting machine. Its envelope carries the accepting machine's `machine_id` and, additively, its `machine_name`, `task_prefix`, `binary_version` and `protocol_fingerprint`; the private federated discovery path carries the same keys. This includes locally registered replicas and excludes active checkoutless catalog entries; owner_machine_id is not a discovery filter. Remote MCP uses direct SSH stdio, and the local proxy does not resolve workspaces, inspect checkouts or read the remote registry.

caller_machine_id and caller_ip are audit metadata. Neither participates in catalog ownership or authorization.

## 7. Database compatibility

Older databases may retain tables and migration records from the removed fleet-registry implementation. Current v1 identity, catalog, health, runtime selection, routing and authorization do not read them. Their presence must never be treated as current authority. Any cleanup still has to preserve supported database upgrades and the immutability of already-shipped Store migrations.

## 8. Failure summary

| Condition | Result |
|---|---|
| [machine] absent on strict path | Actionable initialization error |
| legacy host.toml is valid and config has no identity | Fold into [machine] and remove the legacy file when writable |
| legacy host.toml disagrees with [machine] | Error asking for manual reconciliation |
| [machine] malformed or incomplete | Error; no hostname fallback |
| workspaces.json absent | Empty registry |
| workspaces.json malformed, contradictory or future | Error; original bytes retained |
| selector unknown, ambiguous, inactive or checkoutless | Runtime construction refused |
| task prefix conflicts after allocation | Runtime construction refused |
| hosts.toml malformed, unknown key, future schema or duplicate name/id/prefix | Every consumer refuses; original bytes retained |
| hosts.toml and mcp-destinations.toml both present | `host_file_conflict` on every consumer |
| Web refresh cannot load registry | Last valid in-memory snapshot retained |
| initial Web registry load fails | Server startup fails |

## Task References

- [ORB-14448] host file and `orbit host` commands
- [ORB-14449] task-prefix routing and `--host` selection (specified)
- [ORB-14451] dashboard Settings › Hosts (specified)

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
