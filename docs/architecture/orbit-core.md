---
type: context
summary: Architecture map for orbit-core: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-27
tags: [architecture, orbit-core]
---

# orbit-core

## What it owns

Composes the runtime. `OrbitRuntime` owns a `RuntimeKernel` and derefs to it. Internal graph `runtime ← application ← adapter`, `composition` joins config/bootstrap/runtime/adapters. The crate exports `app`, `Core` and `Error` and nothing else (see [Public face](#public-face)): a surface holds a `Core` and calls an app's `exec` (see [Core, apps and the chokepoint](#core-apps-and-the-chokepoint)).

## Layering

- `bootstrap` depends on `application` and never the reverse; application and adapter never name `bootstrap` or `composition`. Runtime names no higher layer.
- Application modules are ordered `contracts < task < job < automation < review < distributed < pull_drain < admission < workspace_auto < backlog_forecast < sweep_filing`. A lower module reaches a higher one only through a port trait the lower module owns and the higher one implements on `OrbitRuntime` (for example `task::TaskNoticePort`, `job::JobExperimentPort`).
- Modules outside that chain (`app::routine`, `auto_tasks`, `experiment`, `landing`, `codebase`, `resident`, `notice`, `evolution`, `agent_corpus`, `tag_instructions`) call downward freely and are reached from below only through such ports, except the one authorized forecast call in the next bullet.
- `backlog_forecast` has one authorized direct call into `codebase`: `runtime.task_riskiness_model()` in `application/backlog_forecast/assembly.rs`, the codebase risk model whose result answers `forecast_regression_prior`. The guard admits exactly `let risk_model = runtime.task_riskiness_model()?;` in that file. It refuses every other forecast-to-codebase path, grouped import, `super::` chain or inherent call, and every `codebase` path or call back into `backlog_forecast`. Its path checks cover production code, not `tests/`.
- The adapter sits above all of application, which reaches it only through application-owned ports (`contracts::EngineHostPort`, `contracts::ToolInvocationPort`, `pull_drain::drain::PullTransportPort`). `adapter::engine_host::EngineHost` is the sole production implementor of `orbit-engine`'s host traits.
- `scripts/check-dependency-direction.sh` refuses paths, grouped trees, `super::` chains and calls to higher-module inherent methods that break these rules. Core never depends on `orbit-agent`, `orbit-cmd` or transport crates.

## Main modules and types

`application` (task, job, review, landing, distributed, pull_drain, admission, search, notice), `app` (`agent`, `attention`, `audit`, `auto_task`, `automation`, `codebase`, `config`, `crew_memory`, `doctor`, `evolution`, `experiment`, `friction`, `gc`, `host`, `lease`, `notice`, `plugin`, `remote_command`, `resident`, `review`, `routine`, `run`, `search`, `skill`, `task`, `tool`, `workspace`), `adapter`, `composition` (`Core` in `crates/orbit-core/src/composition/core.rs`), `bootstrap`, `runtime` (`OrbitRuntime` in `crates/orbit-core/src/runtime/mod.rs`). See ``crates/orbit-core/src/lib.rs``.

## Public face

- `lib.rs` declares `app` public and re-exports `Core` and `Error`. `adapter`, `application`, `bootstrap`, `composition` and `runtime` are crate-private in a shipped build, so a surface that names the runtime, the store or an application module does not compile; `orbit-cli` and `orbit-web` also have no edge to `orbit-runtime`, `orbit-store` or `orbit-engine`.
- A type or function a surface or `orbit-cmd` names is re-exported from the app that owns its subject, in the block headed "Surface re-exports" in `app/<subject>/mod.rs`. Composition's opening (`Opening`, `store_opener`, `core_for_request`) is re-exported from `app::workspace`, and the captured process environment from `app::host`. `orbit-cmd` names kernel items (the workspace catalog, root resolution) from `orbit-runtime` directly.
- The dev-only `test-support` feature makes the internal modules public, hidden from the docs, for integration fixtures, and `test_support` re-exports `orbit-store` and `orbit-engine` as `fixture_store` and `fixture_engine` for the CLI and dashboard fixtures. A shipped build has no such consumer, so it does not report an item only a fixture reaches as unused; a `test-support` build still reports a dead crate-private item.

## Core, apps and the chokepoint

The entry every surface is being moved to ([design §2, §3](../design/architecture-restructure/2_design.md)). `agent`, `attention`, `audit`, `auto_task`, `automation`, `codebase`, `config`, `crew_memory`, `doctor`, `evolution`, `experiment`, `friction`, `gc`, `host`, `lease`, `notice`, `plugin`, `remote_command`, `resident`, `review`, `routine`, `run`, `search`, `skill`, `task`, `tool` and `workspace` use this entry.

- **`Core`** (`orbit_core::Core`) is what composition returns for one opened workspace. It has no public field and no public method, so a surface can only pass it to an app's `exec`. It holds the `OrbitRuntime` privately. `composition::core_for_command` opens it for a process that runs one command, `composition::core_for_service` for a long-lived server that answers for no session, and `composition::core_for_session` derives one for a request from the server's cached runtime and the request's session. `orbit-cmd`'s `RegisteredRuntimeFactory` opens a workspace as a `composition::Opening`, attaches what the registry knows about it, and returns only the `Core`: an `Opening` offers the open modes, the facts composition attaches and the reads it needs to choose them, and leaves only as a `Core` or, for a kernel interface, the kernel. The CLI gives a command its `Core` through the `ExecuteCore` trait, and the dashboard through the `WsCore` extractor.
- **`app/<subject>/`** is one app: `mod.rs` holds `Command`, `Outcome`, `Error` and one `exec(&core, command)`, and each verb's flow is a private child module that takes `&OrbitRuntime`. `app::friction::Command` has one variant per `FrictionVerb`; its fields stay named and typed as the verb's spec in `orbit-common` declares, and that spec stays the contract every surface shares. A sibling may call only what `mod.rs` re-exports.
- **`app/exec/`** is the chokepoint every `exec` runs through, one stage to a file. `caller.rs` says who is asking: a command line, a session with its grant, or a tool call the generic tool entry already decided. `operation.rs` holds a command's identity, the requirements it declares and the dashboard row it declares. `admit.rs` is the only place a command is authorized: the command row for a command-line caller, each declared requirement in order, then the tool row; a flow asks for a requirement that depends on a record it has read (`exec::require`). `app/exec/audit/` chooses the row the caller is owed and writes it. `mod.rs` starts the audit, admits, runs the app, finishes the audit and maps the app's error into `orbit_core::Error`. A refusal is audited, because the audit starts first.
- **A session's refusal** is `OrbitError::AuthorizationDenied`, which carries the decision the dashboard renders as its 403 body. A command line keeps `CapabilityDenied` and its text. Two dashboard writes are refused before their workspace is opened, the lease write (`app::lease::admit`) and the run replay (`app::run::admit_dashboard_replay`): the decision needs only the session and records nothing, and `exec` decides it again.
- **What is audited.** A command from a command-line `Core` gets the invocation row the CLI's own guard wrote before: `command` is the noun, `subcommand` the CLI path, the target is the noun's audit parameter, and reads are audited like writes. `exec` then marks the row as recorded, so the CLI guard writes no second one; that guard still writes the row of a command refused before it reaches `exec`. The row records the command's outcome, not a later failure to print it. A command from a service `Core` gets no invocation row, as before.
- **The three friction entries** (`orbit friction` in `orbit-cli`, the dashboard's friction API in `orbit-web`, and the tool handler `adapter/tool_host/friction_tools.rs`) call only `app::friction::exec`.
- **The task app** (`app/task/`) is the entry for the task noun and, until decisions get their own commands, the decision noun. `Command::Operation` and `Command::Decision` carry a registry verb and its fields: the two nouns have some ninety parameters between them, and their specs in `orbit-common` stay the contract, so the app does not restate them as typed variants. The other variants are the hand-wired commands: archive, flow, locks contention, recheck-blocked, settle-merged, export, import, reindex and publication from `orbit task`, `Command::Record` for a dashboard write and `Command::Read` for the read models the dashboard takes, whose list, detail and "why stuck" JSON `app/task/projection.rs` and `app/task/why_stuck.rs` render. `orbit task` and `orbit decision` in `orbit-cli`, the dashboard's task handlers and its decision answer write in `orbit-web`, and the tool handlers `adapter/tool_host/task/mod.rs` and `adapter/tool_host/decision_tools.rs` call only `app::task::exec`. The dashboard's decision list (`GET /api/decisions` via `RunCommand::decision_requests`) and its answer handler's preliminary lookup (`RunCommand::decision_request`) are `app::run` reads in `app/run/dashboard.rs`; only the answer write (`DashboardAction::AnswerDecision`) goes through `app::task::exec`. `POST /api/tasks` and `orbit task add` end in the same screened add.
- **What the task app's chokepoint does.** A task or decision operation names no tool row, because the tool entry it runs through decides that row; the command row is still checked for a command-line caller. A dashboard write is audited in the chokepoint as the `task.mutation` row the handler wrote before, one per command. A reservation that is not granted exits 3 and is audited as a failure, as before. The dashboard's operator-session check stays in its handler until the dashboard is given a `Core` of its own.
- **The run app** (`app/run/`) serves run and job commands, script grants and history, and the dashboard's run metrics handlers (`Command::cost`, `Command::fleet_reliability`, and `Command::knowledge_metrics` for `GET /api/metrics/knowledge`), and exports the dashboard cost, spend, reliability and knowledge summary types ([ORB-28962]). `app/run/dashboard.rs` holds the dashboard's other reads as unaudited read commands, including the decision list and single-request lookup ([ORB-28415]), and the invocation ingest ([ORB-28415]). Its command constructors pair each operation with its typed result. Ship mode resolution, submission and the receipt belong to one command. Log subscriptions return a reader scoped to the admitted query.
- **The remote-command app** (`app/remote_command/`, [ORB-28971]) serves the `orbit.command.exec` tool operation from `adapter/tool_host/command_tools.rs`.
- **The lease app** (`app/lease/`, [ORB-28986]) serves the dashboard's path-lease operations (`request`, `renew`, `expand`, `release`, `show`, `list`, `check`, `notices`), the same operations from the Unix-socket intake (`Command::Intake`) and live notice feeds (`LeaseNoticeFeed`) from `crates/orbit-web/src/api/leases.rs` and `crates/orbit-web/src/api/lease_notices.rs`. Dashboard writes keep early `DASHBOARD_LEASE_WRITE` admission through `Command::write` before the workspace is opened.
- **The plugin app** (`app/plugin/`) receives the hand-written plugin CLI commands and dashboard list, toggle and panel requests. CLI reads and writes retain their invocation rows and admin attribution. The chokepoint authorizes dashboard toggles against the authenticated dashboard session and records each attempt as a `dashboard.operations` event. A panel read returns the cache window and a reader for the existing audited read-only tool path. Workspace plugin tools reach the same app from the generic tool entry, which owns their tool policy and invocation row. Host-only discovery and global MCP dispatch continue to operate without opening a workspace.
- **The workspace app** (`app/workspace/`) receives the hand-written `orbit workspace` commands that open a workspace, `orbit migrate --confirm` and its backfill flags (`MigrateStatus`, diff-stats/crew/effort backfills), the dashboard's fleet task reads behind `GET /api/tasks/all`, and the workspace-claim tool verbs. Registry reads and writes keep their invocation rows. `remove` and a confirmed `teardown` are authorized against their governance rows in the chokepoint, and a `teardown` without `--confirm` is not. Removal and teardown run the CLI's own flow from `orbit-cmd`, which the app calls once it has admitted the command.
- **The tool app** (`app/tool/`) receives `orbit tool` (`list`, `show`, `add`, `remove`, `enable`, `disable`, `doctor`, `run`, `--dry-run`), plugin CLI group invocations, and the MCP server's workspace tool dispatch (`ServerMcpHost` caches `Arc<Core>` and calls `app::tool::exec`, `app::friction::exec`, `app::plugin::exec` and `app::agent::exec`). `OrbitToolHost` is implemented by `CoreOrbitToolHost` holding a tool-entry `Core`, and `adapter/tool_host/dispatch.rs` dispatches each noun to its app's `exec` using that `Core`.
- **The config app** (`app/config/`) receives `orbit config` (`show`, `get`, `set`, `keys`, `path`) and the dashboard's Config tab reads and writes (`GET /api/config/{effective,file,keys}`, `PUT/DELETE /api/config/keys/:key`, `PUT/DELETE /api/config/crews/:name`). CLI commands retain their invocation audit rows; dashboard writes authorize against `DASHBOARD_CONFIG_SET` and record their `dashboard.operations` row (with `old_value` and `new_value` on applied writes) in `app/exec/audit/dashboard.rs`.
- **The skill app** (`app/skill/`) receives `orbit skill` (`list`, `show`, `doctor`, `link`, `unlink`) with their invocation audit rows.
- **The audit app** (`app/audit/`) receives `orbit audit` (`list`, `show`, `export`, `stats`, `prune`), `orbit log tail`, and the dashboard's audit listing, summary and denials read models (`GET /api/audit`, `GET /api/audit/summary`, `GET /api/diagnostics/denials`). `orbit audit` commands opt out of command-level invocation audit rows in `app/exec/` (avoiding self-auditing reads of the audit log); `audit prune --confirm` is authorized in `app/exec/admit.rs`, while `audit prune` without `--confirm` is not. `orbit log tail` keeps its `log`/`tail` invocation row.
- **The automation-family apps** (`app/auto_task/`, `app/automation/`, `app/resident/`, `app/experiment/`, `app/evolution/`) each carry `Command::Operation(verb, Input)` for their registry noun: the verb's spec in `orbit-common` stays the contract, and `app/tool_call.rs` holds the `Input` (fields and caller) and the pass through the generic tool entry they share. `orbit auto-task`, `orbit automation`, `orbit resident`, `orbit experiment` and `orbit evolution` in `orbit-cli`, and the tool handlers `adapter/tool_host/{auto_task,automation,resident,experiment,evolution}_tools.rs`, call only their app's `exec`. The read models the dashboard takes are read commands whose outcome is the model: `auto_task::Command::Listing`, `resident::Command::Cards` and `Card`, and `automation::Command::State` and `Evidence`.
- **The agent, attention, codebase, search, notice and crew memory apps** (`app/agent/`, `app/attention/`, `app/codebase/`, `app/search/`, `app/notice/`, `app/crew_memory/`) carry their registry noun's verbs with an `Input`, and pass through the generic tool entry as the automation-family apps do. `orbit agents`, `orbit attention`, `orbit codebase`, `orbit search` and `orbit notice` in `orbit-cli`, and the tool handlers `adapter/tool_host/{agents,codebase,search,notice,memory}_tools.rs`, call only their app's `exec`. Attention has no `attention_tools.rs` handler file: the attention tool dispatches directly through `attention::exec` at `OrbitBuiltinAction::Attention` in `execute_admitted` (`adapter/tool_host/dispatch.rs`). The hand-written commands are typed variants: `agent::Command::Rank`, `RankCalibrate` and `VisionProbe`, `attention::Command::Wait`, and `search::Command::Reindex`. The dashboard's read models are read commands whose outcome is the model: the agent scan, task types, ranking seasons and CI failures, the codebase scans and regression layers, and the notice listing; its notice post and close are `notice::Command::Post` and `Close`, which keep their own notice audit rows.
- **The gc app** (`app/gc/`) receives `orbit gc <class>`, one variant per garbage class, with its invocation row. A reaping `--confirm` is authorized against the class's `gc` command row in the chokepoint, and a report without it is not.
- **The doctor app** (`app/doctor/`) receives `orbit doctor` (the report with its `--fix-*` repairs, `admission-receipts`, `providers` and `fs-access`) with its invocation row, attributed to the admin role and not authorized against a command row. The report, repairs and checks are its flows. Its noun is not registry-derived, so its CLI surface stays hand-written. The checks that read above `orbit-core` (registered hosts, MCP clients, fleet routines, the browser kit, task-store partitions, managed-service environments) reach them through `app::doctor::Host`, which `orbit-cmd` implements and the surface passes in. The dashboard's health probes are `Command::Probe` and `Command::StoreWritable`.
- **The host app** (`app/host/`) receives `orbit host` (`list`, `show`, `add`, `rename`, `offline`, `online`, `remove`), the dashboard's host routes, and, in `app::host::browser_kit`, `orbit config browser-kit` and the kit offer of `orbit workspace init`. Host-registry commands start machine-global without selecting a workspace; they have no `exec` and do not pass the chokepoint. The CLI calls `app::host::run`, while the dashboard calls the same app operations directly. Each surface keeps its own authorization and audit; the dashboard's host edits still authorize the session in `orbit-web`. Reporting dependents remains a machine-global operation: the app reads the global registry, then `app::host::PullDrains` opens each matching registered local replica checkout read-only to list its pending or running pull drains. Those checkouts are inspection inputs, not the command's selected workspace. `app::host::Probe` supplies live remote identity data. `orbit-cmd` implements both ports, including registry-backed checkout selection and read-only opening, and the surface passes them in as `Ports`.
- **The review app** (`app/review/`) receives `orbit application` (`consent`, `rotate`, `revoke`) with their `application` command governance and invocation rows, the dashboard's read-only distributed claim console (`Command::ClaimConsole`), and its governed owner handoff and claim recovery actions (`approve`, `retry`, `revoke`, `recover`) under web task-write provenance with typed console refusals.
- **The routine app** (`app/routine/`) receives `orbit routine enable` and `disable` with their invocation row, authorized against the `routine` command rows in the chokepoint. Its noun is not registry-derived, so its CLI surface stays hand-written. The surface passes the fleet status read in as a closure, because that read lives above `orbit-core`. `Command::Dashboard` carries the dashboard's routine toggle, trigger, breaker and clock writes with the audit row each declares. The app also owns routine discovery, materialization, status and sweep assembly. `app/port.rs` binds the resident-owned status port to the routine app’s selected-checkout adapter.
- **Dashboard writes of these apps.** `auto_task`, `resident`, `review` and `routine` each take `Command::Dashboard(...)`: auto-task create, toggle, mint, schedule and delete; resident create, grant, control and relay; review handoff approve, retry, revoke and claim recover; routine toggle, trigger and clock control. The chokepoint authorizes each against its dashboard governance row for the session the handler names and records the `dashboard.operations` row the handler wrote before, one per command, with the same target and arguments. A denial is recorded and returned as the outcome, so the handler answers it as it did.
- **Still on the runtime.** The dashboard's task projections take the runtime from the `Ws` extractor. The handlers moved into `app/task/` still use the tool host's input and JSON helpers (`adapter/tool_host/input.rs`, `json.rs`).
- **Until the generic tool entry is converted**, a surface's friction command still runs through it: the app passes the command to the tool entry, which checks the fields against the spec, applies tool policy, routes a worker's call to its owner and reaches the tool handler. The handler calls `exec` with a `Core` marked as inside the tool entry, and for that call the chokepoint only runs the flow, because the tool entry authorized and audited it. `app` reaches the tool entry through `contracts::ToolInvocationPort`.

## Dependencies

- Depends on (internal, production): `orbit-automation`, `orbit-clock`, `orbit-common`, `orbit-config`, `orbit-engine`, `orbit-exec`, `orbit-registry`, `orbit-runtime`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-cmd`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-core/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `app`: The apps, one per subject, each with `Command`, `Outcome`, `Error` and one `exec`, and the chokepoint that authorizes and audits every command; imports `application`, `runtime`, `composition` (for `Core` only) and lower workspace crates.
- `adapter`: Protocol adapters translating tool-host, command, and engine-host requests into Core operations; imports `app`, `application`, `runtime`, and lower workspace crates (never `bootstrap` or `composition`).
- `application`: Directional domain operations exposed on `OrbitRuntime` over one workspace and host; imports `runtime` and lower workspace crates (never `adapter`, `bootstrap`, or `composition`).
- `bootstrap`: Workspace initialization, managed asset seeding, and forward-only startup migrations; imports `application`, `runtime`, and lower workspace crates (never `composition`).
- `composition`: Composition root joining resolved configuration, bootstrap, `RuntimeKernel`, and adapters; imports `application`, `bootstrap`, `runtime`, and lower workspace crates.
- `metrics`: Crate-private (`pub(crate)`) reliability and run metrics computed from persisted job-run data; external callers import from `app::run` ([ORB-28962]), and [ORB-28963] moves its internals into `app::run`; imports `application`, `runtime`, `orbit-common`, `orbit-runtime`, `orbit-store`, and `orbit-types`.
- `runtime`: Re-export of `orbit-runtime` and the public `OrbitRuntime` wrapper over `RuntimeKernel`; imports lower workspace crates only (never `application`, `adapter`, `bootstrap`, or `composition`).

## Application module destinations

Where each entry of `crates/orbit-core/src/application/` goes in the [architecture restructure](../design/architecture-restructure/2_design.md) (§3, §5). These are targets: the sections above still describe the current layout. This table is deleted with `application/` (ORB-28450). `scripts/check-architecture-map.py` fails when an entry has no row.

`app::<subject>` is the module `app/<subject>` and `unit::<name>` is the module `unit/<name>`, both under `crates/orbit-core/src/`. `migrate` is the module of that name under the same root. Order: the declared chain `task < run < automation < review < distributed < pull_drain < admission < workspace_auto < backlog_forecast < sweep_filing` is numbered 1 to 10. `above task` means outside the chain and unordered, above `task` as today. `leaf` means the unit imports no `application/` entry. `between task and run` is `crew_pool`: it imports `task` and `run` calls it.

| Entry | Destination | Order |
|---|---|---|
| `admission` | `unit::admission` | 7 |
| `application_authority.rs` | `app::auto_task` | above task |
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

New apps with no `application/` entry: `friction` (built, `app/friction/`), `host` (built, `app/host/`), `update`, `remote_command` (built, `app/remote_command/`, moved out of `app::run` in ORB-28971; [design §3](../design/architecture-restructure/2_design.md#the-remote-command-gets-its-own-app)), and `lease` (built, `app/lease/`, moved out of `app::run` in ORB-28986; [design §3](../design/architecture-restructure/2_design.md#leases-get-their-own-app-and-lock-contention-goes-to-the-task-app)). They have no row, since there is no entry.

A module beside `application/`: `metrics` dissolves into `app::run` (order 2) in two steps, ORB-28962 (built: surface exports on `app::run`, `GET /api/metrics/knowledge` run command, and crate-private `metric` module over `metrics/`) then ORB-28963 (moves the internals into `app::run`) ([design §3](../design/architecture-restructure/2_design.md#metrics-dissolves-into-the-run-app)). It has no row, since it is not an `application/` entry.

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
  - `seeding_policy.rs` → `unit::seeding_policy`: used by `health`, `auto_tasks`, `workspace` and `app::routine`.
  - `statistics.rs` → `unit::statistics`: used by `agents`, `experiment`, `resident` and `backlog_forecast`.
  - `store_opener.rs` → `unit::store_opener`: composition supplies the opener used by both `app::routine` and `workspace`.
  - `tag_instructions.rs` → `unit::tag_instructions`: used by `experiment`, `agent_corpus` and `distributed`.
  - `vision_probe.rs` → `app::agent`: backs `orbit agents vision-probe`.
  - `workflow.rs` → `app::run`: the ship and auto workflow and drain-window rules; `job` is the only app user, and `distributed` and `workspace_auto` are units above `run` that reach it downward.
- Kept as units though no app imports them directly: `pull_drain`, `workspace_auto`, `sweep_filing`, `backlog_forecast` and `landing`. Their callers are units, the engine action dispatch, the runtime or the dashboard. `workspace_auto` stays above `admission` and `pull_drain`, which it calls.
- Kept as an app: `crew_memory`. Its engine adapter and `experiment` callers are internal, but its run-bound crew memory tools are a user-facing surface.
- `metrics` → `app::run`, checked at `84f63223cd`: the only production caller of its reliability reads is `app::run::Command::fleet_reliability`; `orbit-web` names it in four files, one of which calls `aggregate`; its `cost` is an alias for the runtime's module.
- CLI-only command placements decided by callers:
  - `orbit log` (`orbit log tail`) → `app::audit`: reads the host's unified JSONL log feed beside the audit log, opens a runtime, and routes through `app::audit::exec` with its `log`/`tail` invocation row.
  - `orbit clock` (and its `orbit sweep` alias) → `app::routine`: calls `app::routine` for clock status/enable/pause/cadence/repair and `orbit_cmd::registry_routines` for sweep ticks; all subcommands run with `RuntimeNeed::Forbidden` and resolve explicit `--root`/`ORBIT_ROOT` overrides through `orbit-cmd`.
- Existing edge to resolve with the move: `distributed` calls `auto_tasks::job_step` directly, and `auto_tasks` is unordered.
