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

`base` (`Base`, `Role`, config, paths, actor, binding, event log, authorization policy data, process role, managed-run env parsing), `stored` (`Stored`, stores, persistence, authorization audit, `task_access`, `job_runs`, `workspace`, `audit`, `recovery_authority`, `locks`, `friction`), `host` (`Host`, `forge`, `signal`, `resource`, `admission`, `application_consent`, `probes`, `exec`, `plugin`, execution assets, context assembly, tool authorization), `run` (`Run`, `worker_coordination`, `engine`, `recovery_decision`, `run_input`, `tool_exec`, `upgrade_handover`, `cost`, `task_coupling`, execution identity and the drain owner transport), `orbit_runtime` (`RuntimeKernel`), `builder`, `application_port` (`ApplicationPort`). See ``crates/orbit-runtime/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-config`, `orbit-engine`, `orbit-exec`, `orbit-policy`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-core`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-runtime/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Runtime layers

The kernel's state is four structs, each holding the one below it and its own fields. A lower layer is reached by an accessor (`rt.base()`, `rt.stored()`, `rt.host()`, `rt.run()`); none implements `Deref` to another.

| Layer | Directory | Holds | An operation that needs it |
|---|---|---|---|
| `Base` | `crates/orbit-runtime/src/base/` | resolved configuration (`base/config/`), paths and root discovery (`base/path/`), actor identity (`base/actor.rs`, `base/identity.rs`), workspace binding (`base/binding.rs`), event log (`base/event.rs`), authorization policy data (`base/policy.rs`), process role (`base/role.rs`), managed-run env parsing (`base/managed_env.rs`), the lease policy and unlocked context paths (`base/config/`), declared context selectors (`base/path/declared_context.rs`), mutation event wrappers (`base/mutation.rs`), and base accessors (`base/accessors.rs`) | every operation |
| `Stored` | `crates/orbit-runtime/src/stored/` | `Base` plus the opened stores (`stored/stores.rs`), persistence readiness (`stored/persistence.rs`), store-backed authorization (`stored/authorization.rs`), the task access chosen for the process's role (`stored/task_access.rs`), `stored/job_runs/`, `stored/workspace/`, `stored/audit/`, `stored/recovery_authority/`, reservations, context locks and path leases (`stored/locks/`, `stored/reservation_history.rs`), the friction repository (`stored/friction/`), and stored accessors (`stored/accessors.rs`) | one that reads or writes a record |
| `Host` | `crates/orbit-runtime/src/host/` | `Stored` plus this machine as a place work runs: PR forge (`host/forge/`), lifecycle signals (`host/signal.rs`), resource probes/profiles/forecasts (`host/resource/`), cached admission policy (`host/admission.rs`), host probes (`host/probes.rs`), command/tool/script/Git-sandbox execution, embedded assets and the activity catalog (`host/exec/`), application consent and call admission (`host/application_consent/`), host plugins (`host/plugin/`), context assembly (`host/context.rs`), tool authorization (`host/authorization.rs`), and host accessors (`host/accessors.rs`) | one that starts, validates or lands work |
| `Run` | `crates/orbit-runtime/src/run/` | `Host` plus the run this process carries: execution location (`run/identity.rs`), worker coordination (`run/worker_coordination.rs`), engine execution and crew helpers (`run/engine/`), step-recovery decisions (`run/recovery_decision.rs`), run-input task IDs (`run/run_input.rs`), worker-routed tool dispatch (`run/tool_exec.rs`), upgrade handover (`run/upgrade_handover.rs`), cost accounting (`run/cost.rs`), and the tasks coupled to a run (`run/task_coupling/`) | code inside a drain or a worker |

Layer direction is enforced by `scripts/check-dependency-direction.sh`: `base/` names none of `stored`, `host` or `run`; `stored/` names only `base`; `host/` names only `base` and `stored`; `run/` may name any lower layer.

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
| `task_route` | `Stored` | task read routing, projection visibility and coordination write authority, chosen once from `Role` |
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
| `worker_invocation` | `Base` (`Role`) | resolved once into `Role::Worker` when `Base` is built |
| `owner_coordinator` | `Stored` (`task_route`) | a worker's task reads go through the owner, and a task read needs only the stores layer |
| `drain_owner_transport` | `Run` | the drain owner transport |
| Replica checkout’s coordination owner | `Base` (`Role`) | `Role::Replica` carries the remote owner’s machine ID |
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

- `application_port`: Callback port (`ApplicationPort`) the kernel invokes into Core use cases without depending on `orbit-core`; imports `orbit_runtime`, `orbit-common`, `orbit-engine`, `orbit-store`, and `orbit-types`.
- `base`: `Base`, the lowest runtime layer: resolved configuration and `config.toml` path selection (`base/config/`), path normalization, working-directory confinement and root discovery (`base/path/`), actor identity (`base/actor.rs`, `base/identity.rs`), workspace binding (`base/binding.rs`), session event log (`base/event.rs`), process role resolved once (`base/role.rs`), authorization policy data and execution policy context (`base/policy.rs`), managed-run environment marker parsing (`base/managed_env.rs`), the path-lease policy and `workflow.unlocked_context_paths` the settings carry (`base/config/lease_policy.rs`, `base/config/unlocked.rs`), a task's canonical declared context selectors (`base/path/declared_context.rs`), the mutation wrappers that append session `OrbitEvent` entries (`base/mutation.rs`), and base accessors (`base/accessors.rs`); imports `builder` and lower workspace crates (never `stored`, `host`, or `run`).
- `builder`: `RuntimeKernel` assembly wiring stores, policy, tools, plugins, and workspace bindings; imports `base`, `stored`, `host`, and lower workspace crates.
- `host`: `Host`, the runtime layer over `Stored` for this machine as a place work runs: PR forge (`host/forge/`), host lifecycle signals (`host/signal.rs`), load and resource probes, profiles and forecasts (`host/resource/`), cached admission policy (`host/admission.rs`), host probe methods (`host/probes.rs`), remote command execution, tool authorization/policy context, Git write sandbox, script grants and embedded activity/job assets (`host/exec/`), the activity catalog resolving `target: activity:<name>` (`host/exec/activity_catalog.rs`), application consent validation, operator authority and constrained call admission (`host/application_consent/`), host plugins (`host/plugin/`), `OrbitContext` assembly (`host/context.rs`), host accessors (`host/accessors.rs`), and tool-registry authorization (`host/authorization.rs`); imports `base`, `stored`, `orbit_runtime`, and lower workspace crates (never `run`).
- `orbit_runtime`: `RuntimeKernel`, which holds `Run`, its layer accessors, construction (`orbit_runtime/build.rs`), and validation-environment warning cache (`orbit_runtime/validation_warning.rs`); imports sibling runtime modules and lower workspace crates.
- `run`: `Run`, the top runtime layer over `Host` for the run this process carries: drain owner transport (`run/mod.rs`, `run/worker_coordination.rs`), automation execution location (`run/identity.rs`), worker tool routing onto the owner route and the pipeline-poll test wait seam (`run/worker_coordination.rs`), engine execution helpers, crew projections, environment host and invocation metrics (`run/engine/`), step-recovery decisions (`run/recovery_decision.rs`), run-input task IDs (`run/run_input.rs`), worker-routed tool dispatch (`run/tool_exec.rs`), detached worker upgrade handover (`run/upgrade_handover.rs`), managed-execution cost and token accounting plus the dashboard time window (`run/cost.rs`), and the tasks coupled to a run (`run/task_coupling/`: reservation cleanup when the run reaches a terminal state, blocking or requeueing its tasks, provider holds); imports `base`, `stored`, `host`, `orbit_runtime`, and lower workspace crates.
- `stored`: `Stored`, the runtime layer over `Base` that holds the opened stores (`stored/stores.rs`), persistence readiness and audit stores (`stored/persistence.rs`), store-backed authorization (`stored/authorization.rs`), stored accessors (`stored/accessors.rs`), the task access (`stored/task_access.rs`: `Stored::task()` reads the local store or, for a claimed worker, the drain owner. Replica checkouts hide coordination listings. Only operators and drain owners get a local coordination writer; `stored/task_access/owner.rs` holds the owner route, its coordinator, the lock-busy read retry and its test wait seam; `stored/task_access/metadata.rs` holds the body-less metadata reads and the paged owner read), job runs (`stored/job_runs/`), workspace catalog and claims (`stored/workspace/`), runtime audit projections and system audit seams (`stored/audit/`), host-only rebase-recovery authority (`stored/recovery_authority/`), task reservations, context locks and path leases (`stored/locks/`), the reservation history read (`stored/reservation_history.rs`), and the workspace-partitioned friction repository (`stored/friction/`); imports `base`, `builder`, `orbit-store`, and lower workspace crates (never `host` or `run`).
