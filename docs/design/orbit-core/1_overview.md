---
title: Orbit Core — Overview
owner: codex
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Implemented
stage: shipped
next: "—"
feature: orbit-core
doc_role: overview
type: design
summary: Directional runtime, application, adapter, bootstrap, and composition boundaries across orbit-runtime and orbit-core.
tags: [orbit-core, orbit-cmd, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**", "crates/orbit-cmd/**", "crates/orbit-registry/**"]
related_features: [orbit-core]
related_artifacts: [ORB-10016, ORB-10886, ORB-17906, ORB-17924, ORB-17936, ORB-17937, ORB-17938, ORB-17939, ORB-17943, ORB-17972, ORB-22005]
---

# Orbit Core — Overview

Orbit's runtime kernel lives in the internal `orbit-runtime` crate.
`orbit-core` holds the use cases and adapters that drive it, and depends on
`orbit-runtime`. The concerns are directional:

```text
runtime <- application <- adapter
   ^         ^      ^        ^
   |         |  bootstrap    |
   |         |      ^        |
   +------ composition ------+
```

An arrow points at what a layer depends on. Composition loads resolved
`orbit-config`, runs bootstrap, constructs the runtime, and attaches adapters.
Bootstrap depends on application because it seeds through application-owned
definitions, and it opens the runtime too. Neither application nor the adapter names bootstrap or
composition; they reach them through application-owned ports [ORB-22005].
`orbit-runtime` never depends on `orbit-core`,
so kernel code cannot import application or adapter code. Where a kernel
mechanism finishes through a use case, it calls the `ApplicationPort` trait
that composition supplies when it builds the kernel.

`orbit_core::OrbitRuntime` stays the public runtime type. It owns a
`RuntimeKernel` and dereferences to it, so callers reach kernel methods
directly. Application and adapter methods are inherent on the wrapper.
`orbit_core::runtime` re-exports the kernel crate, so `orbit_core::runtime::…`
paths still resolve.

At the crate level, `orbit-core` reaches `orbit-policy` only in tests:
integration fixtures drive `PolicyEngine` directly, while shipped code reaches
policy through `orbit-runtime` (which keeps its own production
`orbit-runtime` → `orbit-policy` edge) and `orbit-tools`.
`scripts/check-dependency-direction.sh` enforces the dev-only edge and rejects
a normal, build, or target-normal `orbit-core` → `orbit-policy` edge.

## 1. Motivation

After [ORB-10016] extracted CLI-facing commands, Core still had a two-way
internal dependency: runtime construction and hosts imported DTOs, defaults,
and business rules from `command`, while command operations reached into
runtime and adapter storage. [ORB-10886] replaced that cycle with explicit
owners inside the crate. [ORB-17906] moved the kernel into `orbit-runtime`,
so an application or adapter edit no longer recompiles it and Cargo, not a
source scan, enforces the runtime direction.

## 2. Core Concepts

- **Runtime kernel** — `orbit-runtime`'s `RuntimeKernel`: stores, engine
  handles, event bus, audit, claims, reservations, process/tool execution
  mechanisms, and construction from an already-resolved config value. The
  actor context, root paths and cost types live there too.
- **Application port** — `orbit_runtime::ApplicationPort`, the operations
  the kernel calls back into (task reads, automation updates, audit records,
  run reconciliation, crew redraws). `orbit-core`'s `composition` module
  implements it on `CoreApplicationPort`, which hands each call the
  `OrbitRuntime` that owns the kernel.
- **Application operations** — use-case DTOs and coordinated task, job,
  workflow, search, and health behavior. Store-coordinated
  lifecycle transitions live here; pure invariants live in `orbit-types`.
- **Adapters** — command audit/dispatch, `orbit.*` tool-host translation, and
  `orbit-engine::RuntimeHost` callback translation. Adapters may use both
  application operations and runtime mechanisms.
- **Bootstrap** — initialization, managed/default assets, policy seeding, and
  forward-only startup migrations [ORB-10012]. It depends on application for
  the seed definitions, and implements `contracts::TaskBundleMaintenancePort`
  so the adapter can publish tasks and count unindexed bundles [ORB-22005].
- **Composition** — root resolution and the sole join of `orbit-config`,
  bootstrap, runtime construction, and adapters [ORB-22005].
- **Runtime roots** — `OrbitRuntimeRoots` carries `global_root`,
  `shared_root`, and `local_root`. On the primary checkout `shared_root` and
  `local_root` are equal; in a linked worktree `shared_root` is the primary
  checkout's `.orbit/` and `local_root` is the worktree's own. An explicit
  `--root` or `ORBIT_ROOT` pins both. A working directory whose own `.orbit/`
  already holds workspace config resolves to that directory, including when
  it sits inside a linked worktree; the linked checkout's own local `.orbit/`
  does not. Bootstrap and workspace `config set` refuse an uninitialized
  `.orbit/` in the working directory when resolution would otherwise adopt a
  different root, and the refusal names `--root` as the way to choose one.
  A submodule is an independent checkout:
  bootstrap stops at its repository boundary and runtime honors its registered
  data directory. Its gitfile does not make it a linked worktree of the
  superproject. When Git is unavailable, linked-worktree discovery requires a
  `worktrees/<name>` gitdir whose `commondir` points to the enclosing `.git`.
- **Registry-aware composition** — opening a selected registered checkout and
  composing routine discovery from the global `config.toml` `[machine]` table
  plus `workspaces.json`
  lives in `orbit-cmd`'s `registry::runtime` and `registry::routines` modules,
  over persistence and validation owned by `orbit-registry`.
- **Consumer-justified re-export** — a root `pub use` kept only because a
  consumer crate (`orbit-cli`, `orbit-web`, `orbit-cmd`) genuinely
  imports it from the root.

## 3. At a Glance

| Concern | File | Task |
|---|---|---|
| Runtime mechanisms | `crates/orbit-runtime/src/` | [ORB-10886], [ORB-17906] |
| Kernel re-export and `OrbitRuntime` wrapper | `crates/orbit-core/src/runtime/` | [ORB-17906] |
| Kernel application port implementation | `crates/orbit-core/src/composition/application_port.rs` | — |
| Application operations and DTOs | `crates/orbit-core/src/application/` | [ORB-10886] |
| Shared application constants and DTOs | `crates/orbit-core/src/application/contracts/` | [ORB-17924] |
| Clock settings and schedule display state below `routines` | `crates/orbit-clock/src/clock_settings.rs` (re-exported from `contracts`), `crates/orbit-core/src/application/contracts/schedule_display.rs` | [ORB-22623], [ORB-22688] |
| Host sweep clock (settings, unit install, pause, cadence, convergence, health), outside Core | `crates/orbit-clock/src/`, re-exported as `routines::clock` | [ORB-22688] |
| Task-to-automation port | `crates/orbit-core/src/application/task/automation_port.rs`, implemented in `automation/task_port.rs` | [ORB-17924], [ORB-17943] |
| Task-owned ports to job, review, distributed, sweep_filing and workspace_auto | `crates/orbit-core/src/application/task/*_port.rs`, implemented in each module's `task_port.rs` | [ORB-17936] |
| Task-owned notice closure on done | `crates/orbit-core/src/application/task/notice_port.rs` (`TaskNoticePort`), implemented in `notice/close.rs` | [ORB-23017] |
| Task-owned checkpoint cost projection | `crates/orbit-core/src/application/task/evolution_port.rs` (`TaskEvolutionPort`), implemented in `evolution/operations.rs` | [ORB-23018] |
| Task-owned commit-pinned readers and executor `host_os` limits | `crates/orbit-core/src/application/task/{base_revision,executor_host_os}.rs`; the host-OS wait is `job/crew_pools/host_os_wait.rs` | [ORB-22289] |
| Backlog forecast, above `workspace_auto` | `crates/orbit-core/src/application/backlog_forecast/`; `task/mod.rs` keeps a two-line re-export shim | [ORB-22287] |
| Job review-admission hook | `crates/orbit-core/src/application/job/review_hook.rs` | [ORB-17924] |
| Job-owned drain and pull-settlement ports | `crates/orbit-core/src/application/job/drain_port.rs`, implemented in `distributed/job_port.rs` and `pull_drain/job_port.rs` | [ORB-17937] |
| Job-owned automation, admission and auto-concurrency ports | `crates/orbit-core/src/application/job/{automation,admission,auto_concurrency}_port.rs`, implemented in `automation/job_port.rs`, `admission/job_port.rs` and `workspace_auto/job_port.rs` | [ORB-17938] |
| Automation-, review- and distributed-owned ports to the modules above them | `crates/orbit-core/src/application/automation/review_port.rs`, `review/distributed_port.rs` and `distributed/admission_port.rs`, implemented in `review/automation_port.rs`, `distributed/review_port.rs` and `admission/distributed_port.rs` | [ORB-17939] |
| Distributed-owned experiment capture port | `crates/orbit-core/src/application/distributed/experiment_port.rs` (`DistributedExperimentPort`), implemented in `experiment/distributed_port.rs` over the capture in `experiment/overlay.rs` | [ORB-22416] |
| Tag-instruction-owned experiment overlay port | `crates/orbit-core/src/application/tag_instructions.rs` (`TagInstructionExperimentPort`), implemented in `experiment/tag_instruction_port.rs` | [ORB-23019] |
| Application-owned ports the adapter implements | `crates/orbit-core/src/application/contracts/tool_invocation.rs` (`ToolInvocationPort`) and `crates/orbit-core/src/application/pull_drain/drain/mod.rs` (`PullTransportPort`), implemented in `adapter/tool_execution.rs` and `adapter/engine_host/actions/pull/adapters.rs` | [ORB-22004] |
| Application-owned port bootstrap implements | `crates/orbit-core/src/application/contracts/task_bundle_maintenance.rs` (`TaskBundleMaintenancePort`), implemented in `bootstrap/task_publication.rs` | [ORB-22005] |
| Product identity marker | `crates/orbit-core/src/application/workspace/product_profile.rs` | [ORB-22005] |
| Tool/engine/command adapters | `crates/orbit-core/src/adapter/` | [ORB-10886] |
| Initialization and managed defaults | `crates/orbit-core/src/bootstrap/` | [ORB-10886] |
| Config loading and runtime assembly | `crates/orbit-core/src/composition/runtime.rs` | [ORB-10886] |
| Context assembly | `crates/orbit-runtime/src/context/state.rs` | — |
| Resolved-config consumption | `crates/orbit-runtime/src/builder.rs` | [ORB-10885], [ORB-10886] |
| Config layering and admission (owning crate); pure TOML path access and the typed dotted-path read are in its private `config_document` leaf below aliases, seeding, operation policy and the key registry; the key registry sits above the domain admission modules (`aliases`, `attention`, `crew`, `operation`, `provider_limit`, `seeding`), which may not name it, and its snapshot projections live beside `ConfigSnapshot`; no-follow file reads and path admission are in its private `config_io` leaf below loading, diagnostics and the mutable `ConfigStore`; the built-in crew registry is in its private `crew_defaults` leaf below snapshot admission, resolved config and seeding, and the registry may not name resolved config (guarded by `scripts/check-dependency-direction.sh`) | `crates/orbit-config/src/` | [ORB-10885] |
| Registered runtime + routine composition | `crates/orbit-cmd/src/registry/runtime/mod.rs`, `crates/orbit-cmd/src/registry/routines.rs` | — |
| Host identity + local workspace catalog | `crates/orbit-registry/src/` | — |
| Tool dispatch and command-audit boundary | `crates/orbit-core/src/adapter/command/dispatch/` | [ORB-10886] |
| Root re-export policy | `crates/orbit-core/src/lib.rs` | [ORB-10016] |
| Extracted command layer | `crates/orbit-cmd/src/` | [ORB-10016] |

## 4. Store Access Boundary

`OrbitStores`, defined in `orbit-runtime`'s `context` module, is the typed holder
for `orbit-store` backends; it is not an external SDK surface. Code inside Core
delegates ordinary persistence to the owning backend traits [ORB-10355], while
outer crates use explicit `OrbitRuntime` methods and application APIs. Runtime exposes mechanisms; coordination across
stores or side effects belongs to the owning application operation.

## 5. Application-Internal Layering

`application/` is one module tree, but its use-case modules depend on each
other in a direction too. Nine of them (`task`, `automation`, `job`, `review`,
`distributed`, `admission`, `workspace_auto`, `pull_drain` and
`sweep_filing`) used to form one strongly connected component. That cycle has
to go before any of them can become its own crate (bearing 5 in
[the decisions](4_decisions.md)). [ORB-17924] cut three edge families,
[ORB-17936], [ORB-17972] and [ORB-17943] cut task's remaining upward edges,
so task now names only `contracts`, [ORB-17937] cut job's edges to
`distributed` and `pull_drain`, [ORB-17938] cut job's remaining upward
edges, so job now names only `task` and `contracts`, [ORB-17939] cut
the back edges among the modules above job, and [ORB-22003] cut the task
and job edges that had regrown past the fixed-name guard, and [ORB-22287]
moved the backlog forecast out of `task` to its own place in the order:

- **Contracts.** `application::contracts` holds the constants and DTOs that
  several of these modules name: the job names `PULL_DRAIN_JOB`,
  `ESCALATION_TRIAGE_JOB` and `FINDING_VERIFICATION_JOB`, `DrainEntryPoint`,
  `PullSettlementEntry` and `PullSettlementOutcome`, the pilot
  `SUPERSEDED_BY_SOURCE` reason, `PilotFingerprints` and the run
  `review_layer` projection; since [ORB-17936] also the crew-draw values
  (`CreationCrewAssignment`, `CrewCandidate`, `InspectionPool`, the
  `PURPOSE_SOURCE_PREFIX` and `PROVIDER_LIMIT_REASSIGNMENT_PREFIX` markers
  and the `auto_task_minted` and `automatic_crew_source` predicates over
  them) and the pipeline submission `RetryKey`; since [ORB-17937] also a
  pull drain's `DrainClaimedLeaf` (with its `ClaimedLeafStage`) and the
  `HostProviderHold` crew draws skip, with the `canonical_provider` name it
  compares; since [ORB-17938] also the `LOCAL_ROUTE_JOB` job name, the
  filing tags job's crew pools route to the sweep purpose
  (`CI_FAILURE_TAG`, `DEPENDABOT_TAG`, `CODE_SCANNING_TAG` and
  `RECONCILIATION_FOLLOW_UP_TAG_PREFIX`) and the `PublishedDelivery` a
  deferred gate grant reports; since [ORB-17939] also the
  `DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED` build gate that `distributed`
  enforces and `review`'s handoff console reports. It imports none of the
  ten modules in the order above it, and every consumer, including `orbit-cli` and `orbit-cmd`,
  imports these items from there. These shapes are still internal to Orbit,
  so they stay in Core rather than `orbit-types`. Their string values and
  serde shapes did not change.
- **Ports.** A lower module that needs behaviour from a higher one owns a
  trait. The higher module implements it on `OrbitRuntime`, so composing the
  runtime binds it, and the lower module never names the higher one.
  `task::TaskAutomationPort` gives task-pilot and
  escalation-triage steps (finding-verification steps too, until [ORB-22844]) the state claim, staleness, fingerprints and live
  holders, and since [ORB-17943] also atomic pilot task reservation and a
  bounded Git reader (`task::SourceGit`, which `automation`'s `Source`
  implements), and `automation` implements it. `job::ReviewAdmissionHook` gives job submission, replay and
  resume the review admission they install and compare, and `review`
  implements it. [ORB-17936] added one task-owned port per remaining upper
  module, each implemented in that module's `task_port.rs`:
  `task::TaskJobPort` (crew draws and the captured crew pools, the PR-route
  forge check, routine run submission, run-owner liveness, the candidate's
  validation gates and unbound owner publications; the pools and the forge
  check are associated types, so task passes them through without naming
  job's types; since [ORB-17972] also the stored run and pipeline-state
  reads, the published-owner and failure-handoff preservation records,
  inspection-capable pools and default crew, crew re-rating, purpose-pool
  candidates and delivery observation, under port names distinct from the
  inherent `job` methods they wrap, with `CrewPoolDraw` in `contracts`;
  since [ORB-22003] also the script-grant decision answer, the decision
  pilot preflight and explicit pilot request, the native publication
  identity desktop review checks, purpose-pool members, the final-recovery
  resume, a claimed leaf's crew candidates, the live pilot run, the exact
  batched run-state read and a row's delivery attention;
  since [ORB-22289] also a task's host-OS wait over the draw's candidates),
  `task::TaskReviewPort` (the review projection, the evidence
  hold resume and the merged-head reconciliation refusal),
  `task::TaskDistributedPort` (the crew a pilot run drew, and since
  [ORB-22003] the completion authority an external merge's handoff
  observation records),
  `task::TaskSweepFilingPort` (CI-sweep admission) and
  `task::TaskWorkspaceAutoPort` (the tasks live `run auto` leaves carry).
  [ORB-22313] added `task::TaskPrecedentDecisionPort` for the precedent
  consumer; [ORB-22808] removed both with decision assistance.
  [ORB-17937] added two job-owned ports in `job/drain_port.rs`:
  `job::JobDrainPort` (the explicit drain entry decision, the Git protection
  preflight, the admission resource throttle, host provider holds, a pull
  drain's claimed leaves and carried admissions, the owner's handoff
  completion authority and commit boundary, and since [ORB-22003] a claim's
  admission receipt and the claimed-leaf stage this host holds for a task),
  which `distributed` implements, and `job::JobPullSettlementPort` (the
  settle-only passes, a claimed leaf's delivery and worker retry, a stopped
  leaf's release, and since [ORB-22003] the orphaned-admission settlement
  after a reconciling run read), which `pull_drain` implements. [ORB-17938] added three more job-owned ports:
  `job::JobAutomationPort` (whether a run's bundle may mix crews, the
  source's repository and the task-pilot routine warning), which
  `automation` implements, `job::JobAdmissionPort` (a resumed published
  delivery's deferred gate reservation and the execution claims holding a
  starved gate's blockers), which `admission` implements, and
  `job::JobAutoConcurrencyPort` (the auto drain's captured concurrency
  mode), which `workspace_auto` implements. [ORB-17939] added three ports
  above job: `automation::AutomationReviewPort` (the before-PR coverage
  exclusions delivery evaluation applies, and the reviewer pool members,
  their `os:` split, candidates and draw for after-landing review tasks),
  which `review` implements, `review::ReviewDistributedPort` (the owner's
  ship contract and machine the before-landing hosts switch checks), which
  `distributed` implements, and `distributed::DistributedAdmissionPort` (the
  executor census a pull's backlog is ordered against), which `admission`
  implements. Where a lower port already wraps the exact read, the cut
  reuses it rather than adding a twin: automation's pilot reservations and
  review's handoff console take the owner's commit boundary, and the console
  the handoff completion authority, through `job::JobDrainPort`, and
  admission's backlog listing takes the tasks live `run auto` leaves carry
  through `task::TaskWorkspaceAutoPort`. `experiment` is outside the order
  and above `distributed`: its readout reads the claim receipt `distributed`
  owns, so admission captures a claimed task's experiments through
  `distributed::DistributedExperimentPort::capture_pull_experiments`, which
  `experiment` implements on `OrbitRuntime`; the receipt read stays a legal
  `experiment -> distributed` edge [ORB-22416]. `experiment` is also above
  `task` and `job`, whose tasks, crew source and claimed leaf it reads back.
  Task show's assignment readout and the provider-limit gate's experiment-pool
  check go through `task::TaskExperimentPort`
  (`task_experiment_assignment_readout`, `task_has_experiment_crew_pool`), and
  job's crew admission, fresh claim routing, claimed-leaf crew installation and
  claimed-leaf capture validation go through `job::JobExperimentPort`
  (`job_experiment_crew_pool`, `job_existing_experiment_capture`,
  `install_job_experiment_capture`, `install_job_claimed_experiment_crew`,
  `validate_job_experiment_capture`). `experiment` implements both on
  `OrbitRuntime` in `experiment/task_port.rs` and `experiment/job_port.rs` and
  keeps enrollment, override resolution, digest validation and every authority
  check; the pool job draws from is the lower-owned
  `contracts::ExperimentCrewPool`. The public `task_experiments` readout and the
  adapter's `experiment::validate_experiment_capture` call are unchanged
  [ORB-23015]. `experiment` also sits above `tag_instructions`: its overlays
  read `LazyRevision`, `TagInstructionFailure` and `read_blob_at`, and call
  `claim_base_revision`. Selection reaches live overlays only through
  `tag_instructions::TagInstructionExperimentPort::tag_instruction_experiment_overlays`,
  which `experiment` implements on `OrbitRuntime` in
  `experiment/tag_instruction_port.rs` by forwarding to
  `experiment_overlays_for_activity`. The `experiment -> tag_instructions`
  helper edge stays legal [ORB-23019]. `landing` sits above
  `distributed`, outside the order: it consumes authorized handoff requests and
  calls down into `task`, `job` and `distributed`. The pure
  handoff-to-claim read `landing_claim` lives in `task/handoff_claim.rs`, so
  task and job no longer resolve claims through landing. Review dispatches
  at the same acceptance and approval points through its own
  `review::ReviewLandingDispatchPort`, implemented in `landing/review_port.rs`;
  failures are still logged and leave the durable request pending [ORB-22708].
- **Selector matching.** `search` sits outside the order, above `task`: it
  reads task candidates and uses the pure matcher owned by
  `task/selector_match.rs`, exported as `task::task_selectors_contain_path`.
  Task listing and pilot no-diff scope checks call the task-owned helper,
  preserving exact and bidirectional containment, normalization, symbol
  selectors and empty-path behavior. `search::task_selectors_contain_path`
  remains an explicit compatibility re-export; retiring it after consumers
  migrate is a follow-up [ORB-23016].
- **Notice closure.** `notice` sits outside the order, above `task`: it reads
  tasks for poster authority and linked-task status. A transition into done
  closes linked notices through `task::TaskNoticePort`
  (`close_linked_notices_for_done_transition`), which `notice` implements on
  `OrbitRuntime` in `notice/close.rs` by delegating to
  `materialize_notice_closures_for_done_task`. The hook keeps that helper's
  unit return and warn-on-failure log. The closure tick, dedupe keys,
  revision checks, owner fencing and closure timing stay in `notice`, and
  `close_notice`, `run_notice_closure_tick` and the other public notice
  APIs are unchanged [ORB-23017].
- **Guard.** `scripts/check-dependency-direction.sh`, run by
  `make ci-guards`, fails when `task` names `automation`, when `task` names
  `job`, `review`, `distributed`, `pull_drain`, `admission`, `sweep_filing`,
  `workspace_auto`, `experiment`, `search`, `notice` or `evolution` by path, grouped tree or `super::` chain,
  when `job` names `distributed`, `pull_drain`, `admission`, `automation`,
  `review`, `sweep_filing`, `workspace_auto` or `experiment` the same ways, and when either calls an
  inherent `OrbitRuntime` method one of those modules defines on any
  receiver, `automation` included [ORB-22035]. Since [ORB-22003] those method
  names are not listed: the guard derives them from the `impl OrbitRuntime`
  blocks under each higher module (methods whose visibility reaches the
  caller), so a new inherent method is covered as soon as it exists, and port
  methods must use distinct names.
  Same-named methods on other types are exact-line allowances (store reads
  through `stores().jobs()` and `stores().tasks()`). `task` has no exempt path.
  It names `backlog_forecast` only in the two exact-line `task/mod.rs`
  re-exports of `BacklogForecastHistory` and `COMPUTE_BUDGET_ENV`, a
  compatibility shim for `lib.rs` and `orbit-web` whose retirement is a
  follow-up [ORB-22287]. The guard also fails when a module above job names a
  module above it in the order below by path, grouped tree or `super::`
  chain, or calls the inherent methods and functions the [ORB-17939] ports
  and reused reads wrap, when `contracts` imports any of the ten modules,
  `routines` or `auto_tasks`, when
  `distributed` names `experiment` or calls its inherent methods [ORB-22416],
  when the single-file `tag_instructions` module names `experiment` or calls
  its inherent methods [ORB-23019],
  when `automation`, `auto_tasks` or `resident` names `routines` or calls its
  inherent methods [ORB-22623, ORB-22956],
  when `task`, `job`, `review` or `distributed` names `landing` or calls its
  inherent methods [ORB-22708],
  or when a shared contract is defined or imported anywhere but
  `application::contracts`. [ORB-22004] The same derivation covers the adapter:
  `application` may not import `crate::adapter`, and no application file may call
  an inherent `OrbitRuntime` method that an `impl OrbitRuntime` block under
  `adapter/` defines with a visibility that reaches it. Application reaches tool
  execution through `contracts::ToolInvocationPort` and the pull transport
  through `pull_drain::drain::PullTransportPort`, both implemented on `OrbitRuntime` by
  the adapter; the adapter's inherent tool methods stay for its own callers. A
  bare `name(` is a free function, not an up-call, so only `receiver.name(`,
  `OrbitRuntime::name(`, `Self::name(` and the qualified-self forms
  `<OrbitRuntime>::name(` and `<crate::OrbitRuntime>::name(` are checked
  [ORB-22159]. Same-named methods on
  other receivers (the skill catalog's `doctor`, the plugin store's
  `list_plugins`) are exact-line allowances. [ORB-22005] It also fails when
  `application` or `adapter` names `crate::bootstrap` or `crate::composition` by
  path, grouped tree or `super::` chain, or calls an inherent `OrbitRuntime`
  method that an `impl OrbitRuntime` block under `composition/` or
  `bootstrap/` defines with a visibility that reaches it. Same-named lines (the
  lexical index store's `reindex_tasks` and the `#[cfg(test)]` fixture's
  `OrbitRuntime::from_roots`) are exact-line allowances. Runtime may not name
  application, adapter, bootstrap or composition, or call inherent methods
  defined by application or adapter; the guard checks those edges too.

The application layering, lowest first, is `contracts < task < job <
automation < review < distributed < pull_drain < admission < workspace_auto <
backlog_forecast < sweep_filing`. `landing` sits above `distributed`, with
review reaching its dispatch through a review-owned
port and task owning their shared handoff-to-claim read. A module map that counts `crate::` paths, grouped trees,
`super::` chains and calls on an `OrbitRuntime` receiver into another
module's inherent methods (a lowercase `module::name(` path is a free
function, not a method call) shows that every direct edge among the ten modules
runs down that order: since [ORB-22003]
no task → job, task → distributed, job → distributed or job → pull_drain
call edge remains (ORB-17972's cut had regrown past its fixed name list),
and since [ORB-17943] no task → automation edge remains, making `task` the
lowest of the ten modules above `contracts`.

`backlog_forecast` sits above `workspace_auto` [ORB-22287] and reads `task`,
`job`, `admission` and `workspace_auto` directly. No module below it may name
it or call its inherent methods, apart from the `task/mod.rs` shim above. Two
edges leave or enter it from outside the order:

- **Forecast → `codebase` (authorized).** `assembly.rs` calls
  `runtime.task_riskiness_model()`, and the returned model answers
  `forecast_regression_prior`. The guard allows exactly that line, and refuses
  any other path or inherent call into `codebase` and any reference from
  `codebase` back to the forecast.
- **`routines` → forecast (downward).** The sweep clock in
  `routines/sweep.rs` calls `record_scheduled_backlog_forecast`. `routines` is
  outside the order, above `automation` and `auto_tasks` (next paragraph).

Review has no edge into `codebase`. Selective-review retirement removed
admission's risk-model freeze and `TaskRiskinessModel::diff_model_identity`;
admission captures the legacy `selective_before_landing` field as `None`.
Codebase retains ownership of the estimator for its remaining consumers,
including the authorized forecast call above. Review owns no risk-model
identity port. The dependency guard rejects production review paths into
codebase, including grouped imports and `super::` chains, and calls to
codebase-owned inherent `OrbitRuntime` methods derived from their defining
impls. The forecast's exact-line allowance does not apply to review.

Reviewer effort selection still needs the candidate's reviewable lines and
changed files. It reads them through review's `ReviewCandidateSizePort`;
`codebase/diff_stats.rs` implements the port on `OrbitRuntime` by delegating to
the existing `candidate_review_size` calculation. This preserves path filters,
counting and unreadable-repository or commit fallbacks while removing the
remaining inherent-method back-edge. The port carries only optional counts,
with no estimator identity or selective-review policy.

`routines` sits above `automation` and `auto_tasks` [ORB-22623]: the sweep
calls automation's evaluation and the auto-task scheduler, and reaches `review`,
`pull_drain` and the forecast. The clock settings `automation` reads for a
condition hold's continuity limit are owned by the `orbit-clock` crate below
Core [ORB-22688] and named through `contracts`, the one contract that reads and
writes a file, under a root its caller names. The
`ScheduleDisplayState` `auto_tasks` projects is in `contracts::schedule_display`.
`routines::clock` and `routines::status` re-export them for outer consumers, a
compatibility shim whose retirement is a follow-up. Resident owns
`resident::ResidentRoutineStatusPort`, whose `disabled_resident_cycle_routine_name`
returns only `Option<String>`; `routines/resident_port.rs` implements it on
`OrbitRuntime` [ORB-22956]. The existing `resident_cycle_routine_disabled`
delegates to it without changing callers. The implementation keeps the selected
checkout status read, silent lookup failures, canonical source-directory and
resident-cycle target matching, and the versioned `!enabled` switch. The guard
refuses resident paths, grouped trees, `super::` chains and calls to routines-owned
inherent methods. Thus automation still reads resident, but resident no longer
names routines, cutting `routines -> automation -> resident -> routines`.

`tag_instructions` sits outside the order but above `task` [ORB-22289]. Task
reads commit-pinned repository files through its own `task::base_revision`
(`run_base_revision`, which reads the recorded worktree base through
`TaskJobPort::stored_run_state`, `workspace_base_revision`, `commit_at`,
`list_blob_paths_at` and `read_blob_at`), and the guard's `task` upper-module
list names `tag_instructions`; its single-file module's inherent names are
derived like a directory module's. `tag_instructions` re-exports `commit_at`
and `read_blob_at` as a compatibility shim for experiment overlays and the
agent corpus; re-pointing them and retiring the shim is a follow-up.
`experiment` sits above this module: its overlays read `LazyRevision`,
`TagInstructionFailure` and `read_blob_at`, and call `claim_base_revision`.
Selection reaches live overlays only through
`TagInstructionExperimentPort::tag_instruction_experiment_overlays`, which
`experiment` implements on `OrbitRuntime` in `experiment/tag_instruction_port.rs`
by forwarding to `experiment_overlays_for_activity`. Ordering, default
suppression, role filtering, capture authority, errors and the combined byte
budget stay here. The guard refuses a path, grouped tree, `super::` chain or
inherent call from the single-file module into `experiment`; the helper reads
stay a legal `experiment -> tag_instructions` edge [ORB-23019]. Executor
`host_os` limits (`task::executor_host_os`) are a task module; the
host-OS wait that resolves the draw's candidates is
`job::crew_pools::host_os_wait`.

`codebase` (the analytics behind `orbit codebase`) sits outside the order but
above `task` [ORB-22625]. Task owns the pure block classification
(`task::block_cause`: `BlockCause`, `Anchor`, `classify_block`, `actor_kind`,
`entry_marks_run`), which its blocked-recovery readout calls, and records a
done landing's diff stats through `task::TaskCodebasePort`, which `codebase`
implements on `OrbitRuntime` in `codebase/diff_stats.rs`. `codebase` keeps the
failed-claim settlement read that needs `distributed`, so `task -> codebase ->
distributed` no longer climbs; it also keeps the persisted representation of
`BlockCause`, and re-exports the moved names as a compatibility shim for
`agents` and its own modules, whose retirement is a follow-up. The guard's
`task` upper-module list names `codebase`.

`resident` (resident orchestrator mandates) sits outside the order but above
`task` [ORB-22955]: it drives task writes and reads task history, so task
reaches its policy only through `task::TaskResidentPort`, which `resident`
implements on `OrbitRuntime` in `resident/task_port.rs`. A
`resident_authority` decision answer applies the human's grant through it,
and a task write admits a resident comment under the task lock and refuses a
resident record whose verbatim text needs redaction through it, by delegating
to the unchanged resident helpers. The guard's `task` upper-module list names
`resident`, so a path, grouped tree, `super::` chain or call to a
resident-defined inherent method from `task` is refused.

`notice` (fleet notices) sits outside the order but above `task` [ORB-23017].
It reads tasks for poster authority and linked-task status, so a transition
into done closes linked notices only through `task::TaskNoticePort`, which
`notice` implements on `OrbitRuntime` in `notice/close.rs`. The hook delegates
to `materialize_notice_closures_for_done_task`, keeping the unit return and
the best-effort warn-on-failure log. The closure tick, dedupe keys, revision
checks, owner fencing and closure timing stay in notice. The guard's `task`
upper-module list names `notice`, so a path, grouped tree, `super::` chain
or call to a notice-defined inherent method from `task` is refused.

`evolution` (the capability-gap ledger) sits outside the order but above
`task` [ORB-23018]. It reads tasks for cost validation and proposal state and
replays task comments for recovery. Task writes project checkpoint costs only
through `task::TaskEvolutionPort::project_task_checkpoint_costs`, implemented
on `OrbitRuntime` in `evolution/operations.rs` by forwarding to the existing
`ingest_checkpoint_costs`. The call stays immediately after the durable task
mutation under the task lock: ingestion errors still propagate after the
comment commits. Proposal refresh, coordination authority checks, source-key
deduplication and persisted occurrences stay unchanged, as does the public
`reconcile_evolution_checkpoint` replay path for interrupted ingestion. The
guard's `task` upper-module list names `evolution`, refusing task paths,
grouped trees, `super::` chains and evolution-owned inherent calls. All public
evolution APIs remain available.

The remaining back-edges among the unordered modules (`resident`,
`experiment`, `codebase`) and the rest of the application still keep the
core strongly connected component and await a later audit. [ORB-22708] cut
the lower modules’ direct edges into `landing`.

## Task References

- [ORB-10012] — introduced the versioned workspace-layout migration pre-flight the runtime open runs.
- [ORB-10016] — extracted `orbit-cmd` from orbit-core and trimmed the root re-export surface.
- [ORB-10355] — removed the hand-maintained store delegation layer in favor of direct typed backend access.
- [ORB-10885] — extracted `orbit-config` from orbit-core and moved config layering into the owning crate.
- [ORB-10886] — removed the internal command/runtime cycle and established the directional module graph.
- [ORB-17906] — extracted the runtime kernel into the `orbit-runtime` crate behind the `OrbitRuntime` wrapper.
- [ORB-17924] — added `application::contracts`, the task-to-automation port and the job review-admission hook, and guarded those cuts.
- [ORB-17936] — cut task's edges to job, review, distributed, sweep_filing and workspace_auto through task-owned ports and crew-draw contracts, and guarded them.
- [ORB-17937] — cut job's edges to distributed and pull_drain through job-owned drain and pull-settlement ports and the claimed-leaf and host-hold contracts, and guarded them.
- [ORB-17938] — cut job's edges to admission, automation, review, sweep_filing and workspace_auto through job-owned automation, admission and auto-concurrency ports and the local-route, filing-tag and published-delivery contracts, and guarded them.
- [ORB-17939] — cut the back edges among automation, review, distributed, admission and workspace_auto through ports the lower modules own and the distributed mutation gate contract, and guarded the order above job.
- [ORB-17972] — routed task's remaining inherent calls to `job` through `task::TaskJobPort` and guarded them; no task → job edge remains.
- [ORB-22003] — routed the regrown task → job, task → distributed, job → distributed and job → pull_drain calls through the existing lower-owned ports and made the guard derive forbidden inherent names from each higher module's `impl OrbitRuntime` blocks; the forecast stayed exempt until its move ([ORB-22287]).
- [ORB-22004] — moved the application's tool-invocation and pull-transport calls behind `contracts::ToolInvocationPort` and `pull_drain::drain::PullTransportPort`, which the adapter implements on `OrbitRuntime`, and guarded the application → adapter edge against inherent `OrbitRuntime` calls.
- [ORB-22005] — removed the application → composition, application → bootstrap and adapter → bootstrap edges through `AssistanceServicePort`, `TaskBundleMaintenancePort` and moving `skill_link_roots` and `ProductProfile` into application, settled the direction as `bootstrap → application`, and guarded it.
- [ORB-17943] — routed task-pilot's bounded `Source` reader and atomic pilot reservation through `task::TaskAutomationPort` and `task::SourceGit`, dropping the guard allowlist; no task → automation edge remains.
- [ORB-22035] — made the guard refuse `task` calls into `automation`'s inherent `OrbitRuntime` methods, which the derived-name check skipped because `automation` was not in its upper list; the forecast's exemption of that time ([ORB-22287] removed it) did not cover them.
- [ORB-22159] — closed the derived-name guard's UFCS gap so `<OrbitRuntime>::name` and the qualified-self forms are checked as up-calls.
- [ORB-22289] — moved executor `host_os` limits, mock admission and the commit-pinned readers into `task`, put the host-OS wait in `job::crew_pools` and the wait and live-delivery-run reads behind `TaskJobPort`, and added `tag_instructions` to the guarded `task` upper modules.
- [ORB-22287] — moved the backlog forecast from `task/backlog_forecast/` to `backlog_forecast/` above `workspace_auto`, removed its guard exemption, and guarded the forecast → `codebase` edge to the one authorized call.
- [ORB-22313] — cut the task → precedent_decision edge through `task::TaskPrecedentDecisionPort`, moved the pure enablement records to `contracts`, and guarded the direction.
- [ORB-22416] — cut the distributed → experiment capture edge through `distributed::DistributedExperimentPort`, implemented by `experiment`, and guarded the direction.
- [ORB-22624] — placed `visual_delivery` above `distributed`, moved its two pure reads into `task`, cut review's and distributed's calls through `review::ReviewVisualPort` and `distributed::DistributedVisualPort`, and guarded the direction.
- [ORB-22708] — placed `landing` above `distributed` beside `visual_delivery`, moved its pure handoff claim resolution into `task`, routed review dispatch through `ReviewLandingDispatchPort`, and guarded the direction.
- [ORB-22625] — moved the block classification into `task`, routed the done-landing diff stats through `task::TaskCodebasePort`, and added `codebase` to the guarded `task` upper modules.
- [ORB-22956] — cut resident's cycle status back-edge through `resident::ResidentRoutineStatusPort`, implemented by `routines`, and guarded resident references and inherent calls into routines.
- [ORB-22955] — cut task's grant-answer and resident-comment calls into `resident` through `task::TaskResidentPort`, implemented by `resident`, and added `resident` to the guarded `task` upper modules.
- [ORB-22688] — moved the host sweep clock (`routines/clock`, its settings and unit templates) into the `orbit-clock` crate, which names only `orbit-common` and so compiles outside the serial Core library; Core re-exports it at the old paths.
- [ORB-22623] — cut automation's and auto_tasks' back-edges into `routines` by moving the clock settings and `ScheduleDisplayState` to `contracts`, and guarded the direction.
- [ORB-22807] — removed visual review: `visual_delivery`, `review::ReviewVisualPort`, `distributed::DistributedVisualPort`, `task::mock_admission` and `task::visual_review_json` are gone, `landing` calls down into `task`, `job` and `distributed` only, and the guard's `visual_delivery` rule is dropped.
- [ORB-23017] — cut task's done-transition notice closure through `task::TaskNoticePort`, implemented by `notice`, and added `notice` to the guarded `task` upper modules.
- [ORB-23018] — cut task's post-commit checkpoint cost projection through `task::TaskEvolutionPort`, implemented by `evolution`, and added `evolution` to the guarded `task` upper modules without changing ingestion or replay.
- [ORB-23019] — cut tag instructions' overlay query through `tag_instructions::TagInstructionExperimentPort`, implemented by `experiment`, and guarded the single-file module's paths and inherent calls into `experiment` while leaving the helper reads legal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
