---
title: Orbit Core — Overview
owner: codex
last_updated: 2026-10-24
last_validated: 2026-10-24
status: Accepted
feature: orbit-core
doc_role: overview
type: design
summary: Directional runtime, application, adapter, bootstrap, and composition boundaries across orbit-runtime and orbit-core.
tags: [orbit-core, orbit-cmd, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**", "crates/orbit-cmd/**", "crates/orbit-registry/**"]
related_features: [orbit-core]
related_artifacts: [ORB-10016, ORB-10886, ORB-17906, ORB-17924, ORB-17936, ORB-17937, ORB-17938, ORB-17939]
---

# Orbit Core — Overview

Orbit's runtime kernel lives in the internal `orbit-runtime` crate.
`orbit-core` holds the use cases and adapters that drive it, and depends on
`orbit-runtime`. The concerns are directional:

```text
runtime <- application <- adapter
   ^             ^           ^
   +------ composition -------+
              ^
           bootstrap
```

Composition loads resolved `orbit-config`, runs bootstrap, constructs the
runtime, and attaches adapters. `orbit-runtime` never depends on `orbit-core`,
so kernel code cannot import application or adapter code. Where a kernel
mechanism finishes through a use case, it calls the `ApplicationPort` trait
that composition supplies when it builds the kernel.

`orbit_core::OrbitRuntime` stays the public runtime type. It owns a
`RuntimeKernel` and dereferences to it, so callers reach kernel methods
directly. Application and adapter methods are inherent on the wrapper.
`orbit_core::runtime` re-exports the kernel crate, so `orbit_core::runtime::…`
paths still resolve.

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
  run reconciliation, crew redraws). `orbit-core`'s `runtime` module
  implements it on the `OrbitRuntime` that owns the kernel.
- **Application operations** — use-case DTOs and coordinated task, job,
  workflow, search, and health behavior. Store-coordinated
  lifecycle transitions live here; pure invariants live in `orbit-types`.
- **Adapters** — command audit/dispatch, `orbit.*` tool-host translation, and
  `orbit-engine::RuntimeHost` callback translation. Adapters may use both
  application operations and runtime mechanisms.
- **Bootstrap** — initialization, managed/default assets, policy seeding, and
  forward-only startup migrations.
- **Composition** — root resolution and the sole join of `orbit-config`,
  bootstrap, runtime construction, and adapters.
- **Runtime roots** — `OrbitRuntimeRoots` carries `global_root`,
  `shared_root`, and `local_root`. On the primary checkout `shared_root` and
  `local_root` are equal; in a linked worktree `shared_root` is the primary
  checkout's `.orbit/` and `local_root` is the worktree's own. An explicit
  `--root` or `ORBIT_ROOT` pins both. A submodule is an independent checkout:
  bootstrap stops at its repository boundary and runtime honors its registered
  data directory. Its gitfile does not make it a linked worktree of the
  superproject. When Git is unavailable, linked-worktree discovery requires a
  `worktrees/<name>` gitdir whose `commondir` points to the enclosing `.git`.
- **Registry-aware composition** — opening a selected registered checkout and
  composing routine discovery from the global `config.toml` `[machine]` table
  plus `workspaces.json`
  lives in `orbit-cmd::registry_runtime` and `orbit-cmd::registry_routines`,
  over persistence and validation owned by `orbit-registry`.
- **Consumer-justified re-export** — a root `pub use` kept only because a
  consumer crate (`orbit-cli`, `orbit-web`, `orbit-cmd`) genuinely
  imports it from the root.

## 3. At a Glance

| Concern | File | Task |
|---|---|---|
| Runtime mechanisms | `crates/orbit-runtime/src/` | [ORB-10886], [ORB-17906] |
| Kernel re-export, `OrbitRuntime` wrapper and application port | `crates/orbit-core/src/runtime.rs` | [ORB-17906] |
| Application operations and DTOs | `crates/orbit-core/src/application/` | [ORB-10886] |
| Shared application constants and DTOs | `crates/orbit-core/src/application/contracts/` | [ORB-17924] |
| Task-to-automation port | `crates/orbit-core/src/application/task/automation_port.rs` | [ORB-17924] |
| Task-owned ports to job, review, distributed, sweep_filing and workspace_auto | `crates/orbit-core/src/application/task/*_port.rs`, implemented in each module's `task_port.rs` | [ORB-17936] |
| Job review-admission hook | `crates/orbit-core/src/application/job/review_hook.rs` | [ORB-17924] |
| Job-owned drain and pull-settlement ports | `crates/orbit-core/src/application/job/drain_port.rs`, implemented in `distributed/job_port.rs` and `pull_drain/job_port.rs` | [ORB-17937] |
| Job-owned automation, admission and auto-concurrency ports | `crates/orbit-core/src/application/job/{automation,admission,auto_concurrency}_port.rs`, implemented in `automation/job_port.rs`, `admission/job_port.rs` and `workspace_auto/job_port.rs` | [ORB-17938] |
| Automation-, review- and distributed-owned ports to the modules above them | `crates/orbit-core/src/application/automation/review_port.rs`, `review/distributed_port.rs` and `distributed/admission_port.rs`, implemented in `review/automation_port.rs`, `distributed/review_port.rs` and `admission/distributed_port.rs` | [ORB-17939] |
| Tool/engine/command adapters | `crates/orbit-core/src/adapter/` | [ORB-10886] |
| Initialization and managed defaults | `crates/orbit-core/src/bootstrap/` | [ORB-10886] |
| Config loading and runtime assembly | `crates/orbit-core/src/composition.rs` | [ORB-10886] |
| Context assembly | `crates/orbit-runtime/src/context.rs` | — |
| Resolved-config consumption | `crates/orbit-runtime/src/builder.rs` | [ORB-10885], [ORB-10886] |
| Config layering (owning crate) | `crates/orbit-config/src/` | [ORB-10885] |
| Registered runtime + routine composition | `crates/orbit-cmd/src/registry/runtime/mod.rs`, `crates/orbit-cmd/src/registry/routines.rs` | — |
| Host identity + local workspace catalog | `crates/orbit-registry/src/` | — |
| Tool dispatch and command-audit boundary | `crates/orbit-core/src/adapter/command/dispatch/` | [ORB-10886] |
| Root re-export policy | `crates/orbit-core/src/lib.rs` | [ORB-10016] |
| Extracted command layer | `crates/orbit-cmd/src/` | [ORB-10016] |

## 4. Store Access Boundary

`OrbitStores` is Core's internal holder for typed `orbit-store` backends; it is
not an external SDK surface. Code inside Core delegates ordinary persistence
to the owning backend traits, while outer crates use explicit `OrbitRuntime`
methods and application APIs. Runtime exposes mechanisms; coordination across
stores or side effects belongs to the owning application operation.

## 5. Application-Internal Layering

`application/` is one module tree, but its use-case modules depend on each
other in a direction too. Nine of them (`task`, `automation`, `job`, `review`,
`distributed`, `admission`, `workspace_auto`, `pull_drain` and
`sweep_filing`) used to form one strongly connected component. That cycle has
to go before any of them can become its own crate (bearing 5 in
[the decisions](4_decisions.md)). [ORB-17924] cut three edge families, and
[ORB-17936] cut task's remaining upward edges except the two automation
callers below, [ORB-17937] cut job's edges to `distributed` and
`pull_drain`, [ORB-17938] cut job's remaining upward edges, so job now
names only `task` and `contracts`, and [ORB-17939] cut the back edges among
the modules above job:

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
  nine modules, and every consumer, including `orbit-cli` and `orbit-cmd`,
  imports these items from there. These shapes are still internal to Orbit,
  so they stay in Core rather than `orbit-types`. Their string values and
  serde shapes did not change.
- **Ports.** A lower module that needs behaviour from a higher one owns a
  trait. The higher module implements it on `OrbitRuntime`, so composing the
  runtime binds it, and the lower module never names the higher one.
  `task::TaskAutomationPort` gives task-pilot, finding-verification and
  escalation-triage steps the state claim, staleness, fingerprints and live
  holders, and `automation` implements it. Two task-pilot callers that
  [ORB-17924] did not list still call `automation` directly, by the owner's
  stop-and-report ruling: the bounded `Source` Git reader in
  `task/pilot/no_diff.rs` and `pilot_reservation::reserve` in
  `task/pilot/prepare.rs`. `job::ReviewAdmissionHook` gives job submission, replay and
  resume the review admission they install and compare, and `review`
  implements it. [ORB-17936] added one task-owned port per remaining upper
  module, each implemented in that module's `task_port.rs`:
  `task::TaskJobPort` (crew draws and the captured crew pools, the PR-route
  forge check, routine run submission, run-owner liveness, the candidate's
  validation gates and unbound owner publications; the pools and the forge
  check are associated types, so task passes them through without naming
  job's types), `task::TaskReviewPort` (the review projection, the evidence
  hold resume and the merged-head reconciliation refusal),
  `task::TaskDistributedPort` (the crew a pilot run drew),
  `task::TaskSweepFilingPort` (CI-sweep admission) and
  `task::TaskWorkspaceAutoPort` (the tasks live `run auto` leaves carry).
  [ORB-17937] added two job-owned ports in `job/drain_port.rs`:
  `job::JobDrainPort` (the explicit drain entry decision, the Git protection
  preflight, the admission resource throttle, host provider holds, a pull
  drain's claimed leaves and carried admissions, the owner's handoff
  completion authority and commit boundary), which `distributed` implements,
  and `job::JobPullSettlementPort` (the settle-only passes, a claimed leaf's
  delivery and worker retry, and a stopped leaf's release), which
  `pull_drain` implements. [ORB-17938] added three more job-owned ports:
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
  through `task::TaskWorkspaceAutoPort`.
- **Guard.** `scripts/check-dependency-direction.sh`, run by
  `make ci-guards`, fails when `task` names `automation` (outside those two
  exact lines), when `task` names `job`, `review`, `distributed`,
  `sweep_filing` or `workspace_auto` by path, grouped tree or `super::` chain
  or calls the inherent methods and functions its ports wrap, when `job`
  names `distributed`, `pull_drain`, `admission`, `automation`, `review`,
  `sweep_filing` or `workspace_auto` the same ways or calls the inherent
  methods and functions its ports wrap (except the review hook's, whose
  names match the methods it wraps), when a module above job names a module
  above it in the order below by path, grouped tree or `super::` chain, or
  calls the inherent methods and functions the [ORB-17939] ports and reused
  reads wrap, when `contracts` imports any of the nine modules, or when a
  shared contract is defined or imported anywhere but
  `application::contracts`.

The application layering, lowest first, is `contracts < task < job <
automation < review < distributed < pull_drain < admission < workspace_auto <
sweep_filing`. A module map that counts `crate::` paths, grouped trees,
`super::` chains and calls on an `OrbitRuntime` receiver into another
module's inherent methods (a lowercase `module::name(` path is a free
function, not a method call) shows that every edge out of `job` and the
seven modules above it runs down that order. Two back edges, both out of
`task`, still hold `task`, `job` and `automation` in one strongly connected
component:

| Cluster | Remaining back edges | Task |
|---|---|---|
| Task's unlisted pilot callers | task → automation (`Source`, `pilot_reservation::reserve`) | [ORB-17943] |
| Task's inherent calls into job | task → job (run reads, crew pools and draws, crew re-rating, published-owner requeue records) | [ORB-17972] |

## Task References

- [ORB-10012] — introduced the versioned workspace-layout migration pre-flight the runtime open runs.
- [ORB-10016] — extracted `orbit-cmd` from orbit-core and trimmed the root re-export surface.
- [ORB-10355] — removed the hand-maintained store delegation layer in favor of direct typed backend access.
- [ORB-10886] — removed the internal command/runtime cycle and established the directional module graph.
- [ORB-17906] — extracted the runtime kernel into the `orbit-runtime` crate behind the `OrbitRuntime` wrapper.
- [ORB-17924] — added `application::contracts`, the task-to-automation port and the job review-admission hook, and guarded those cuts.
- [ORB-17936] — cut task's edges to job, review, distributed, sweep_filing and workspace_auto through task-owned ports and crew-draw contracts, and guarded them.
- [ORB-17937] — cut job's edges to distributed and pull_drain through job-owned drain and pull-settlement ports and the claimed-leaf and host-hold contracts, and guarded them.
- [ORB-17938] — cut job's edges to admission, automation, review, sweep_filing and workspace_auto through job-owned automation, admission and auto-concurrency ports and the local-route, filing-tag and published-delivery contracts, and guarded them.
- [ORB-17939] — cut the back edges among automation, review, distributed, admission and workspace_auto through ports the lower modules own and the distributed mutation gate contract, and guarded the order above job.
- [ORB-17943], [ORB-17972] — follow-ups for task's back edges to automation and job.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
