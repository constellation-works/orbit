---
type: context
summary: Architecture map for orbit-core: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-core]
---

# orbit-core

## What it owns

Composes the runtime. `OrbitRuntime` owns a `RuntimeKernel` and derefs to it. Internal graph `runtime ← application ← adapter`, `composition` joins config/bootstrap/runtime/adapters.

## Layering

- `bootstrap` depends on `application` and never the reverse; application and adapter never name `bootstrap` or `composition`. Runtime names no higher layer.
- Application modules are ordered `contracts < task < job < automation < review < distributed < pull_drain < admission < workspace_auto < backlog_forecast < sweep_filing`. A lower module reaches a higher one only through a port trait the lower module owns and the higher one implements on `OrbitRuntime` (for example `task::TaskNoticePort`, `job::JobExperimentPort`).
- Modules outside that chain (`routines`, `auto_tasks`, `experiment`, `landing`, `codebase`, `resident`, `notice`, `evolution`, `agent_corpus`, `tag_instructions`) call downward freely and are reached from below only through such ports, except the one authorized forecast call in the next bullet.
- `backlog_forecast` has one authorized direct call into `codebase`: `runtime.task_riskiness_model()` in `application/backlog_forecast/assembly.rs`, the codebase risk model whose result answers `forecast_regression_prior`. The guard admits exactly `let risk_model = runtime.task_riskiness_model()?;` in that file. It refuses every other forecast-to-codebase path, grouped import, `super::` chain or inherent call, and every `codebase` path or call back into `backlog_forecast`. Its path checks cover production code, not `tests/`.
- The adapter sits above all of application, which reaches it only through application-owned ports (`contracts::ToolInvocationPort`, `pull_drain::drain::PullTransportPort`).
- `scripts/check-dependency-direction.sh` refuses paths, grouped trees, `super::` chains and calls to higher-module inherent methods that break these rules. Core never depends on `orbit-agent`, `orbit-cmd` or transport crates.

## Main modules and types

`application` (task, job, review, landing, distributed, pull_drain, admission, routines, search, notice), `adapter`, `composition`, `bootstrap`, `runtime` (`OrbitRuntime` in `crates/orbit-core/src/runtime/mod.rs`). See ``crates/orbit-core/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-automation`, `orbit-clock`, `orbit-common`, `orbit-config`, `orbit-engine`, `orbit-exec`, `orbit-registry`, `orbit-runtime`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-cmd`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-core/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `adapter`: Protocol adapters translating tool-host, command, and engine-host requests into Core operations; imports `application`, `runtime`, and lower workspace crates (never `bootstrap` or `composition`).
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

New apps with no module today: `friction`, `host`, `update`. They have no row, since there is no entry.

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
- Existing edge to resolve with the move: `distributed` calls `auto_tasks::job_step` directly, and `auto_tasks` is unordered.
