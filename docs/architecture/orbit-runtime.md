---
type: context
summary: Architecture map for orbit-runtime: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-runtime]
---

# orbit-runtime

## What it owns

Runtime kernel: `RuntimeKernel`, stores, engine handles, event bus, audit, claims, reservations, plugin host, workspace binding, host probes, process/tool execution. Never depends on `orbit-core`; finishes use-cases via `ApplicationPort`. The kernel's state is four layer structs, `Base`, `Stored`, `Host` and `Run`, each holding the one below; see [Runtime layers](#runtime-layers).

## Main modules and types

`orbit_runtime` (`RuntimeKernel`), `base` (`Base`, config, paths, actor, binding, event log, authorization policy data), `stored` (`Stored`, stores, persistence, authorization audit, `job_runs`, `workspace`, `audit`, `recovery_authority`), `host` (`Host`, execution assets, context assembly, tool authorization), `run` (`Run`, execution identity and coordination ownership), `builder`, `engine`, `plugin`, `application_port` (`ApplicationPort`), `cost`. See ``crates/orbit-runtime/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-config`, `orbit-engine`, `orbit-exec`, `orbit-policy`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-core`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-runtime/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Runtime layers

The kernel's state is four structs, each holding the one below it and its own fields. A lower layer is reached by an accessor (`rt.base()`, `rt.stored()`, `rt.host()`, `rt.run()`); none implements `Deref` to another.

| Layer | Module | Holds | An operation that needs it |
|---|---|---|---|
| `Base` | `base` | the resolved configuration, paths, actor, clock, event log, host OS and workspace binding | every operation |
| `Stored` | `stored` | `Base` and the opened stores | one that reads or writes a record |
| `Host` | `host` | `Stored` and this machine as a place work runs | one that starts, validates or lands work |
| `Run` | `run` | `Host` and the run this process carries | code inside a drain or a worker |

Job runs, local pull admissions and drain intent are reached through `stores().jobs()`, a `JobRuns` handle (`crates/orbit-runtime/src/stored/job_runs/mod.rs`). It runs each admission, lifecycle, checkpoint-state and pull-occupancy rule inside one `Store::write`, over the store's job-run, local-pull, drain-intent and admission-load repositories.

`RuntimeKernel` holds `Run`, the `application` callback and the three test hooks, and nothing else. Its methods are unchanged, and each reads its state through the layer that holds it. A field goes to the lowest layer in which every operation could use it. A field that is optional today stays optional.

### Where each former kernel field lives

The 34 fields `RuntimeKernel` had before the carve. `context` was one field holding five parts, which are listed separately because they went to three layers.

| Former field | Now in | Why that layer |
|---|---|---|
| `context` (paths) | `Base` | every operation resolves paths |
| `context` (policy engine, child-environment and Codex execution policy) | `Base` | resolved configuration |
| `context` (settings, which carry the actor) | `Base` | resolved configuration and the actor |
| `context` (stores) | `Stored` | the stores |
| `context` (tool registry, skill catalog, plugin load) | `Host` | execution assets |
| `config_loader` | `Base` | reloads the resolved configuration |
| `event_log` | `Base` | the event log |
| `ci_failure_time` | `Base` | a clock override |
| `host_os` | `Base` | the host OS |
| `registered_host_oses` | `Base` | the registry's host OS list, read beside `host_os` |
| `workspace_binding` | `Base` | the workspace binding |
| `workspace_explicitly_selected` | `Base` | how the bound workspace was chosen |
| `opened_registered_workspace` | `Base` | the bound workspace's registry identity |
| `composition_snapshot` | `Base` | data composition loaded at open; needs no store |
| `_temp_dir` | `Base` | keeps an in-memory runtime's paths alive |
| `write_free` | `Stored` | how the stores were opened |
| `workspace_catalog` | `Stored` | record reads that span workspaces |
| `layout_report` | `Stored` | the layout pre-flight that ran when the stores opened |
| `task_pr_forge` | `Host` | the PR forge |
| `host_signals` | `Host` | host signals |
| `host_load` | `Host` | the load probe |
| `host_resources` | `Host` | the resource probe |
| `resource_profiles` | `Host` | resource profiles |
| `admission_policy` | `Host` | the cached admission policy |
| `admission_policy_snapshot` | `Host` | the admission policy pinned for one pass |
| `browser_kit_home` | `Host` | read by admission to place `ui` work on this machine |
| `claim_known_red_budget` | `Host` | the time budget of a base validation run |
| `validated_trees` | `Host` | validated trees |
| `pr_repair_checks` | `Host` | PR repair checks |
| `worker_invocation` | `Run` | the worker invocation |
| `owner_coordinator` | `Run` | the owner coordinator |
| `drain_owner_transport` | `Run` | the drain owner transport |
| `coordination_write_owner` | `Run` | the coordination write owner |
| `automation_execution_location` | `Run` | the execution location |
| `application` | `RuntimeKernel` | the callback into `orbit-core`; not layered |
| `orphan_reconcile_hook` | `RuntimeKernel` | test hook; not layered |
| `transition_read_hook` | `RuntimeKernel` | test hook; not layered |
| `transition_run_hook` | `RuntimeKernel` | test hook; not layered |

Six of those fields were public. Callers outside the crate now read them through accessors on `RuntimeKernel` that return the same value with the same borrow:

| Former field read | Accessor |
|---|---|
| `context.settings()` | `settings()` |
| `context.persistence()` | `persistence()` |
| `context.paths()` | `paths()` |
| `context.data_root()` | `data_root_path()` |
| `context.apply_child_env_defaults(..)` | `apply_child_env_defaults(..)` |
| `event_log` | `event_log()` |
| `ci_failure_time` | `ci_failure_time()` |
| `claim_known_red_budget` | `claim_known_red_budget()` |
| `validated_trees` | `validated_trees()` |
| `pr_repair_checks` | `pr_repair_checks()` |

## Ownership by module

- `activity_catalog`: Activity catalog resolving `target: activity:<name>` definitions; imports `assets` and `orbit-engine`.
- `application_consent`: Application consent validation, host authority, and constrained call admission; imports `orbit_runtime`, `plugin`, `orbit-common`, `orbit-store`, `orbit-tools`, and `orbit-types`.
- `application_port`: Callback port (`ApplicationPort`) the kernel invokes into Core use cases without depending on `orbit-core`; imports `orbit_runtime`, `orbit-common`, `orbit-engine`, `orbit-store`, and `orbit-types`.
- `assets`: Embedded built-in activity definitions available to runtime and bootstrap layers; imports `orbit-types` only.
- `base`: `Base`, the lowest runtime layer: resolved configuration and `config.toml` path selection (`config`), path normalization, working-directory confinement and root discovery (`path`), actor identity (`actor`, `identity`), workspace binding (`binding`), session event log (`event`), authorization policy data and execution policy context (`policy`), and base accessors (`accessors`); imports `builder`, `host_signal`, `run_input`, `task`, and lower workspace crates (never `stored`, `host`, or `run`).
- `builder`: `RuntimeKernel` assembly wiring stores, policy, tools, plugins, and workspace bindings; imports `base`, `stored`, `host`, `plugin`, `task`, and lower workspace crates.
- `command_exec`: Claim-gated remote command execution entry point (`orbit.command.exec`); imports `orbit_runtime`, `orbit-common`, and `orbit-types`.
- `cost`: Managed-execution cost and token accounting plus shared dashboard time windows; imports `chrono`, `orbit-common`, `orbit-types`, and `serde` only.
- `engine`: Engine execution helpers, crew projections, environment host, and invocation metrics on `RuntimeKernel`; imports `orbit_runtime`, `cost`, `run_input`, and lower workspace crates.
- `friction`: Workspace-scoped friction commands and projections over the host store snapshots and transactions; imports `orbit_runtime`, `stored`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `git_sandbox`: Host-owned Git write boundaries for macOS and Linux provider sandboxes; imports `orbit-common` and `orbit-types` only.
- `host`: `Host`, the runtime layer over `Stored` for this machine as a place work runs: PR forge, host signals, load and resource probes, admission policy, execution assets, `OrbitContext` assembly (`context`), host accessors (`accessors`), and tool-registry authorization (`authorization`); imports `base`, `stored`, `host_resource`, `host_signal`, `orbit_runtime`, `plugin`, `task`, `tool_exec`, and lower workspace crates.
- `host_resource`: Host resource probes, shared pressure history, learned profiles, and load forecasts; imports `run_input`, `orbit-common`, `orbit-config`, `orbit-store`, and `orbit-types`.
- `host_signal`: Host lifecycle signal probes (such as scheduled shutdown/reboot) gating new admissions; imports `std`, `chrono`, and `serde` only.
- `mutation`: `RuntimeKernel` mutation wrappers that append session `OrbitEvent` entries; imports `orbit_runtime`, `orbit-common`, and `orbit-types`.
- `orbit_runtime`: `RuntimeKernel`, which holds `Run`, its layer accessors, and its method groups for construction, admission policy, validation warnings, and host probes; imports sibling runtime modules and lower workspace crates.
- `plugin`: Host plugin contributions, recorded grants, install layout, and runtime load pass; imports `assets`, `base`, `stored`, and lower workspace crates.
- `recovery_decision`: Durable step-recovery decisions (`step_failure_recovery`); imports `orbit-common`, `orbit-engine`, and `orbit-types`.
- `run`: `Run`, the top runtime layer over `Host` for the run this process carries: worker invocation, owner coordinator, drain owner transport, execution location, and coordination ownership (`identity`); imports `base`, `stored`, `host`, `orbit-tools`, and `orbit-types`.
- `run_input`: Managed-run environment marker parsing (`ORBIT_MANAGED_RUN_CONTEXT`, run/task IDs); imports `std`, `serde_json`, and `orbit-common` only.
- `scripts`: Host-local script grant consent keyed by workspace script content digest; imports `orbit-common`, `orbit-engine`, `orbit-exec`, `orbit-tools`, and `orbit-types`.
- `stored`: `Stored`, the runtime layer over `Base` that holds the opened stores (`stores`), persistence readiness and audit stores (`persistence`), store-backed authorization (`authorization`), stored accessors (`accessors`), job runs (`job_runs`), workspace catalog and claims (`workspace`), runtime audit projections and system audit seams (`audit`), and host-only rebase-recovery authority (`recovery_authority`); imports `base`, `builder`, `run_input`, `task`, `orbit-store`, and lower workspace crates (never `host` or `run`).
- `task`: Task reservation, PR forge closure, and run-coupling behavior on `RuntimeKernel`; imports `orbit_runtime`, `stored`, `run_input`, and lower workspace crates.
- `tool_exec`: Registered tool execution, capability enforcement, and redaction on `RuntimeKernel`; imports `orbit_runtime`, `base`, `run_input`, `orbit-common`, `orbit-engine`, `orbit-tools`, and `orbit-types`.
- `upgrade_handover`: Detached pipeline worker drain participation during Orbit binary upgrades; imports `orbit_runtime`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `worker_coordination`: Managed-worker owner-coordinator RPC retries and test wait seams on `RuntimeKernel`; imports `orbit_runtime`, `orbit-common`, `orbit-tools`, and `orbit-types`.
