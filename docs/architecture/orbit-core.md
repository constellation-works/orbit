---
type: context
summary: Architecture map for orbit-core: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-26
tags: [architecture, orbit-core]
---

# orbit-core

## What it owns

Composes the runtime. `OrbitRuntime` owns a `RuntimeKernel` and derefs to it. Internal graph `runtime ← application ← adapter`, `composition` joins config/bootstrap/runtime/adapters. Surfaces are being moved off `OrbitRuntime`: a converted surface holds a `Core` and calls an app's `exec` (see [Core, apps and the chokepoint](#core-apps-and-the-chokepoint)).

## Layering

- `bootstrap` depends on `application` and never the reverse; application and adapter never name `bootstrap` or `composition`. Runtime names no higher layer.
- Application modules are ordered `contracts < task < job < automation < review < distributed < pull_drain < admission < workspace_auto < backlog_forecast < sweep_filing`. A lower module reaches a higher one only through a port trait the lower module owns and the higher one implements on `OrbitRuntime` (for example `task::TaskNoticePort`, `job::JobExperimentPort`).
- Modules outside that chain (`routines`, `auto_tasks`, `experiment`, `landing`, `codebase`, `resident`, `notice`, `evolution`, `agent_corpus`, `tag_instructions`) call downward freely and are reached from below only through such ports, except the one authorized forecast call in the next bullet.
- `backlog_forecast` has one authorized direct call into `codebase`: `runtime.task_riskiness_model()` in `application/backlog_forecast/assembly.rs`, the codebase risk model whose result answers `forecast_regression_prior`. The guard admits exactly `let risk_model = runtime.task_riskiness_model()?;` in that file. It refuses every other forecast-to-codebase path, grouped import, `super::` chain or inherent call, and every `codebase` path or call back into `backlog_forecast`. Its path checks cover production code, not `tests/`.
- The adapter sits above all of application, which reaches it only through application-owned ports (`contracts::ToolInvocationPort`, `pull_drain::drain::PullTransportPort`).
- `scripts/check-dependency-direction.sh` refuses paths, grouped trees, `super::` chains and calls to higher-module inherent methods that break these rules. Core never depends on `orbit-agent`, `orbit-cmd` or transport crates.

## Main modules and types

`application` (task, job, review, landing, distributed, pull_drain, admission, routines, search, notice), `app` (`friction`, `plugin`, `run`, `task`), `adapter`, `composition` (`Core` in `crates/orbit-core/src/composition/core.rs`), `bootstrap`, `runtime` (`OrbitRuntime` in `crates/orbit-core/src/runtime/mod.rs`). See ``crates/orbit-core/src/lib.rs``.

## Core, apps and the chokepoint

The entry every surface is being moved to ([design §2, §3](../design/architecture-restructure/2_design.md)). `friction`, `plugin`, `run` and `task` use this entry.

- **`Core`** (`orbit_core::Core`) is what composition returns for one opened workspace. It has no public field and no public method, so a surface can only pass it to an app's `exec`. It holds the `OrbitRuntime` privately. `composition::core_for_command` opens it for a process that runs one command, and `composition::core_for_service` for a long-lived server; `orbit-cmd`'s `RegisteredRuntimeFactory` hands both out beside the runtime it opened. The CLI gives a command its `Core` through the `ExecuteCore` trait, and the dashboard through the `WsCore` extractor.
- **`app/<subject>/`** is one app: `mod.rs` holds `Command`, `Outcome`, `Error` and one `exec(&core, command)`, and each verb's flow is a private child module that takes `&OrbitRuntime`. `app::friction::Command` has one variant per `FrictionVerb`; its fields stay named and typed as the verb's spec in `orbit-common` declares, and that spec stays the contract every surface shares. A sibling may call only what `mod.rs` re-exports.
- **`app/exec.rs`** is the chokepoint every `exec` runs through. It authorizes the command against its operation's governance rows (the command row for a command-line caller, then the tool row), starts the audit guard, runs the app, tells the guard the outcome, and maps the app's error into `orbit_core::Error`. A refusal is audited, because the guard starts first.
- **What is audited.** A command from a command-line `Core` gets the invocation row the CLI's own guard wrote before: `command` is the noun, `subcommand` the CLI path, the target is the noun's audit parameter, and reads are audited like writes. `exec` then marks the row as recorded, so the CLI guard writes no second one; that guard still writes the row of a command refused before it reaches `exec`. The row records the command's outcome, not a later failure to print it. A command from a service `Core` gets no invocation row, as before.
- **The three friction entries** (`orbit friction` in `orbit-cli`, the dashboard's friction API in `orbit-web`, and the tool handler `adapter/tool_host/friction_tools.rs`) call only `app::friction::exec`.
- **The task app** (`app/task/`) is the entry for the task noun and, until decisions get their own commands, the decision noun. `Command::Operation` and `Command::Decision` carry a registry verb and its fields: the two nouns have some ninety parameters between them, and their specs in `orbit-common` stay the contract, so the app does not restate them as typed variants. The other variants are the hand-wired commands: archive, flow, recheck-blocked, settle-merged, export, import, reindex and publication from `orbit task`, `Command::Record` for a dashboard write and `Command::Read` for the read models the dashboard takes. `orbit task` and `orbit decision` in `orbit-cli`, the dashboard's task and decision handlers in `orbit-web`, and the tool handlers `adapter/tool_host/task/mod.rs` and `adapter/tool_host/decision_tools.rs` call only `app::task::exec`. `POST /api/tasks` and `orbit task add` end in the same screened add.
- **What the task app's chokepoint does.** A task or decision operation names no tool row, because the tool entry it runs through decides that row; the command row is still checked for a command-line caller. A dashboard write is audited in the chokepoint as the `task.mutation` row the handler wrote before, one per command. A reservation that is not granted exits 3 and is audited as a failure, as before. The dashboard's operator-session check stays in its handler until the dashboard is given a `Core` of its own.
- **The run app** (`app/run/`) serves run and job commands, script grants and history, lock contention, and the dashboard's lease and run metrics handlers. Its command constructors pair each operation with its typed result. Ship mode resolution, submission and the receipt belong to one command. Log and lease notice subscriptions return a reader scoped to the admitted query.
- **The plugin app** (`app/plugin/`) receives the hand-written plugin CLI commands and dashboard list, toggle and panel requests. CLI reads and writes retain their invocation rows and admin attribution. The chokepoint authorizes dashboard toggles against the authenticated dashboard session and records each attempt as a `dashboard.operations` event. A panel read returns the cache window and a reader for the existing audited read-only tool path. Workspace plugin tools reach the same app from the generic tool entry, which owns their tool policy and invocation row. Host-only discovery and global MCP dispatch continue to operate without opening a workspace.
- **The workspace app** (`app/workspace/`) receives the hand-written `orbit workspace` commands that open a workspace, the dashboard's fleet task reads behind `GET /api/tasks/all`, and the workspace-claim tool verbs. Registry reads and writes keep their invocation rows. `remove` and a confirmed `teardown` are authorized against their governance rows in the chokepoint, and a `teardown` without `--confirm` is not. Removal and teardown run the CLI's own flow from `orbit-cmd`, which the app calls once it has admitted the command.
- **Still on the runtime.** The owner lookup of `orbit task retry-handoff` takes the runtime from the CLI's dispatch context, and the dashboard's task projections take it from the `Ws` extractor. The handlers moved into `app/task/` still use the tool host's input and JSON helpers (`adapter/tool_host/input.rs`, `json.rs`).
- **Until the generic tool entry is converted**, a surface's friction command still runs through it: the app passes the command to the tool entry, which checks the fields against the spec, applies tool policy, routes a worker's call to its owner and reaches the tool handler. The handler calls `exec` with a `Core` marked as inside the tool entry, and for that call the chokepoint only runs the flow, because the tool entry authorized and audited it. `app` reaches the tool entry through `contracts::ToolInvocationPort`.

## Dependencies

- Depends on (internal, production): `orbit-automation`, `orbit-clock`, `orbit-common`, `orbit-config`, `orbit-engine`, `orbit-exec`, `orbit-registry`, `orbit-runtime`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-cmd`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-core/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `app`: The apps, one per subject, each with `Command`, `Outcome`, `Error` and one `exec`, and the chokepoint that authorizes and audits every command; imports `application`, `runtime`, `composition` (for `Core` and its `Entry` only) and lower workspace crates.
- `adapter`: Protocol adapters translating tool-host, command, and engine-host requests into Core operations; imports `app`, `application`, `runtime`, and lower workspace crates (never `bootstrap` or `composition`).
- `application`: Directional domain operations exposed on `OrbitRuntime` over one workspace and host; imports `runtime` and lower workspace crates (never `adapter`, `bootstrap`, or `composition`).
- `bootstrap`: Workspace initialization, managed asset seeding, and forward-only startup migrations; imports `application`, `runtime`, and lower workspace crates (never `composition`).
- `composition`: Composition root joining resolved configuration, bootstrap, `RuntimeKernel`, `CoreApplicationPort`, and adapters; imports `application`, `bootstrap`, `runtime`, and lower workspace crates.
- `metrics`: Reliability and run metrics computed from persisted job-run data; imports `application`, `runtime`, `orbit-common`, `orbit-runtime`, `orbit-store`, and `orbit-types`.
- `runtime`: Re-export of `orbit-runtime` and the public `OrbitRuntime` wrapper over `RuntimeKernel`; imports lower workspace crates only (never `application`, `adapter`, `bootstrap`, or `composition`).

## Application module destinations

Where each entry of `crates/orbit-core/src/application/` goes in the [architecture restructure](../design/architecture-restructure/2_design.md) (§3, §5). These are targets: the sections above still describe the current layout. This table is deleted with `application/` (ORB-28450). `scripts/check-architecture-map.py` fails when an entry has no row.

`app::<subject>` is the module `app/<subject>` and `unit::<name>` is the module `unit/<name>`, both under `crates/orbit-core/src/`. `migrate` is the module of that name under the same root. Order: the declared chain `task < run < automation < review < distributed < pull_drain < admission < workspace_auto < backlog_forecast < sweep_filing` is numbered 1 to 10. `above task` means outside the chain and unordered, above `task` as today. `leaf` means the unit imports no `application/` entry. `between task and run` is `crew_pool`: it imports `task` and `run` calls it.

| Entry | Destination | Order |
|---|---|---|
| `admission` | `unit::admission` | 7 |
| `agent_corpus.rs` | `app::agent` | above task |
| `agents` | `app::agent` | above task |
| `application_authority.rs` | `app::auto_task` | above task |
| `attention` | `app::attention` | above task |
| `audit_event.rs` | `app::audit` | above task |
| `audit_event` | `app::audit` | above task |
| `auto_tasks` | `app::auto_task` | above task |
| `automation` | `app::automation` | 3 |
| `backlog_forecast` | `unit::backlog_forecast` | 9 |
| `change_watch.rs` | `unit::change_watch` | leaf |
| `codebase` | `app::codebase` | above task |
| `config` | `app::config` | above task |
| `contracts` | dissolved: each shape goes to the app that owns it | — |
| `crew_memory` | `app::crew_memory` | above task |
| `distributed` | `unit::distributed` | 5 |
| `evolution` | `app::evolution` | above task |
| `executor.rs` | `unit::executor` | leaf |
| `experiment` | `app::experiment` | above task |
| `gc` | `app::gc` | above task |
| `health` | `app::doctor` | above task |
| `host_labels.rs` | `unit::host_labels` | leaf |
| `job` | `app::run`; its `crew_pools` module goes to `unit::crew_pool` (between task and run) | 2 |
| `landing` | `unit::landing` | above task |
| `managed_assets` | `unit::managed_assets` | leaf |
| `notice` | `app::notice` | above task |
| `operator_input.rs` | `app::review` | 4 |
| `plugin` | `app::plugin` | above task |
| `pull_drain` | `unit::pull_drain` | 6 |
| `resident` | `app::resident` | above task |
| `review` | `app::review` | 4 |
| `routines` | `app::routine` | above task |
| `search` | `app::search` | above task |
| `seeding_policy.rs` | `unit::seeding_policy` | above task |
| `skill` | `app::skill` | above task |
| `statistics.rs` | `unit::statistics` | leaf |
| `store_opener.rs` | `unit::store_opener` | leaf |
| `sweep_filing` | `unit::sweep_filing` | 10 |
| `tag_instructions.rs` | `unit::tag_instructions` | above task |
| `task` | `app::task` | 1 |
| `task_bundle` | `app::task` (bundle export, import, publication, reindex); its `migration.rs` → `migrate` | 1 |
| `test_support.rs` | dissolved: test-only fixtures move with their users | — |
| `tests` | dissolved: test-only; each module moves with what it tests | — |
| `tool_name_migration` | `migrate` | — |
| `vision_probe.rs` | `app::agent` | above task |
| `workflow.rs` | `app::run` | 2 |
| `workspace` | `app::workspace` | above task |
| `workspace_auto` | `unit::workspace_auto` | 8 |

New apps with no `application/` entry: `friction` (built, `app/friction/`), `host`, `update`, `remote_command`, which takes `app/run/command.rs` out of the run app ([design §3](../design/architecture-restructure/2_design.md#the-remote-command-gets-its-own-app)), and `lease`, which takes `app/run/lease.rs` out of it ([design §3](../design/architecture-restructure/2_design.md#leases-get-their-own-app-and-lock-contention-goes-to-the-task-app)). They have no row, since there is no entry.

A module beside `application/`: `metrics` dissolves into `app::run` (order 2) in two steps, ORB-28962 then ORB-28963 ([design §3](../design/architecture-restructure/2_design.md#metrics-dissolves-into-the-run-app)). It has no row, since it is not an `application/` entry.

### Decided by callers

Callers were checked at base `2f18287`, by module path and by inherent-method name across `crates/`. Method names are matched by name, so counts can include port calls.

- `task_bundle`: corrected from `migrate`. Only its `migration.rs` is legacy. The rest is the `orbit task` bundle group (export, import, publication, reindex), called from `orbit-cli`.
- Single files, decided because the starting table left them open:
  - `application_authority.rs` → `app::auto_task`: its only operations are auto-task add and update under application consent.
  - `agent_corpus.rs` → `app::agent`: implements `task`'s `TaskCorpusPort`, so the higher app implements it.
  - `change_watch.rs` → `unit::change_watch`: reads task and run stamps, so it spans two apps; the dashboard is its only caller.
  - `executor.rs` → `unit::executor`: used by `job`, `vision_probe` (agents) and bootstrap seeding.
  - `host_labels.rs` → `unit::host_labels`: used by `task` and `job`.
  - `operator_input.rs` → `app::review`: its only app user is `review`; the dashboard and the `landing` unit also use it.
  - `seeding_policy.rs` → `unit::seeding_policy`: used by `health`, `auto_tasks`, `workspace` and `routines`.
  - `statistics.rs` → `unit::statistics`: used by `agents`, `experiment`, `resident` and `backlog_forecast`.
  - `store_opener.rs` → `unit::store_opener`: composition supplies the opener used by both `routines` and `workspace`.
  - `tag_instructions.rs` → `unit::tag_instructions`: used by `experiment`, `agent_corpus` and `distributed`.
  - `vision_probe.rs` → `app::agent`: backs `orbit agents vision-probe`.
  - `workflow.rs` → `app::run`: the ship and auto workflow and drain-window rules; `job` is the only app user, and `distributed` and `workspace_auto` are units above `run` that reach it downward.
- Kept as units though no app imports them directly: `pull_drain`, `workspace_auto`, `sweep_filing`, `backlog_forecast` and `landing`. Their callers are units, the engine action dispatch, the runtime or the dashboard. `workspace_auto` stays above `admission` and `pull_drain`, which it calls.
- Kept as an app: `crew_memory`. Its engine adapter and `experiment` callers are internal, but its run-bound crew memory tools are a user-facing surface.
- `metrics` → `app::run`, checked at `84f63223cd`: the only production caller of its reliability reads is `app::run::Command::fleet_reliability`; `orbit-web` names it in four files, one of which calls `aggregate`; its `cost` is an alias for the runtime's module.
- Existing edge to resolve with the move: `distributed` calls `auto_tasks::job_step` directly, and `auto_tasks` is unordered.
