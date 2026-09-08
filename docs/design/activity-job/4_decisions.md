---
summary: "Activity / Job — Decisions"
type: design
title: "Activity / Job — Decisions"
owner: codex
last_updated: 2026-09-08
last_validated: 2026-09-08
status: Draft
feature: activity-job
doc_role: decisions
tags: ["activity-job"]
---

# Activity / Job — Decisions

This Decision log records the decisions that define the current Activity / Job substrate. Entries are append-only and stay in place when later decisions supersede or fold them. See [1_overview.md](./1_overview.md) for the feature summary, [2_design.md](./2_design.md) for the current implementation, and [3_vision.md](./3_vision.md) for the questions that may force more decisions.

The log keeps four load-bearing rollup decision bodies. Folded entries remain in place with a title-based `Superseded by` link and a one-line pointer, per [CONVENTIONS §4e](../CONVENTIONS.md#4e-rollup-decisions).

[Remove the planning duel and retain compatibility-only residue](#remove-the-planning-duel-and-retain-compatibility-only-residue) proposes retiring the planning competition and superseding the two
decisions dedicated to its plan-selection and dispatch-override mechanisms.
[ORB-10627] removes their implementation while the proposal remains recorded as
historical reasoning.

---

## Canonical v2 assets normalize into one execution contract

**Recorded:** 2026-05-11 02:06:39.279893Z · [T20260419-2156], [T20260418-2143], [T20260419-0104], [T20260418-2019], [T20260423-0445], [T20260425-0204], [T20260419-2347], [T20260426-0047], [T20260428-8], [T20260506-18]

### Context
Activity/job correctness depends on making authoring conveniences disappear before execution. The old log carried separate ADRs for schema retirement, backend resolution, target refs, defaults, catalog precedence, seeded assets, and workflow admission, but all enforce the same boundary: YAML is human-authored input, while execution sees normalized, validated runtime state.

### Decision
Treat `schemaVersion: 2` as the only activity/job asset family, load seeded and workspace catalogs with explicit layer precedence, resolve authoring sugar (`target: activity:<name>`, object-valued defaults, and workflow admission) before dispatch, and keep seeded activities/jobs as executable reference contracts for that normalized surface.

**Amended by [ORB-10801].** `backend: auto` is no longer resolved — agent backend selection was retired. The load-time normalization pass now *rejects* the retired declarations (`backend: http | auto`, any `session:` binding) instead of concretizing them, so nothing is silently reinterpreted. See [specs/backend-resolution.md](./specs/backend-resolution.md). Direct task-workflow admission remains a workflow-specific normalization path rather than a generic task-update rule.

Folded instances:

| ADR | Instance folded into this rollup |
|-----|----------------------------------|
| [Resolve `backend: auto` once, before dispatch](#resolve-backend-auto-once-before-dispatch) | Retired by [ORB-10801]: agent backend selection no longer exists. |
| [`target: activity:<name>` is authoring sugar, not an execution primitive](#target-activityname-is-authoring-sugar-not-an-execution-primitive) | `target: activity:<name>` is authoring sugar resolved before execution. |
| [Seed reference activities and jobs as load-bearing runtime contracts](#seed-reference-activities-and-jobs-as-load-bearing-runtime-contracts) | Seeded activities and jobs are load-bearing runtime contracts. |
| [Merge object-valued job defaults with caller input, and surface early pipeline failures as synthetic job steps](#merge-object-valued-job-defaults-with-caller-input-and-surface-early-pipeline-failures-as-synthetic-job-steps) | Object-valued job defaults shallow-merge with caller input, and early failures get synthetic job steps. |
| [Job catalog discovery honors layer precedence](#job-catalog-discovery-honors-layer-precedence) | Job catalog discovery honors layer precedence. |
| [Activity catalogs honor layer precedence and activity execution stays job-owned](#activity-catalogs-honor-layer-precedence-and-activity-execution-stays-job-owned) | Activity catalog discovery honors layer precedence, and activity execution stays job-owned. |
| [Workflow admission is distinct from generic task updates](#workflow-admission-is-distinct-from-generic-task-updates) | Workflow admission is distinct from generic task updates. |

### Consequences
- The runtime now documents and validates one typed activity/job surface.
- Human-authored YAML stays readable while executors consume concrete steps, concrete backends, merged inputs, and first-wins catalog entries.
- New workspaces start with real executable reference assets rather than empty examples.
- Costs retained from folded entries:
- Cost: old assets stop limping along; migration work becomes mandatory instead of gradual.
- Cost: callers must remember to run the normalization pass before dispatch, and any missed call site fails as a structural bug.
- Cost: the load path owns more normalization logic, and stale refs fail before dispatch instead of being lazily recoverable.
- Cost: seeded assets become part of the public maintenance burden and can drift if docs/tests stop exercising them.
- Cost: the job-level input contract is now a shallow merge rule that docs and tests must preserve, and run history can include synthetic job-level failure steps that were not literal authored YAML steps.
- Cost: lower-precedence job assets can be shadowed silently, so debugging an unexpected workflow now requires checking catalog source paths.
- Cost: lower-precedence activity assets can be shadowed silently, and direct ad hoc activity execution is no longer a documented CLI workflow.
- Cost: task lifecycle semantics are no longer uniform across all status mutation surfaces; reviewers must distinguish workflow admission from ordinary task updates.

## Host boundaries and agent dispatch stay explicit

**Recorded:** 2026-05-11 02:06:39.281318Z · [T20260419-2014], [T20260418-2210], [T20260419-0104], [T20260423-0114], [T20260427-48], [T20260430-15], [T20260418-2018], [T20260419-0623-2], [T20260420-0510-2], [T20260428-9], [T20260428-12], [T20260506-16], [T20260506-17], [T20260505-22], [T20260506-18]

### Context
The agent-loop path is where activity/job can most easily leak provider implementation details, mutable sessions, or role configuration across crate boundaries. The split ADRs all defended the same shape: shared types live low, orbit-core hosts primitive services, the engine dispatches concrete activity specs, and provider/backends remain explicit choices.

### Decision
Keep activity/job types in `orbit-types`, keep orbit-core free of `orbit-agent` transport types, and route agent dispatch through retained provider CLI runtimes behind a host-resolved executor contract. Activity routing is crew-based; the retired Groundhog, HTTP, and role-based surfaces remain historical. Task-aware CLI envelopes carry durable run context, and provider static-arg fixups run before sandbox dispatch.

Folded instances:

| ADR | Instance folded into this rollup |
|-----|----------------------------------|
| [Cross-iteration `session:` binding is a loop-scoped HTTP-only feature](#cross-iteration-session-binding-is-a-loop-scoped-http-only-feature) | Retired by [ORB-10801]: the HTTP agent loop is gone, so any `session:` binding is refused at load. |
| [Keep the retained CLI runtimes as the implementation of `backend: cli`](#keep-the-retained-cli-runtimes-as-the-implementation-of-backend-cli) | Retained CLI runtimes are the agent implementation. |
| [Groundhog is a sibling activity kind, not an `agent_loop` mode bit](#groundhog-is-a-sibling-activity-kind-not-an-agentloop-mode-bit) | Groundhog is a sibling activity kind, not an `agent_loop` mode bit. |
| [CLI backend resolves executor args, not just provider commands](#cli-backend-resolves-executor-args-not-just-provider-commands) | CLI backend resolves executor args, not just provider commands. |
| [Codex CLI dynamic flags stay in provider runtime config](#codex-cli-dynamic-flags-stay-in-provider-runtime-config) | Codex CLI dynamic flags stay in provider runtime config. |
| [`orbit init` is the writer for per-role agent settings](#orbit-init-is-the-writer-for-per-role-agent-settings) | `orbit init` writes per-role agent settings. |
| [`[agent.<role>]` config overrides inline `agent_loop` settings at dispatch](#agentrole-config-overrides-inline-agentloop-settings-at-dispatch) | `[agent.<role>]` config overrides inline `agent_loop` settings at dispatch. |
| [CLI agent envelopes carry durable task and run context](#cli-agent-envelopes-carry-durable-task-and-run-context) | CLI agent envelopes carry durable task and run context. |
| [Provider static-arg fixups apply before sandbox dispatch](#provider-static-arg-fixups-apply-before-sandbox-dispatch) | Provider static-arg fixups apply before sandbox dispatch. |
| [`orbit init` uses a recommendation-first setup wizard](#orbit-init-uses-a-recommendation-first-setup-wizard) | `orbit init` uses a recommendation-first setup wizard. |

### Consequences
- Parsing, validation, dispatch, and CLI display share one Rust type family without making orbit-core depend on provider transport objects.
- Agent dispatch has one path after [ORB-10801]; tool enforcement stays delegated to the provider harness.
- First-run and named-crew choices live in user config while YAML stays reusable across workspaces.
- Costs retained from folded entries:
- Cost: `orbit-types` owns the shared runtime vocabulary and has to stay disciplined about not accreting behavior.
- Cost: session reuse becomes a narrowly scoped feature instead of a general-purpose memory layer.
- Cost: the feature now has materially different semantics between HTTP and CLI, especially around tool enforcement.
- Cost: ActivityV2 gains another sibling variant and the feature family becomes slightly broader.
- Cost: the engine/core boundary is slightly wider than a single string and every smoke host implementing `V2RuntimeHost` must model executor args explicitly.
- Cost: the v2 host boundary exposes a provider-config map, so backend CLI dispatch remains aware of provider-specific runtime settings.
- Cost: until [T20260428-12] landed, the values written to `config.toml` were inert — they round-tripped but did not influence dispatch, so reviewers had to treat the behavior as half-shipped during that window.
- Cost: dispatch now has one more clone-and-mutate path per role-tagged step. The same role might get queried multiple times within one job run; if that ever shows up in profiles, memoize at the executor level rather than in the host trait.
- Cost: the `V2RuntimeHost` seam now has a method that is purely a config-config concern. Tests that build their own mock host get a free `None` default, but a host that wants to exercise the override path has to opt in explicitly.
- Cost: CLI stdin blobs now contain more task prose, so audit blob readers should continue treating those blobs as diagnostic artifacts rather than small control messages.
- Cost: provider static-arg fixups mean executor YAML values such as Claude's `--debug-file` path are no longer honored verbatim; maintainers must read dispatcher behavior alongside assets.
- Cost: prompt collection now owns display formatting and a small choice loop, so tests must cover interaction flow in addition to config values.

## Resolve `backend: auto` once, before dispatch

**Superseded by:** [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries) (folded)
**Recorded:** 2026-04 · [T20260418-2143], [T20260419-0104]

Folded into [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries)'s rollup for canonical v2 asset normalization.

## `target: activity:<name>` is authoring sugar, not an execution primitive

**Superseded by:** [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries) (folded)
**Recorded:** 2026-04 · [T20260418-2019]

Folded into [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries)'s rollup for canonical v2 asset normalization.

## Cross-iteration `session:` binding is a loop-scoped HTTP-only feature

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-04 · [T20260418-2018], [T20260419-0104], [T20260419-0623-2]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## Keep the retained CLI runtimes as the implementation of `backend: cli`

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-04 · [T20260419-0104], [T20260418-2210]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## Run state, audit, and operator inspection are durable layers

**Recorded:** 2026-05-11 02:06:39.282657Z · [T20260419-0002], [T20260423-0447], [T20260423-2004-4], [T20260426-0526], [T20260426-0519], [T20260426-0705], [T20260426-0709], [T20260425-2010], [T20260426-0742], [T20260426-2313], [T20260426-2349], [T20260430-31], [T20260505-8], [T20260506-18]

### Context
Activity/job execution produces operator evidence at several layers: audit envelopes, job-run records, metrics, live traces, retained blobs, run-inspection commands, PR handoff summaries, and cancellation state. The separate ADRs all instantiate the same rule: runtime output is durable workflow state, not process stdout or live assets pretending to be history.

### Decision
Keep a v2 audit envelope layered over lower-level loop audit, persist direct and pipeline job runs as durable `JobRun` bundles, store file-backed traces under workspace state, read run inspection through runtime accessors, and place public run browsing under `orbit run`. CLI subprocess output may stream through tracing, but retained blobs remain archival; redaction belongs to the tracing subscriber; metrics, execution summaries, and cancellation are persisted as first-class run/task state.

Folded instances:

| ADR | Instance folded into this rollup |
|-----|----------------------------------|
| [Historical workflow inspection must not depend on live seeded job assets](#historical-workflow-inspection-must-not-depend-on-live-seeded-job-assets) | Historical workflow inspection reads stored data, not live seeded assets. |
| [Direct v2 job runs are durable job runs, not audit-only executions](#direct-v2-job-runs-are-durable-job-runs-not-audit-only-executions) | Direct v2 job runs persist durable job-run bundles. |
| [V2 job metrics persist invocation traces beside audit](#v2-job-metrics-persist-invocation-traces-beside-audit) | V2 job metrics persist invocation traces beside audit. |
| [File-backed run traces live under workspace state](#file-backed-run-traces-live-under-workspace-state) | File-backed run traces live under workspace state. |
| [Run inspection reads v2 traces through runtime accessors](#run-inspection-reads-v2-traces-through-runtime-accessors) | Run inspection reads v2 traces through runtime accessors. |
| [Run inspection belongs to `orbit run`](#run-inspection-belongs-to-orbit-run) | Run inspection belongs to `orbit run`. |
| [CLI subprocess output is a live tracing stream and a retained audit blob](#cli-subprocess-output-is-a-live-tracing-stream-and-a-retained-audit-blob) | CLI subprocess output is both a live tracing stream and retained audit blob. |
| [CLI output redaction belongs to the tracing subscriber](#cli-output-redaction-belongs-to-the-tracing-subscriber) | CLI output redaction belongs to the tracing subscriber. |
| [Task PRs require durable execution summaries](#task-prs-require-durable-execution-summaries) | Task PRs require durable execution summaries. |
| [Dashboard cancellation is a durable job-run transition](#dashboard-cancellation-is-a-durable-job-run-transition) | Dashboard cancellation is a durable job-run transition. |

### Consequences
- Reviewers can traverse runs by job, step, activity, and raw loop detail without parsing agent process output as workflow handoff.
- Operator surfaces share durable state for history, metrics, logs, cancellation, and PR handoff.
- The file layout clearly separates command audit queries from run-trace reconstruction files.
- Costs retained from folded entries:
- Cost: audit review now spans two related storage layouts instead of one.
- Cost: some read-only inspection paths no longer shared the same asset-validation gate as active workflow execution paths.
- Cost: direct v2 execution now has persistence side effects and can record synthetic job-level steps that were not literal authored YAML steps.
- Cost: job execution now has another persistence side effect, and CLI metrics remain limited by the provider harness output format.
- Cost: existing local `.orbit/audit/` artifacts are legacy files; readers looking for historical runs may need to check both locations during any manual transition period.
- Cost: the runtime layer now owns a read-side view model for audit JSONL, so envelope schema changes must update both writer and accessor tests together.
- Cost: scripts and muscle memory that used the removed aliases must migrate to the `orbit run` forms.
- Cost: CLI output now has two observability paths; the tracing line text is UTF-8/lossy and newline-stripped while the retained blob bytes remain the archival source.
- Cost: tests that inspect tracing safety must capture formatted subscriber output, not raw `Event` fields.
- Cost: manual or custom-body shipment paths must still persist task summaries before opening the PR, even when the caller already prepared a complete body.
- Cost: direct in-process job runs still cannot safely self-signal; dashboard cancellation is primarily the durable pipeline-worker/operator path.

## Seed reference activities and jobs as load-bearing runtime contracts

**Superseded by:** [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries) (folded)
**Recorded:** 2026-04 · [T20260419-2347], [T20260419-0622-3], [T20260419-0623], [T20260419-0623-2]

Folded into [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries)'s rollup for canonical v2 asset normalization.

## Groundhog is a sibling activity kind, not an `agent_loop` mode bit

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-04 · [T20260420-0510-2]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

**Superseded by ORB-10332:** the Groundhog activity kind was removed as unused; activity specs are now only `agent_loop` and `deterministic`. Retained for history.

## Historical workflow inspection must not depend on live seeded job assets

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-04 · [T20260423-0447]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## Merge object-valued job defaults with caller input, and surface early pipeline failures as synthetic job steps

**Superseded by:** [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries) (folded)
**Recorded:** 2026-04 · [T20260423-0445]

Folded into [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries)'s rollup for canonical v2 asset normalization.

## Direct v2 job runs are durable job runs, not audit-only executions

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-04 · [T20260423-2004-4]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## Job catalog discovery honors layer precedence

**Superseded by:** [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries) (folded)
**Recorded:** 2026-04 · [T20260425-0204]

Folded into [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries)'s rollup for canonical v2 asset normalization.

## Public run workflows are execution aliases only

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-04 · [T20260425-2010]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## CLI backend resolves executor args, not just provider commands

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-04 · [T20260423-0114]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## Activity catalogs honor layer precedence and activity execution stays job-owned

**Superseded by:** [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries) (folded)
**Recorded:** 2026-04 · [T20260426-0047]

Folded into [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries)'s rollup for canonical v2 asset normalization.

## V2 job metrics persist invocation traces beside audit

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-04 · [T20260426-0526]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## File-backed run traces live under workspace state

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-04 · [T20260426-0519]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## Run inspection reads v2 traces through runtime accessors

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-04 · [T20260426-0705], [T20260426-0709]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## Run inspection belongs to `orbit run`

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-04 · [T20260426-0742]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## CLI subprocess output is a live tracing stream and a retained audit blob

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-04 · [T20260426-2313]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## CLI output redaction belongs to the tracing subscriber

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-04 · [T20260426-2349]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## Seeded task-shipment workflows are deterministic, recoverable, and lock-aware

**Recorded:** 2026-05-11 02:06:39.283992Z · [T20260427-33], [T20260425-2010], [T20260427-45], [T20260430-9], [T20260430-12], [T20260430-14], [T20260421-0542-2], [T20260430-27], [T20260430-30], [T20260430-26], [T20260427-34], [T20260427-36], [T20260505-2], [T20260505-10], [T20260506-18], [T20260509-14]

### Context
The seeded task workflows added many small ADRs as shipment behavior grew: run aliases, deterministic auto-dispatch, remote base selection, recovery hooks, backlog exclusions, operator status, and lock cleanup. They are one decision family: task shipment is an explicit durable workflow, not an advisory agent step or hidden side effect.

### Decision
Keep `orbit run` workflow aliases focused on execution, make automatic task shipment deterministic from backlog listing through gate fan-out, default shipping worktrees to fetched remote base refs, admit tasks through status-aware workflow gates, and protect overlapping work with durable task-lock reservations whose seeded TTL covers the child wait budget. Recovery is bounded, step-scoped on direct shipment workflows, and assigned through the configured reviewer role; child pipeline joins are followed by deterministic success guards after required cleanup, operator status is derived from persisted pipeline state, and run-owned reservations clean up when their owner run reaches a terminal state.

Folded instances:

| ADR | Instance folded into this rollup |
|-----|----------------------------------|
| [Public run workflows are execution aliases only](#public-run-workflows-are-execution-aliases-only) | Public run workflows are execution aliases only. |
| [Shipping worktrees default to fetched remote base refs](#shipping-worktrees-default-to-fetched-remote-base-refs) | Shipping worktrees default to fetched remote base refs. |
| [Job-level recovery activity handles retry-exhausted step errors](#job-level-recovery-activity-handles-retry-exhausted-step-errors) | Job-level recovery handles retry-exhausted step errors. |
| [Ship default task-step recovery only on direct shipment workflows](#ship-default-task-step-recovery-only-on-direct-shipment-workflows) | The first direct-shipment recovery default was deterministic and conservative. |
| [Default recovery is step-scoped and agent-driven](#default-recovery-is-step-scoped-and-agent-driven) | Default recovery is step-scoped and agent-driven. |
| [Auto-backlog lock exclusions are structured output](#auto-backlog-lock-exclusions-are-structured-output) | Auto-backlog lock exclusions are structured output. |
| [Auto shipment reports operator workflow status from durable pipeline state](#auto-shipment-reports-operator-workflow-status-from-durable-pipeline-state) | `ship-auto` reports operator workflow status from durable pipeline state. |
| [Gate reservations release after terminal child waits](#gate-reservations-release-after-terminal-child-waits) | Gate reservations release after terminal child waits. |
| [Accepted friction reports enter auto-backlog by status](#accepted-friction-reports-enter-auto-backlog-by-status) | Historical friction-task admission rule; retired by [ORB-10202]. |
| [Run-owned task-lock reservations clean up at owner terminal](#run-owned-task-lock-reservations-clean-up-at-owner-terminal) | Run-owned task-lock reservations clean up at owner terminal. |

### Consequences
- Task shipment workflows expose durable admission, recovery, status, and lock state without asking downstream steps to parse model output.
- Auto-dispatch no longer depends on provider credentials before it has deterministic backlog bundles.
- Gate-owned reservations serialize overlapping bundles while their owner run is alive and are released by both seeded early-release steps and engine-owned terminal cleanup.
- Seeded gate defaults require `ttl_seconds >= dispatch_timeout_seconds` so a legal child shipment wait cannot outlive its admission reservation.
- Costs retained from folded entries:
- Cost: the auto-dispatch audit trail no longer contains a model-authored advisory grouping note.
- Cost: users of `orbit run ship local`, `orbit run ship list/show`, and `orbit run duel list/show` must update their command muscle memory and scripts.
- Cost: default shipping workflows now require the configured base branch to be fetchable from `origin`; callers that intentionally operate without a remote must opt into `base_sync: local`.
- Cost: job authors must make the recovery activity generic enough for every retryable step in that job.
- Cost: this is intentionally conservative; it does not perform semantic git cleanup, task mutation, or child-run reconciliation until a more specific recovery policy is justified.
- Cost: default recovery now depends on the configured reviewer agent being available, and authors must decide which steps deserve recovery rather than flipping one workflow-level switch.
- Cost: the Rust serializer and seeded activity YAML schema now duplicate the exclusion shape and must be kept in sync.
- Cost: the CLI formatter now knows selected fields from `task_auto_pipeline` state, so future pipeline key renames must either preserve compatibility or update the operator summary parser.
- Cost: `task_gate_pipeline` now relies on the dynamic `task_{{ input.mode }}_pipeline` job-name convention, so future gate modes must either follow that naming convention or refactor the dispatch selector.
- Cost: child dispatch status remains data until explicit guard steps run, so seeded workflow authors must preserve guard placement after cleanup when they fork task-shipment YAML.
- Cost: longer default gate reservations can block overlapping work for up to two hours if both explicit release and run-owned cleanup fail.
- Cost: job-run finalization and reservation reserve paths are more coupled, so new terminal run paths must route through the cleanup helper rather than writing directly to the job-run store.

## Shipping worktrees default to fetched remote base refs

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-04 · [T20260427-45]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## Codex CLI dynamic flags stay in provider runtime config

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-04 · [T20260427-48]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## Workflow admission is distinct from generic task updates

**Superseded by:** [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries) (folded)
**Recorded:** 2026-04 · [T20260428-8]

Folded into [Allocate ADR IDs globally via orbit.adr.add before authoring feature decision entries](../orbit-docs/4_decisions.md#allocate-adr-ids-globally-via-orbitadradd-before-authoring-feature-decision-entries)'s rollup for canonical v2 asset normalization.

## `orbit init` is the writer for per-role agent settings

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-04 · [T20260428-9]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## Job-level recovery activity handles retry-exhausted step errors

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-04 · [T20260430-9]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## Ship default task-step recovery only on direct shipment workflows

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-04 · [T20260430-12]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## Default recovery is step-scoped and agent-driven

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-04 · [T20260430-14]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## `[agent.<role>]` config overrides inline `agent_loop` settings at dispatch

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-04 · [T20260428-12]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## CLI agent envelopes carry durable task and run context

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-04 · [T20260430-15]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## Auto-backlog lock exclusions are structured output

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-04 · [T20260421-0542-2]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## Auto shipment reports operator workflow status from durable pipeline state

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-04 · [T20260430-27], [T20260430-30]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## Gate reservations release after terminal child waits

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-04 · [T20260430-26]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## Task PRs require durable execution summaries

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-05 · [T20260430-31]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## Accepted friction reports enter auto-backlog by status

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded; rule retired by [ORB-10202])
**Recorded:** 2026-05 · [T20260505-2]

The historical rule was folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware), then retired when [ORB-10202] removed `friction` from the task status taxonomy.

## Dashboard cancellation is a durable job-run transition

**Superseded by:** [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers) (folded)
**Recorded:** 2026-05 · [T20260505-8]

Folded into [Run state, audit, and operator inspection are durable layers](#run-state-audit-and-operator-inspection-are-durable-layers)'s rollup for durable run state and operator inspection.

## Run-owned task-lock reservations clean up at owner terminal

**Superseded by:** [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware) (folded)
**Recorded:** 2026-05 · [T20260505-10]

Folded into [Seeded task-shipment workflows are deterministic, recoverable, and lock-aware](#seeded-task-shipment-workflows-are-deterministic-recoverable-and-lock-aware)'s rollup for seeded task-shipment workflow automation.

## Provider static-arg fixups apply before sandbox dispatch

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-05 · [T20260505-22]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## `orbit init` uses a recommendation-first setup wizard

**Superseded by:** [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit) (folded)
**Recorded:** 2026-05 · [T20260506-16], [T20260506-17]

Folded into [Host boundaries and agent dispatch stay explicit](#host-boundaries-and-agent-dispatch-stay-explicit)'s rollup for explicit agent dispatch boundaries.

## One-task PR bodies start with the task contract

**Recorded:** 2026-05-11 02:06:39.285327Z · [T20260508-3]

### Context
Task-shipping PRs now carry one task, but the default generated body still reflected the older batch shape. Reviewers had to leave the PR to read the task description and acceptance criteria, while GitHub already rendered the changed-file list natively.

### Decision
Render one-task PR bodies as `## Task`, optional collapsed `## Execution Summary`, `## Validation`, and `## Branch Freshness`. The task section includes the task link, verbatim description, and plain-bullet acceptance criteria. Multi-task callers keep the legacy body while those paths remain supported.

### Consequences
- Cost: _(migration: missing in source)_

## Epic review status is a shipped stop state

**Recorded:** 2026-05-11 02:06:39.286602Z · [T20260427-38]

### Context
`task_epic_pipeline` exits from deterministic `load_epic` snapshots, while normal child shipment workflows stop successful subtasks in `review` for human handoff. Treating `review` as open work made a clean epic cycle redispatch already-shipped subtasks or run until its iteration ceiling.

### Decision
For epic orchestration only, treat `review` as a shipped stop state: `load_epic` omits review subtasks from the open workset, allows them to satisfy `all_terminal`, and maps their epic summary state to `done` while preserving the raw task status.

### Consequences
- Epic loops can converge after normal PR/local child shipment without embedding human approval into the pipeline.
- Operators can still inspect raw `status: "review"` in the final snapshot and task records before approving lifecycle completion.
- Cost: `summarize_epic`'s `done` counter now includes review-shipped subtasks for epic completion, so readers must distinguish pipeline completion from task approval.

## Epic orchestrator does not block on child runs

**Recorded:** 2026-05-11 02:06:39.288004Z · [T20260427-40]

### Context
The `epic_orchestrator` activity exists to make one judgment cycle: read the deterministic epic snapshot, choose ready bundles, and dispatch child `task_gate_pipeline` runs. Its previous instruction also made the HTTP agent call `orbit.pipeline.wait`, but a normal gate-and-ship envelope can exceed the orchestrator's wall-clock by hours: gate admission can wait, child dispatch can wait, and implementer activities have their own long timeout.

### Decision
Keep the orchestrator fire-and-forget. It may call `orbit.pipeline.invoke`, then must return structured `dispatched_run_ids`. `task_epic_pipeline` performs the blocking join through deterministic `pipeline_wait`, then runs `refresh_epic` so loop exit still keys off durable task state. The per-cycle wait budget should satisfy `iteration_wait_seconds >= task_gate_pipeline.max_wait_seconds + task_gate_pipeline.dispatch_timeout_seconds` for full-envelope joins; seeded defaults currently keep `iteration_wait_seconds` at the pipeline wait cap of 7200 seconds, below the theoretical 10800-second gate envelope, so a timeout can surface a still-running child.

### Consequences
- A premium HTTP orchestrator session is bounded to a dispatch decision cycle instead of babysitting child workflow polling.
- Audit lineage moves from agent tool calls to deterministic `ActivityStarted` / `ActivityFinished` envelopes for the join step; the child relationship remains reconstructable from `dispatched_run_ids` and run-step state.
- If `pipeline_wait` times out while a child is still running, the next deterministic `load_epic` snapshot still shows open work. Redundant redispatch is bounded by the gate pipeline's task-lock reservation: overlapping context files are denied while the child reservation is active, and TTL remains the abandoned-run fallback.

## CLI subprocess cwd is runtime-owned workspace state

**Recorded:** 2026-05-11 02:06:39.289462Z · [T20260508-8]

### Context
Per-run worktrees are supposed to isolate task implementation, but `backend: cli` children previously inherited the pipeline worker cwd and only learned the intended workspace through prompt/input data.

### Decision
Resolve CLI subprocess cwd before spawn from `input.workspace_path`, then task snapshot `workspace_path`, then best-effort `ToolContext.workspace_root`. Declared input/task paths fail fast if stale, and the selected cwd is recorded in the CLI started audit event plus line-level tracing.

### Consequences
- The runtime, not the prompt, controls where relative paths in provider CLIs resolve.
- Groundhog and CLI dispatch share one workspace resolver, reducing future drift between orchestration and implementation attempts.
- Cost: stale declared worktrees now fail before spawn instead of silently running from the parent process directory.

## Job executor internals split by execution responsibility

**Recorded:** 2026-05-11 02:06:39.290769Z · [T20260509-2]

### Context
The v2 job executor concentrated step dispatch, retry/recovery, construct orchestration, template rendering, validation, audit projection, and inline tests in one 2.8k-line file.

### Decision
Keep the public job-executor API stable, but organize the implementation as `job_executor/` child modules with `mod.rs` holding the exported entrypoints and private helpers shared through module-scoped visibility.

### Consequences
- Reviewers can inspect retry/recovery, target dispatch, fan-out, loop, validation, and audit behavior in smaller files without changing runtime semantics.
- The split preserves the existing engine/core and CLI-runner boundaries; no new crate edge or provider type crosses the activity/job layer.
- Cost: private helper movement now requires maintaining intra-module visibility and imports across several files instead of one lexical scope.

## Each new executor block ships with a sibling test module

**Recorded:** 2026-05-11 02:06:39.293317Z · [T20260509-7]

### Context
The v2 job executor sub-modules (`step.rs`, `parallel.rs`, `fan_out.rs`, `loop_block.rs`, `target.rs`, `recovery.rs`) own non-trivial concurrency, ordering, and audit invariants. Without test coverage co-located with each block, regressions to those invariants surface only as production failures or as audit-trace anomalies that are hard to reproduce.

### Decision
Every executor-block module under `crates/orbit-engine/src/activity_job/job_executor/` gets a sibling test module in `tests/` whose test function names name the specific invariant or failure mode each test guards. The current layout is `step.rs`, `parallel.rs`, `fanout.rs`, `loop.rs`, and `pipeline_durability.rs`, alongside the pre-existing `audit.rs`, `recovery.rs`, and `target.rs`. Shared scaffolding (`ScriptedHost`, `Action`, job/step builders) lives in `tests/mod.rs` so block modules stay focused on their own invariants and don't fork the host shape. Sandbox and policy boundary coverage lives next to the implementations they guard: `crates/orbit-exec/src/macos_sandbox.rs#tests` (read-deny enforcement and a realistic agent_loop profile boundary) and `crates/orbit-policy/src/engine.rs#tests` (global denyRead/denyModify last-match-wins, unknown-profile error, matched_rule observability).

### Consequences
- Future refactors of an executor block must keep the matching invariant test alive in the same-named test file or update it to reflect the new contract.
- New blocks (e.g. a future `dag` or `gate` construct) must land with a sibling test module covering at least the invariants enumerated in the seed surface.
- Shared scaffolding in `tests/mod.rs` is the consolidation seam — broaden it (agent_loop or shell hosts, additional builders) there rather than re-deriving in each block module.

## Auto-populate `task.context_files` from the winning duel plan

**Recorded:** 2026-05-11 02:06:39.292058Z · [T20260509-9]

### Context
Planning duels already write the winning plan markdown to `task.plan`, but operators were forced to extract the plan's "Context Files" section and push it to `task.context_files` by hand (see T20260509-7's post-hoc fix). `context_files` is the canonical machine-readable handoff to file-lock, focused-read, and scoped-agent consumers; leaving it empty silently degrades every downstream tool that depends on it.

### Decision
During duel resolution, `writeback_planning_duel_task` parses the normalized winning plan for a "Context Files" section and replaces `task.context_files` with the canonicalized entries when extraction succeeds. Section recognition is deliberately strict to keep the failure mode safe (preserve existing field) rather than best-effort:

- A heading line at level `##` or `###` whose trimmed, case-insensitive text equals `context files` or `context_files` (a single trailing `:` is permitted, additional words are not). The section body extends to the next heading of equal-or-higher level, or to end-of-string.
- Within the section body, unindented `- ` or `* ` bullets contribute one entry each: the first inline-code span on the line, otherwise the first whitespace-bounded token after the marker. Sub-bullets and prose lines are ignored.
- Each entry is canonicalized via `orbit_common::utility::selector::canonical_selector`. Raw paths upgrade to `file:` (or `dir:` if trailing `/`); already-canonical `file:` / `dir:` / `symbol:` selectors round-trip unchanged. Entries that fail canonicalization are dropped and reported as `OrbitEvent::PlanningDuelContextFileSkipped` for observability.
- Duplicates collapse in first-seen order. The replace-not-merge semantics mirror `task.plan`: the winning plan is the new source of truth.

When the section is absent OR recognized but yields zero canonical entries (placeholder / all-unparseable), the writeback leaves `task.context_files` untouched. Both branches are asymmetric-with the right safety bias: clearing a curated field on resolution would silently destroy operator state.

The plumbing adds a single optional field to `TaskAutomationUpdate` (`context_files: Option<Vec<String>>`, default `None` = leave untouched, `Some(v)` = replace). The store layer's `TaskRecordUpdateParams.context_files` already supports this shape, so no store changes are required. Plan-writing flows that aren't duel-mediated are explicitly out of scope for this ADR.

### Consequences
- The duel-resolution writeback is no longer a half-conversion: structured task fields stay in sync with the persisted plan markdown.
- Section-recognition heuristics drift between writers is bounded by the strict rule above; future planner agents that emit non-conforming shapes simply fall back to the preserve-existing branch instead of triggering best-effort guesses.
- A new `TaskAutomationUpdate.context_files` field touches every existing automation call site, but the `..Default::default()` pattern keeps each site at the "leave untouched" default. A regression test in `task_host` guards that contract.
- Operators get a `PlanningDuelContextFileSkipped` event channel for debugging stale or malformed plan markdown, instead of silently-dropped entries.

## Condition guards stay equality-only

**Recorded:** 2026-05-11 02:06:39.294685Z · [T20260509-11]

### Context
`task_auto_pipeline` needed to skip its success guard for empty backlog runs, but its seeded `bundle_count > 0` guard rendered to an unsupported comparison and failed before the step could be skipped. Orbit could either extend the shared evaluator with numeric ordering or express the guard in the existing grammar.

### Decision
Keep the shared condition grammar to `==` and `!=`, with `&&` and `||` composition, and express skip-on-empty guards with equality-compatible forms such as `!= 0` and `!= []`. The `ship-auto` guard uses `{{ steps.validate_bundles.output.bundle_count }} != 0`, so zero bundles skip the guard and populated fan-out still checks child gate success.

### Consequences
- The evaluator stays string-based and shared between `StepCondition::Expr`, v2 `when:`, and loop `break_when:` without adding numeric coercion rules.
- Seeded jobs can still model empty collections and counts, but authored guards must avoid ordering operators unless a future task intentionally extends the grammar.
- Cost: authors cannot write natural numeric comparisons in guards today; they must encode supported equality checks or add a deliberate grammar extension with tests and docs.

## CLI timeout supervision owns the subprocess group

**Recorded:** 2026-05-11 02:06:39.295859Z · [T20260509-40]

### Context
`backend: cli` captures stdout/stderr through reader threads while supervising a wall-clock timeout. Killing only the immediate child lets shell-spawned grandchildren survive, keep inherited pipe write ends open, and either hang reader joins or leak background work after a timed-out activity.

### Decision
Spawn bare Unix CLI subprocesses as process-group leaders, matching the existing macOS sandbox wrapper boundary. On timeout, signal the whole child process group with `SIGKILL`, wait for the main child, and bound timeout-path reader joins; after a normal child exit, clean up the same process group before joining readers so orphaned pipe holders do not block capture.

### Consequences
- CLI subprocess supervision has one Unix tree boundary for bare and macOS-sandboxed paths.
- Output capture still preserves partial stdout/stderr bytes already drained before timeout, even if a reader thread does not finish within the bounded join window.
- Cost: Unix process groups do not cover descendants that deliberately create a new session/process group, and non-Unix platforms still use the immediate-child fallback until an equivalent tree-kill primitive is added.

## Legacy parallel-batch workers use cancellable runs

**Recorded:** 2026-05-11 02:06:39.297166Z · [T20260509-38]

### Context
The retained `run_parallel_task_pipeline` automation path used scoped threads to call `run_job_now_with_input_debug`, then marked active workers failed after a long receive timeout. Rust scoped threads still join before the scope exits, so a never-returning worker could keep the parent dispatcher hung even after timeout failure recording.

### Decision
Launch each legacy parallel-batch worker through the durable pipeline surface (`orbit.pipeline.invoke`) and poll active run IDs through `orbit.pipeline.wait` instead of owning scoped worker threads. When the configured worker timeout elapses, the dispatcher cancels every active child run before writing `WORKER_TIMEOUT` task failure state and returning the batch failure.

### Consequences
- Timeout return no longer depends on the worker's thread or agent process eventually exiting.
- Timed-out child work gets the same run-cancellation path operators use elsewhere, including bounded process-group signaling for running pipeline workers.
- Cost: the retained legacy path now depends on the v2 pipeline tool surface and polls active workers, so completion can lag by the polling interval rather than waking on an in-process channel send.

---

## Task References

- **[T20260418-2018]** — Add `JobV2` DAG constructs (`parallel`, `fan_out`, `loop`, `retry`, `when`).
- **[T20260418-2019]** — Add v2 activity name resolution and pipeline skeleton assets.
- **[T20260418-2143]** — Wire `V2RuntimeHost` in orbit-core and add `orbit activity run-v2`.
- **[T20260418-2210]** — Reshape `V2RuntimeHost` to keep `orbit-agent` types out of orbit-core.
- **[T20260419-0002]** — Add `workspace_path` provenance to the v2 audit envelope.
- **[T20260419-0104]** — Add `backend: cli` dispatch for v2 `agent_loop`.
- **[T20260419-0622-3]** — Add `task_gate_pipeline`.
- **[T20260419-0623]** — Add `task_auto_pipeline`.
- **[T20260419-0623-2]** — Add `task_epic_pipeline`.
- **[T20260419-2014]** — Merge `orbit-types` into `orbit-common`.
- **[T20260419-2156]** — Retire v1 assets and drop the transitional v2 naming.
- **[T20260419-2347]** — Seed activities and workflows on `orbit init`.
- **[T20260420-0510-2]** — Add the Groundhog v1 activity runner.
- **[T20260421-0542-2]** — Add structured `list_backlog_tasks` output for context-lock exclusions.
- **[T20260423-0114]** — Expose the `backend: cli` executor-args gap during a local task ship run.
- **[T20260423-0445]** — Merge object-valued job defaults over explicit run input and persist synthetic failed job steps for early v2 pipeline failures.
- **[T20260423-0447]** — Restore usable `orbit run duel` read-only surfaces after duel workflow retirement.
- **[T20260423-2004-4]** — Persist direct v2 `orbit job run` executions into durable job-run records and state.
- **[T20260425-0204]** — Make v2 job catalog discovery honor workspace-over-global `MergeByKey` precedence.
- **[T20260425-2010]** — Refactor `orbit run` task workflow commands and revive `duel-plan` as a seeded run workflow.
- **[T20260426-0047]** — Make v2 activity catalog discovery honor workspace-over-global `MergeByKey` precedence and remove the public `orbit activity run` command.
- **[T20260426-0526]** — Restore v2 job invocation trace persistence so dashboard metrics surfaces can report agent and tool usage.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under `.orbit/state/audit`.
- **[T20260426-0705]** — Expose v2 run audit events through `orbit run events` and `orbit run trace`.
- **[T20260426-0709]** — Align run step selectors on activity `step.id` and move CLI invocation log reading behind orbit-core runtime accessors.
- **[T20260426-0742]** — Remove duplicate job-level run inspection aliases and keep run inspection under `orbit run`.
- **[T20260426-2313]** — Stream CLI subprocess stdout/stderr through structured tracing events while retaining the existing audit/blob path.
- **[T20260426-2349]** — Move CLI tracing output redaction from `cli_runner` call sites into the default tracing formatter layer.
- **[T20260427-33]** — Remove the audit-only `dispatch_agent` step from `task_auto_pipeline`.
- **[T20260427-34]** — Add seeded pipeline success guards so non-succeeded child runs fail parent shipment workflows.
- **[T20260427-36]** — Align task-gate reservation TTL with the child dispatch wait budget.
- **[T20260427-38]** — Treat review as a shipped stop state for epic automation.
- **[T20260427-40]** — Move epic child-run waiting out of the orchestrator agent and into a deterministic workflow step.
- **[T20260427-45]** — Use freshly fetched remote base refs for default task-shipping worktrees.
- **[T20260427-48]** — Thread provider config into the v2 CLI backend and keep Codex dynamic flags exec-compatible.
- **[T20260428-8]** — Add workflow-specific task admission for task-starting workflows.
- **[T20260428-9]** — `orbit init` writes per-role agent settings to `[agent.<role>]` in `config.toml`.
- **[T20260428-12]** — Wire `[agent.<role>]` config into `agent_loop` dispatch via the `role:` field and a host-backed resolver.
- **[T20260430-9]** — Add a job-level recovery activity hook for retry-exhausted v2 step failures.
- **[T20260430-12]** — Ship a generic deterministic recovery activity for direct task shipment workflows.
- **[T20260430-14]** — Make default step recovery agent-driven and step-scoped.
- **[T20260509-14]** — Reuse the configured reviewer role for step-failure recovery.
- **[T20260430-15]** — Embed task-aware input and run context in backend: cli agent envelopes.
- **[T20260430-19]** — Shorten the Activity / Job design docs while preserving required structure.
- **[T20260430-26]** — Release task-gate reservations after terminal child shipment runs and expose active reservations through the lock view.
- **[T20260430-27]** — Make `ship-auto` output distinguish empty backlog, gated no-op, and waiting gate children.
- **[T20260430-30]** — Make `ship-auto` default text output human-readable while preserving JSON fields.
- **[T20260430-31]** — Require populated execution summaries before opening task PRs.
- **[T20260505-2]** — Admit accepted backlog friction reports in automatic backlog listing.
- **[T20260505-8]** — Add dashboard/runtime controls to cancel active job runs.
- **[T20260505-10]** — Release run-owned task lock reservations through engine-owned terminal cleanup and reserve-pressure reconciliation.
- **[T20260505-22]** — Rewrite Claude's `--debug-file` static arg at dispatch time so the log lands at a sandbox-allowed absolute path.
- **[T20260506-16]** — Replace raw `orbit init` agent prompts with a recommendation-first setup wizard.
- **[T20260506-17]** — Make `orbit init` recommend Codex for reviewer and implementer when available.
- **[T20260506-18]** — Compact activity-job ADRs via rollups.
- **[T20260508-3]** — Revise generated task PR bodies around the one-task-per-PR workflow.
- **[T20260508-8]** — Resolve backend: cli subprocess cwd from workspace context and record it in audit/tracing.
- **[T20260509-2]** — Split the v2 job executor into responsibility-focused modules without changing runtime behavior.
- **[T20260509-7]** — Establish focused test coverage for the activity/job DAG executor (linear, retry, parallel, fan-out, loop, pipeline durability) and the macOS sandbox / policy boundary.
- **[T20260509-9]** — Auto-populate `task.context_files` from the winning planning-duel plan after resolution.
- **[T20260509-11]** — Keep condition guards on equality-only grammar and repair the `ship-auto` empty-backlog guard.
- **[T20260509-38]** — Run legacy parallel-batch workers through cancellable pipeline runs so timeout failure paths return promptly.
- **[T20260509-40]** — Run CLI subprocesses in killable process groups and bound timeout-path output reader joins.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Dedicated Groundhog activity kind

**Recorded:** 2026-05-11 02:06:39.339751Z · [T20260420-0510-2]

### Context
Groundhog has its own state, retry loop, and checkpoint-closing builtins. Treating it as an `agent_loop` toggle would have hidden that behavior inside flags and made dispatch harder to reason about.

### Decision
Groundhog is its own `ActivityV2Spec::Groundhog` variant with a dedicated runner.

### Consequences
- Dispatch can validate Groundhog-specific preconditions up front.
- Runtime code gets a clear place to own checkpoint state and snapshot handling.
- Cost: one more activity shape to document, validate, and keep aligned with `agent_loop`.

## HTTP-only first ship

**Recorded:** 2026-05-11 02:06:39.340854Z · [T20260420-0510-2]

### Context
Groundhog relies on a fresh prompt boundary per attempt and on explicit builtin closures. The existing CLI-backend path does not expose the same runtime control surface.

### Decision
Groundhog's shipped runner is HTTP-only. Dispatch rejects providers whose HTTP transport is not wired.

### Consequences
- The first ship stays inside the transport model the runtime already controls.
- The provider/type surface remains narrower in practice than the enum implies.
- Cost: CLI-backed execution gets no Groundhog behavior unless the transport story broadens later.

## Structured checkpoints live in the task plan

**Recorded:** 2026-05-11 02:06:39.341995Z · [T20260420-0509-2], [T20260420-0510-2]

### Context
Groundhog needs a durable, machine-readable checkpoint list. Freeform task plans do not give the runner enough structure to decide what to retry, verify, or record.

### Decision
Groundhog reads typed checkpoints from the task's structured `plan` field.

### Consequences
- Checkpoint identity, success criteria, and retry budget are available to both runtime and agent.
- The task artifact becomes the authoritative source of execution structure.
- Cost: Groundhog inherits the quality of the task plan; weak checkpointing produces weak execution.

## Git scratch branches for rewind

**Recorded:** 2026-05-11 02:06:39.343133Z · [T20260420-0509-4]

### Context
Retrying from a dirty workspace is the main failure mode Groundhog is trying to avoid. The rewind mechanism also needs to survive crashes and remain inspectable after a failed attempt.

### Decision
Each attempt executes on a scratch branch named `groundhog/<task_id>/day-<n>` and rewinds by resetting the task branch back to `snapshot_ref`.

### Consequences
- Failed attempts leave behind inspectable scratch branches.
- Success can be materialized as one squash commit per checkpoint.
- Cost: scratch branches proliferate during long runs and need cleanup discipline.

## Explicit Groundhog builtins close an attempt

**Recorded:** 2026-05-11 02:06:39.344310Z · [T20260420-0509-3], [T20260420-0510-2]

### Context
The runtime needs a crisp signal for "this attempt succeeded" versus "this attempt failed" without parsing freeform assistant text.

### Decision
Groundhog uses dedicated builtins for checkpoint success, checkpoint failure, and side-effect recording. The runner treats missing terminal verbs as synthetic failure.

### Consequences
- Attempt closure is deterministic and machine-readable.
- Retry logic does not depend on assistant prose conventions.
- Cost: the tool surface becomes load-bearing; mismatches between docs and registered builtins are high-risk drift.

## Preserve an append-only chronicle serializer

**Recorded:** 2026-05-11 02:06:39.345468Z · [T20260420-0509]

### Context
Groundhog wants stable checkpoint memory that can be serialized incrementally. Rewriting prior chronicle bytes would make cache-friendly prefix reuse impossible if the runtime ever leans on those helpers.

### Decision
Keep an append-only chronicle serializer contract where earlier serializations are byte-exact prefixes of later ones.

### Consequences
- The runtime has a reusable primitive for stable checkpoint-memory serialization.
- Chronicle history can grow without mutating prior serialized bytes.
- Cost: current runtime persistence is still split across `Chronicle` and `groundhog/state.json`, so the serializer's benefits are only partially realized today.

## Mechanical criteria verify at the checkpoint boundary

**Recorded:** 2026-05-11 02:06:39.346616Z · [T20260420-0510], [T20260420-0510-2]

### Context
Letting the agent self-certify success is too weak for buildable coding tasks. Mechanical checks need to execute outside the conversational loop.

### Decision
Groundhog verifies mechanical success criteria at the checkpoint-success boundary and converts failures into retryable `FailureReport`s.

### Consequences
- Success is gated on workspace reality, not just agent confidence.
- A richer shared verifier can serve non-Groundhog code paths too.
- Cost: the current runner still uses its own thinner inline verifier, so this decision is only partially reflected in implementation.

## Materialize independent review as a post-publication child Run

**Superseded by:** [Retire opt-in post-publication shipment review](#retire-opt-in-post-publication-shipment-review)
**Recorded:** 2026-07-18 07:45:35.717603Z · [ORB-10266]
**Paths:** `crates/orbit-core/assets/jobs/task_*_pipeline.yaml`, `.orbit/resources/jobs/task_*_pipeline.yaml`, `crates/orbit-core/assets/activities/agent_review.yaml`, `.orbit/resources/activities/agent_review.yaml`, `crates/orbit-core/assets/activities/invoke_and_wait.yaml`, `.orbit/resources/activities/invoke_and_wait.yaml`, `crates/orbit-core/src/command/pipeline_run.rs`, `crates/orbit-core/src/runtime/v2_host/pipeline_actions.rs`, `crates/orbit-dashboard/src/projections.rs`

### Context

An inline `agent_review` step ran before the PR candidate was committed, pushed, or published and left no independently addressable review Run. Orbit could keep that inline activity and add more output checks, or materialize review only after publication as its own durable child bound to the pushed SHA.

### Decision

For explicit-task PR shipment with review enabled, dispatch exactly one `task_review_pipeline` child after push, PR publication, and task promotion. Snapshot the parent run, task IDs, workspace, explicit review crew, candidate branch, pushed SHA, and PR identity in the child input; require a structured verdict whose reviewed SHA exactly matches that snapshot. Preflight the selected crew and deployed job/activity contract before inserting the implementation run, and reject review outside PR mode.

### Consequences

- Independent review is observable and resumable through normal job-run records and cannot silently inherit the implementation crew.
- `review=false` keeps the implementation-only shipment path, while review-enabled no-diff and local shipments do not invent an unpublished candidate to review.
- Cost: review-enabled shipment adds a child Run and wait boundary after PR publication, increasing latency and requiring source/shipped workflow assets to stay synchronized.

## Freeze shipped SQLite baselines and preflight worker schema compatibility

**Recorded:** 2026-07-26 22:27:45.849235Z · [ORB-10462]
**Paths:** `crates/orbit-store/src/sqlite/migration/**`, `crates/orbit-core/src/command/job/pipeline.rs`

### Context
The versioned SQLite ledger skips migrations already recorded, so editing the v1 baseline alone changes fresh databases but not legacy databases. A long-lived worker can also retain an older runtime while another Orbit process advances the shared database, causing a late downgrade-guard error only when telemetry reopens the store after agent work. The alternatives were to rerun the mutable baseline on every open or to preserve shipped versions and require append-only upgrades plus an early worker compatibility check.

### Decision
Treat the v1 baseline as an immutable historical artifact guarded by a structural fingerprint. Every schema change after v1 must use a new append-only migration, and tests compare a fresh database with a v1 database upgraded through every registered version. Pipeline workers reopen the store once before claiming the run so compatible pending migrations apply and incompatible newer schemas fail before agent work. Invocation telemetry remains non-fatal and records durable degradation evidence.

### Consequences
- Fresh and legacy databases converge through the same ordered registry, and baseline-only edits fail structural tests.
- Long-lived workers pay one additional SQLite open before claiming a run and cannot discover schema incompatibility only after useful agent work.
- Cost: schema authors must append a migration and update structural expectations instead of editing the fresh baseline in place; the baseline fingerprint is intentionally strict.

## PID-namespace scope decides who may adjudicate job-run liveness

**Recorded:** 2026-08-02 22:06:16.048888Z · [ORB-10594], [ORB-10557]
**Paths:** `crates/orbit-common/src/utility/process_identity.rs`, `crates/orbit-core/src/command/job/run/owner.rs`, `crates/orbit-core/src/command/job/run/reconcile.rs`

### Context

Orbit records a job run's owner as a PID plus a process-start identity token, and the orphan sweep marks a `running` run `interrupted` when both `ps` and `kill(pid, 0)` say that PID is gone. A PID, however, only names a process *within a PID namespace*. Orbit's own sandbox (`bwrap --die-with-parent --new-session --unshare-all --share-net --dev /dev --proc /proc`) gives every sandboxed agent a private PID namespace and a fresh procfs. Inside it, host worker PIDs are invisible, so both probes answer confidently and wrongly: `process_not_found`. Sandboxed agents routinely invoke the Orbit CLI, and many CLI surfaces reconcile.

**Incident 2026-08-02.** `jrun-20260802-2013-2` (`task_pr_pipeline`) ran to a complete success — all 9 steps succeeded, provider PID 83502 exited 0 after ~42 min, PR opened at 20:55:49Z — yet its record read `state: interrupted`, `finished_at: 20:43:00.148Z`, `duration_ms: 1754138` (~29 min). Two sibling runs were condemned in the same pass, 13 ms apart. The condemning process was PID 40790 with cwd `.../worktrees/orbit-jrun-20260802-2013-2` — an agent inside that run's own sandbox. Its PID sequence (40345…40836, +6..10 per invocation) runs concurrently with the host sequence (190031…228874) in the same seconds: two independent monotonic PID allocations, i.e. two namespaces. The false state then cascaded — both parents failed `pipeline_success_guard` on the child's fabricated status.

Across history this is not rare: of 60 `reason=process_not_found` interrupts, **32 were false positives** (the run's audit trail shows activity after its recorded `finished_at`), clustering into synchronized batches — 3 at 20:43:00, 5 at 19:45:23, 6 at 05:07:34, 3 at 01:58:44, 3 at 00:09:14 — the signature of one cross-namespace sweep condemning every live run at once. 27 were genuine deaths, the capability that must be preserved.

**ORB-10557 already diagnosed this root cause** (2026-08-01) and shipped a gate: skip reconciliation when `ORBIT_MANAGED_RUN_CONTEXT` + `ORBIT_RUN_ID` mark a managed child. It did not hold, because the gate wraps exactly one call site — `reconcile_stale_job_runs_on_open` in `OrbitRuntime::from_roots`. Reconciliation has at least five other entry points that are ungated: `job_history`, `list_job_runs`, `show_job_run`, `execute_pipeline_run_worker`, and `release_stale_owned_task_reservations`. The 2026-08-02 sweep entered through the last of these: the audit shows `task.locks.reserve.released` from PID 40790 at 20:43:00.155, bracketing the three condemnations at .148/.154/.161.

### Decision

Make namespace scope a property of the *decision*, not of the entry point.

1. The process-start identity token is versioned up to `ps-lstart-utc-v2:pidns=<inode>:<lstart>`, recording the PID namespace (`/proc/self/ns/pid`) of the process that wrote it. v1 tokens are still read and still verify against a v2 probe on their process-start value, so an in-flight run claimed by an older binary is not invalidated by the upgrade.
2. Owner classification compares the observer's namespace against the recorded one *before* any probe runs. A mismatch yields a new terminal-safe classification, `OwnerIdentity::ForeignPidNamespace`, which joins `ProbeUnavailable` in the never-stale set and is tagged `reason=foreign_pid_namespace` in diagnostics. Cancellation refuses to signal across the boundary for the same reason.
3. A missing namespace on either side yields `Unknown`, which behaves exactly as before. This is deliberately asymmetric: the guard never converts "unknown" into "foreign".
4. An orphaned run's `finished_at` is derived from the last event in its own audit trail (clamped to `[started_at, now]`) rather than the moment of detection.

ORB-10557's env gate is left in place: it is a cheap first line of defense for the common case, and this ADR does not depend on it.

### Rejected alternatives

- *Extend the env gate to every reconciliation entry point.* Cheaper, but it is a per-call-site allowlist that must be re-audited whenever a new caller appears — which is precisely how ORB-10557 failed. It also trusts an inherited environment variable rather than an observable kernel fact.
- *Consult `provider_processes` for liveness.* The table did track PID 83502 correctly throughout. But it is an audit projection of the *provider child*, not the run owner, it is absent for non-CLI steps, and reading it cross-namespace has the identical PID-meaning problem.
- *Add a heartbeat or step-progress signal.* Genuinely useful and orthogonal, but it changes the write path of every step for a defect whose cause is a misread of an existing correct signal. Left for a follow-up.
- *Treat "observer is not in the initial PID namespace" as foreign for tokenless runs.* Rejected: it would disable genuine-orphan detection outright for any deployment whose workers legitimately run inside a container, and it would break detection for the common `pid_start_time: None` case (an owner whose `ps` probe failed at claim time).
- *Weaken `pipeline_success_guard` to tolerate `interrupted`.* Explicitly out of scope. The guard behaved correctly on false input; the input was the defect.

### Consequences

- A run whose owner was recorded in another PID namespace can no longer be condemned from that namespace, through *any* reconciliation entry point, including ones added later.
- Genuine orphan detection is unchanged when observer and owner share a namespace — the case that covers every host-side sweep, `orbit doctor`, and the dashboard.
- Recorded `duration_ms` for genuine orphans no longer includes detection lag, so cost and throughput metrics reading it stop over-counting.
- The false-interrupt vector that automatically released task-file reservations (`StaleRunReconciled`) while the owning run was still editing those files is closed at its source.
- `Cost:` the identity token gains a namespace field, so a v2 token written by a new binary is not byte-comparable with a v1 token written by an old one. Comparison is version-tolerant rather than string equality, which is one more rule for a reader of `process_identity.rs` to hold. A same-namespace PID reuse is still caught; a *cross*-namespace PID reuse is now reported as unverifiable instead of as a mismatch, which is the correct answer but a strictly weaker one.
- `Cost:` runs claimed by a pre-ORB-10594 binary carry no namespace field, so they remain condemnable from a foreign namespace until they finish. The exposure is bounded by run lifetime (hours), and closing it would require the rejected initial-namespace heuristic.
- `Cost:` an orphaned run with no audit trail still falls back to detection time for `finished_at`.

## Classify independent-review startup separately from reviewer rejection

**Recorded:** 2026-08-08 20:34:04.798005Z · [ORB-10606]
**Paths:** `crates/orbit-core/assets/jobs/task_pr_pipeline.yaml`, `crates/orbit-core/src/runtime/v2_host/pipeline_actions.rs`, `crates/orbit-engine/src/executor/automation/vcs/failure.rs`

### Context
A parent shipment previously treated every failed review child identically, so a pre-review infrastructure failure triggered the same blocked/manual-reconciliation handoff as a reviewer rejection. The alternatives were to keep generic child-status gating, weaken the worktree guard, or make the review boundary classify whether a durable reviewer checkpoint exists.

### Decision
Keep the worktree integrity guard unchanged and require review dispatch to supply the complete workspace_path/repo_root pair. At the parent boundary, classify a failed child with no durable reviewer checkpoint as review-not-started, preserve the child diagnostic in the parent failure, and make terminal PR handoff record that event without blocking or republishing the already-published candidate. A child with a reviewer checkpoint, including request_changes, remains a review-ran failure and retains the ordinary blocked handoff.

### Consequences
- Operators can distinguish infrastructure startup failure from a reviewer verdict in the parent run and task history without opening the child run.
- Review-not-started still fails the gated parent, but the task and existing PR stay in review for a clean retry rather than implying that code reconciliation was requested.
- The worktree guard remains fail-closed; safety comes from threading and testing the complete declared path pair for every supported reviewer provider family.
- Cost: the generic pipeline success guard now carries an opt-in review contract and depends on durable child step checkpoints to classify review progress.

## WITHDRAWN — Task owns its reviewer; Orbit holds no reviewer-independence policy

**Recorded:** 2026-08-09 02:02:02.081939Z · [ORB-10628]
**Paths:** `crates/orbit-common/src/types/task.rs`, `crates/orbit-common/src/types/task_artifacts.rs`, `crates/orbit-core/src/command/job/pipeline.rs`, `crates/orbit-core/src/command/workflow.rs`, `crates/orbit-core/assets/jobs/task_pr_pipeline.yaml`, `crates/orbit-core/assets/jobs/task_review_pipeline.yaml`, `crates/orbit-core/assets/activities/agent_review.yaml`, `crates/orbit-store/src/**`, `crates/orbit-dashboard/src/**`, `docs/CONFIG.md`

### Context

**Withdrawn 2026-08-08, never accepted.** This ADR proposed moving review-crew selection onto the task. Daniel subsequently decided to remove independent review from Orbit entirely (ORB-10628), so there is no review routing left to own. The record is kept because the reasoning about mechanism-versus-policy outlived the feature: Orbit ships mechanism, and a rule about who may review whom belongs to the operator. Do not implement this. ORB-10623, ORB-10624, and ORB-10625 were rejected alongside it.

The original context follows.

The review crew is chosen by a ship-time flag, and preflight rejects any review crew that matches a task's implementation crew — by crew name, or by identical model, provider, and backend. That encodes one operator's adversarial-review policy in Orbit itself, and it makes "who reviewed this" a property of an invocation rather than of the task, so review attribution cannot be recovered from durable state. A single ship also carries a single review crew across every task in it. The alternative was to keep ship-level selection and tighten the built-in independence rule to compare provider families, which would have deepened the policy Orbit encodes rather than removing it.

### Decision

Superseded by removal. The proposal was: an optional `reviewer` field on the task names the crew that reviews it and is the source of truth; the ship-time flag applies only to tasks that declare none, and the configured system crew is the final fallback. Orbit would validate only that a named reviewer crew resolves to a usable model, provider, and backend. Review dispatch would fan out one child per reviewed task.

### Consequences

- None. The feature this decision governs is being deleted rather than reworked.
- The mechanism-versus-policy reasoning survives independently: [Retire crew role slots and role-based model resolution](../agent-families/4_decisions.md#retire-crew-role-slots-and-role-based-model-resolution) keeps the rule that routing is expressed through configuration rather than code, and the operator's cross-provider review convention now lives in the operator's own instructions rather than in Orbit.
- Cost: the review attribution problem this ADR was written to solve is not solved, it is removed. If independent review is ever rebuilt on the simplified pipeline, per-task reviewer ownership and the absence of a built-in independence check should be revisited as the starting design rather than rediscovered.

## Remove the planning duel and retain compatibility-only residue

**Recorded:** 2026-08-09 02:47:50.219705Z · [ORB-10627]
**Supersedes:** [Auto-populate `task.context_files` from the winning duel plan](#auto-populate-taskcontextfiles-from-the-winning-duel-plan), [Scope duel-plan candidate and model overrides to \[duel\]](../agent-families/4_decisions.md#scope-duel-plan-candidate-and-model-overrides-to-duel)
**Paths:** `crates/orbit-*/**`, `docs/**`, `website/**`

### Context

The planning duel is unused, duplicates crew-based model selection, and owns thousands of lines across execution, persistence, tooling, configuration, CLI, dashboard, and documentation. Existing workspaces still contain the `[duel]` tables written by older `orbit init`, shipped databases contain a nullable invocation `slot` column, and task bundles may contain historical duel artifacts.

### Decision

Remove the planning-duel activities, job, runner, types, scoreboards, tools, CLI and dashboard surfaces, plus the duel-only per-dispatch model override and role-slot APIs. Continue accepting the retired `[duel]` and `[duel.models]` tables with a warning, leave the shipped nullable SQLite `slot` column in place while ceasing to write it, and leave historical task-bundle artifacts inert rather than migrating or deleting them.

### Consequences

- Agent dispatch selects provider and model only through activity assets and crew resolution.
- Scoreboard summary schema v8 drops duel projections; maintained dashboard, website sync, and README consumers tolerate the resulting shape.
- Existing initialized workspaces keep starting while operators receive explicit cleanup guidance.
- Historical database columns and task artifacts remain readable inert residue.
- Cost: the compatibility warning, frozen nullable SQLite column, and inert task artifacts remain until a future migration window explicitly retires them.

## Retire opt-in post-publication shipment review

**Recorded:** 2026-08-09 05:04:42.091018Z · [ORB-10628]
**Supersedes:** [Materialize independent review as a post-publication child Run](#materialize-independent-review-as-a-post-publication-child-run)
**Paths:** `crates/orbit-core/src/command/job/pipeline.rs`, `crates/orbit-core/assets/jobs/task_*_pipeline.yaml`, `crates/orbit-dashboard/src/api/runs.rs`

### Context

The opt-in post-publication child review made ship submission depend on deployed YAML shape preflight, review-only lineage, and duplicated contracts across four submission adapters. The subsystem added a second orchestration path whose reliability cost outweighed its isolated use. Keeping it dormant would preserve stale operator-facing inputs and hard-coded asset contracts.

### Decision

Remove the independent review activity, guard, child job, shipment inputs, lineage policy, deduplication branch, and deployed-asset contract preflight. Ship submission inserts the selected shipment run directly after the shared in-flight guard. Retain the generic invoke-and-wait activity, pipeline success guard, response-envelope support, and explicit crew resolution for their remaining consumers. Historical task comments remain opaque durable comments.

### Consequences

- Ship has one submission contract across CLI, dashboard, MCP tool, and deterministic action.
- Deployed workflow YAML is no longer loaded and shape-checked before run insertion.
- Existing seeded copies of retired workflow assets must be overwritten or removed during operator upgrade.
- Published execution profiles must be regenerated because the ship-closure digest changes.
- Cost: Orbit no longer offers a built-in opt-in post-publication review gate; teams needing one must compose a separate workflow outside ship.

## Task References

- **[T20260418-2018]** — Add `JobV2` DAG constructs (`parallel`, `fan_out`, `loop`, `retry`, `when`).
- **[T20260418-2019]** — Add v2 activity name resolution and pipeline skeleton assets.
- **[T20260418-2143]** — Wire `V2RuntimeHost` in orbit-core and add `orbit activity run-v2`.
- **[T20260418-2210]** — Reshape `V2RuntimeHost` to keep `orbit-agent` types out of orbit-core.
- **[T20260419-0002]** — Add `workspace_path` provenance to the v2 audit envelope.
- **[T20260419-0104]** — Add `backend: cli` dispatch for v2 `agent_loop`.
- **[T20260419-0622-3]** — Add `task_gate_pipeline`.
- **[T20260419-0623]** — Add `task_auto_pipeline`.
- **[T20260419-0623-2]** — Add `task_epic_pipeline`.
- **[T20260419-2014]** — Merge `orbit-types` into `orbit-common`.
- **[T20260419-2156]** — Retire v1 assets and drop the transitional v2 naming.
- **[T20260419-2347]** — Seed activities and workflows on `orbit init`.
- **[T20260420-0510-2]** — Add the Groundhog v1 activity runner.
- **[T20260421-0542-2]** — Add structured `list_backlog_tasks` output for context-lock exclusions.
- **[T20260423-0114]** — Expose the `backend: cli` executor-args gap during a local task ship run.
- **[T20260423-0445]** — Merge object-valued job defaults over explicit run input and persist synthetic failed job steps for early v2 pipeline failures.
- **[T20260423-0447]** — Restore usable `orbit run duel` read-only surfaces after duel workflow retirement.
- **[T20260423-2004-4]** — Persist direct v2 `orbit job run` executions into durable job-run records and state.
- **[T20260425-0204]** — Make v2 job catalog discovery honor workspace-over-global `MergeByKey` precedence.
- **[T20260425-2010]** — Refactor `orbit run` task workflow commands and revive `duel-plan` as a seeded run workflow.
- **[T20260426-0047]** — Make v2 activity catalog discovery honor workspace-over-global `MergeByKey` precedence and remove the public `orbit activity run` command.
- **[T20260426-0526]** — Restore v2 job invocation trace persistence so dashboard metrics surfaces can report agent and tool usage.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under `.orbit/state/audit`.
- **[T20260426-0705]** — Expose v2 run audit events through `orbit run events` and `orbit run trace`.
- **[T20260426-0709]** — Align run step selectors on activity `step.id` and move CLI invocation log reading behind orbit-core runtime accessors.
- **[T20260426-0742]** — Remove duplicate job-level run inspection aliases and keep run inspection under `orbit run`.
- **[T20260426-2313]** — Stream CLI subprocess stdout/stderr through structured tracing events while retaining the existing audit/blob path.
- **[T20260426-2349]** — Move CLI tracing output redaction from `cli_runner` call sites into the default tracing formatter layer.
- **[T20260427-33]** — Remove the audit-only `dispatch_agent` step from `task_auto_pipeline`.
- **[T20260427-34]** — Add seeded pipeline success guards so non-succeeded child runs fail parent shipment workflows.
- **[T20260427-36]** — Align task-gate reservation TTL with the child dispatch wait budget.
- **[T20260427-38]** — Treat review as a shipped stop state for epic automation.
- **[T20260427-40]** — Move epic child-run waiting out of the orchestrator agent and into a deterministic workflow step.
- **[T20260427-45]** — Use freshly fetched remote base refs for default task-shipping worktrees.
- **[T20260427-48]** — Thread provider config into the v2 CLI backend and keep Codex dynamic flags exec-compatible.
- **[T20260428-8]** — Add workflow-specific task admission for task-starting workflows.
- **[T20260428-9]** — `orbit init` writes per-role agent settings to `[agent.<role>]` in `config.toml`.
- **[T20260428-12]** — Wire `[agent.<role>]` config into `agent_loop` dispatch via the `role:` field and a host-backed resolver.
- **[T20260430-9]** — Add a job-level recovery activity hook for retry-exhausted v2 step failures.
- **[T20260430-12]** — Ship a generic deterministic recovery activity for direct task shipment workflows.
- **[T20260430-14]** — Make default step recovery agent-driven and step-scoped.
- **[T20260509-14]** — Reuse the configured reviewer role for step-failure recovery.
- **[T20260430-15]** — Embed task-aware input and run context in backend: cli agent envelopes.
- **[T20260430-19]** — Shorten the Activity / Job design docs while preserving required structure.
- **[T20260430-26]** — Release task-gate reservations after terminal child shipment runs and expose active reservations through the lock view.
- **[T20260430-27]** — Make `ship-auto` output distinguish empty backlog, gated no-op, and waiting gate children.
- **[T20260430-30]** — Make `ship-auto` default text output human-readable while preserving JSON fields.
- **[T20260430-31]** — Require populated execution summaries before opening task PRs.
- **[T20260505-2]** — Admit accepted backlog friction reports in automatic backlog listing.
- **[T20260505-8]** — Add dashboard/runtime controls to cancel active job runs.
- **[T20260505-10]** — Release run-owned task lock reservations through engine-owned terminal cleanup and reserve-pressure reconciliation.
- **[T20260505-22]** — Rewrite Claude's `--debug-file` static arg at dispatch time so the log lands at a sandbox-allowed absolute path.
- **[T20260506-16]** — Replace raw `orbit init` agent prompts with a recommendation-first setup wizard.
- **[T20260506-17]** — Make `orbit init` recommend Codex for reviewer and implementer when available.
- **[T20260506-18]** — Compact activity-job ADRs via rollups.
- **[T20260508-3]** — Revise generated task PR bodies around the one-task-per-PR workflow.
- **[T20260508-8]** — Resolve backend: cli subprocess cwd from workspace context and record it in audit/tracing.
- **[T20260509-2]** — Split the v2 job executor into responsibility-focused modules without changing runtime behavior.
- **[T20260509-7]** — Establish focused test coverage for the activity/job DAG executor (linear, retry, parallel, fan-out, loop, pipeline durability) and the macOS sandbox / policy boundary.
- **[T20260509-9]** — Auto-populate `task.context_files` from the winning planning-duel plan after resolution.
- **[T20260509-11]** — Keep condition guards on equality-only grammar and repair the `ship-auto` empty-backlog guard.
- **[T20260509-38]** — Run legacy parallel-batch workers through cancellable pipeline runs so timeout failure paths return promptly.
- **[T20260509-40]** — Run CLI subprocesses in killable process groups and bound timeout-path output reader joins.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Unified async ship dispatch

**Recorded:** 2026-07-26 21:51:42.320486Z · [ORB-00075], [ORB-10458]

### Context

Orbit had three shipment aliases: `ship`, `ship-local`, and `ship-auto`. Operators used the auto path because it already queued behind dependency and lock gates, while explicit shipment still failed fast before the waiting-reason surfaces could explain parked work.

### Decision

Use `orbit run ship` as the only public shipment command. Omitted task IDs run backlog auto mode, provided task IDs seed explicit singleton bundles, and both forms submit `task_auto_pipeline`; mode still routes inside `task_gate_pipeline` to `task_{{ input.mode }}_pipeline`. The historical companion-record citation no longer resolves to a surviving body.

### Consequences


- Explicit task selection now waits inside the gated job path instead of failing at CLI dispatch time.
- `orbit run ship` returns after `submit_pipeline_run`, and operators inspect waiting or terminal state with `orbit run history -j task_auto_pipeline` and `orbit run show <RUN_ID>`.
- The deprecated `ship-auto` CLI form errors toward `orbit run ship`, and `ship-local` is no longer a workflow alias.
- Cost: dispatch output no longer contains the former synchronous auto-shipment summary because terminal pipeline state is unavailable at submit time.


## The v2 shell activity surface is removed, not sandboxed

**Recorded:** 2026-08-01 19:14:39.899572Z · [ORB-00374], [ORB-00363], [ORB-10479]

**Context.** The v2 `shell` activity (`ActivityV2Spec::Shell`) dispatched `Command::new(program)` with no OS sandbox, no cwd confinement, and no policy consultation — unlike the `backend: cli` agent path. Its only guard was a `program ∈ allowed_programs` check where both fields came from the same workspace-supplied YAML, so the allowlist was a tautology ([ORB-00363]). The real alternatives were to retrofit the sandbox/policy/cwd pipeline onto `run_shell`, or to remove the surface entirely.

**Decision.** Remove the `shell` activity surface end to end: drop `ShellSpec`, `ActivityV2Spec::Shell`, `run_shell`, the `Shell*` `DispatchError` variants, and every match arm, re-export, demo asset, and doc reference. A workspace activity/job declaring `type: shell` now fails to deserialize at load (the `#[serde(tag = "type")]` enum has no matching variant) instead of executing. Narrow subprocess needs are served by registered `deterministic` actions and the policy-gated `backend: cli` agent path, which enforces `proc_allowed_programs` outside the workspace-supplied spec.

**Consequences.**
- A malicious or careless workspace can no longer obtain unsandboxed arbitrary-program execution through a self-asserted allowlist; the failure mode is fail-closed (load error), not silent execution.
- The only built-in dispatch leaf that produced `Ok(success = false)` is gone; every remaining leaf returns `Ok(success = true)` or `Err`. The structural non-success propagation in the job executor (`StepOutcome.success`, parallel / fan-out / loop aggregation) is retained as the general contract for block-level outcomes and any future fallible-but-`Ok` activity.
- No single code anchor: the constraint is the absence of the variant, enforced by the typed `ActivityV2Spec` enum and review.
- Cost: the `Ok(success = false)` audit-message path lost its only coverage — the two shell-specific tests asserting it were removed rather than migrated, because `deterministic` actions cannot produce that outcome.
- Cost: workspaces that legitimately used `type: shell` must migrate to a registered `deterministic` action or an `agent_loop`; there is no compatibility shim, and old YAML fails at load.

## Pending job runs are owned and reconciled like running runs

**Recorded:** 2026-07-11 21:42:05.454481Z · [ORB-10070]

### Context

A parent pipeline run reaching terminal `interrupted` (crash, reboot) could strand its queued child runs in `pending` forever: pending runs recorded no owner process, `reconcile_stale_job_runs_on_open` only handled orphaned `running` runs, `orbit doctor` only reported running orphans, and no CLI could terminalize a stuck run. Two four-day-old pending gate runs were observed in `codebases/sextant`, demonstrating that stale queue state could persist indefinitely.

### Decision

Give queued runs the same owner-liveness contract as running runs. The pipeline worker claims its run at startup (`claim_pending_job_run_owner` records `pid` + start-time token while the run is still `pending`). Reconcile finalizes a pending run as `interrupted` only when the claimed owner is Mismatch/Missing, or when the run was never claimed and is older than a 30-minute grace window. Inconclusive probes and fresh unclaimed runs stay pending. `orbit doctor` reports both orphan classes, and `orbit run cancel <run_id>` is the manual path.

### Consequences

- Orphaned pending runs self-heal at workspace open and lazily on run list/show.
- Queued runs written by pre-claim binaries whose worker is still alive are shielded only by the grace window; a live legacy queued run older than 30 minutes may be terminalized once, after which its worker exits cleanly and the run remains resumable.
- `pid` on a pending run means claiming worker; `mark_job_run_running` overwrites it when execution starts.
- Cost: the grace-window heuristic can interrupt a still-live legacy queued run because old binaries never record ownership.

## Triage dispositions are applied by a dedicated bounded deterministic action, not update_task or the agent

**Recorded:** 2026-07-11 21:51:26.844937Z · [ORB-10129], [ORB-10243]
**Paths:** `crates/orbit-core/src/runtime/v2_host/triage.rs`, `crates/orbit-core/assets/jobs/task_triage_pipeline.yaml`, `crates/orbit-core/assets/activities/apply_triage_dispositions.yaml`, `crates/orbit-core/assets/activities/triage_failed_runs.yaml`

### Context
The unattended triage agent (ORB-10129) normally returns advisory dispositions to a bounded deterministic writer. ORB-10243 exposed two cases the original design did not cover: work can already be verifiably merged when a later pipeline step fails, and a `stay_blocked` diagnosis for the same coupled failed run otherwise causes a fresh agent diagnosis on every sweep. The alternatives were to keep all reconciliation human-only, or to add a new agent-output disposition applied by the deterministic step; both retain unnecessary repeat work or require trusting advisory output to represent evidence gathered outside that step.

### Decision
`apply_triage_dispositions` remains the only writer for disposition-driven transitions, with its candidates-only, still-blocked, same-coupled-run, environmental-only re-backlog, and durable-budget bounds unchanged. The triage agent has one narrow direct-write exception: for a listed blocked candidate whose own deliverable is conclusively evidenced as merged to the landing branch, it may call `orbit.task.update` to move that candidate from `blocked` to `done` and attach the exact merge evidence. It then returns `stay_blocked`; the deterministic apply step re-reads the task, observes that it is no longer blocked, and skips without a second write.

`list_triage_candidates` suppresses implicit re-triage when the task history already contains a `triage_diagnosis` naming the currently coupled failed run. A different coupled `run_id` is new evidence and remains eligible, while explicit `task_ids` input bypasses suppression for human-requested re-diagnosis.

### Consequences
- Agent output remains advisory; the sole direct lifecycle permission is an evidence-gated `blocked` → `done` write on a candidate the deterministic listing supplied.
- Externally completed work is reconciled without re-running merged work, and the existing still-blocked apply guard provides overlap safety and idempotency.
- Same-run `stay_blocked` verdicts await human action without recurring agent cost, while new failures and explicit requests remain visible.
- Cost: the shipped triage instruction and tool allowlist now carry one auditable lifecycle exception, and same-run suppression depends on the stable `triage_diagnosis` history-note prefix.

## CLI response envelopes are optional for artifact-backed activities

**Recorded:** 2026-07-16 02:08:38.533091Z · [ORB-10231]
**Paths:** `crates/orbit-types/src/workflow/activity_job/**`, `crates/orbit-engine/src/activity_job/cli_runner/**`, `crates/orbit-core/assets/activities/**`

### Context
CLI providers can exit successfully after persisting authoritative task, review, and git artifacts while emitting prose or provider wrapper JSON that lacks an Orbit response envelope. Treating every missing or malformed envelope as fatal strands completed work; treating every response as advisory would break activities whose downstream templates consume structured fields.
### Decision
CLI agent-loop activities treat response envelopes as best-effort by default. Exit status and timeout determine transport success, valid envelopes still project result fields, and parse failures become bounded redacted diagnostics. An activity sets `require_response_envelope: true` only when downstream workflow steps consume its structured response.
### Consequences
- Artifact-backed implementation and review runs no longer fail solely because final agent prose is malformed or missing an envelope.
- Structured-output activities remain fail-closed through an explicit per-activity contract.
- Cost: Activity authors must classify the handoff correctly, and response-consuming activities need a regression that pins strict mode.

## PR handoff recovery follows job checkpoints and exact remote leases

**Recorded:** 2026-08-01 19:17:22.214848Z · [ORB-10232], [ORB-10479]

**Context.** A recovered rebase previously retried the composite `pr_open` action, replaying commit and remote side effects. The alternatives were to make that composite infer prior work from commit subjects and generic divergence, or to expose each handoff phase as durable job state with explicit rewrite provenance.

**Decision.** Model commit, pre-rewrite branch preparation, exact-base rebase, push, PR create-or-reuse, and task promotion as separate `task_pr_pipeline` activity steps. A divergent push is authorized only when a persisted preparation checkpoint names the exact remote SHA observed before the rewrite, the rebase phase confirms that a rewrite occurred, and the push uses a branch-scoped `--force-with-lease=<ref>:<sha>`; all ambiguous or changed remote state fails closed.

**Consequences.**
- Recovery resumes at the first incomplete job step, while step output records whether each phase was performed, skipped, or reused.
- Remote-only commits are never treated as implicit authorization to force-push, and PR retries discover the branch PR before creating one.
- Cost: the shipped workflow and deterministic activity catalog gain three focused activities plus explicit data plumbing between their output schemas.
- Cost: push performs remote inspection/fetch work before classifying non-current refs, and operators must reconcile ambiguous divergence manually.

## Fail delivery before Git mutation when execution outcome is not success

**Recorded:** 2026-07-18 22:01:53.500654Z · [ORB-10313]
**Paths:** `crates/orbit-engine/src/executor/automation/vcs/**`

**Context.** During ORB-10262, `task_pr_pipeline` advanced a task to review and published a PR even though the durable execution summary began `Outcome: failed`. `commit_batch_changes` mutated Git without checking the durable outcome, and `load_handoff_context` treated every nonempty, non-placeholder summary as promotable, so a nonempty summary reporting failure still delivered (friction F2026-07-091). The alternatives were to make the advisory agent response envelope authoritative or to teach `pipeline_success_guard` to parse task prose; both move the source of truth away from the durable task record.

**Decision.** Add one shared durable predicate (`require_delivery_success`) in the VCS handoff seam that reads the persisted task execution summary and requires its first nonblank line to be exactly `Outcome: success`. A `failed`, missing, malformed, or unknown outcome fails closed with a typed error naming the task and rejected value; empty and placeholder summaries keep their existing rejection. Enforce it in `commit_batch_changes` before the checkout is resolved, files staged, the index changed, or a commit created, and again in `load_handoff_context` so every fresh or resumed `pr_prepare`, rebase, push, `pr_open`, `pr_promote`, and no-diff promotion revalidates durable state. The durable task record stays the delivery authority; the response envelope is not made authoritative and `pipeline_success_guard` is unchanged.

**Consequences.**
- Delivery fails closed against the durable record: a failed, unchecked, or malformed outcome cannot commit, push, open a PR, or promote a task, on both fresh and replayed pipelines.
- Agents must write `Outcome: success` as the first nonblank execution-summary line for delivery to proceed; nonempty prose alone is no longer sufficient.
- Cost: the delivery contract now depends on a summary-line convention; a genuinely successful task whose summary omits or misformats the outcome line is blocked until the durable summary is corrected.

## Terminal PR shipment uses a job-level failure handoff

**Recorded:** 2026-07-25 02:34:08.643735Z · [ORB-10363]
**Paths:** `crates/orbit-types/src/workflow/activity_job/**`, `crates/orbit-engine/src/activity_job/**`, `crates/orbit-engine/src/executor/automation/vcs/**`, `crates/orbit-core/assets/jobs/task_pr_pipeline.yaml`

### Context
A task shipment can fail after an agent has produced coherent work but before the normal commit, rebase, push, and PR checkpoints finish. The real alternatives are to overload per-step recovery so it replays or impersonates later checkpoints, or give the workflow one explicit terminal failure hook that preserves the original failure while publishing any recoverable candidate.

### Decision
Add an optional job-level failure activity that runs once after a terminal step failure with the job input, completed pipeline checkpoints, failing step, and structured error. `task_pr_pipeline` binds it to a deterministic PR failure handoff which restores the pre-rebase candidate, commits dirty work, pushes without rewriting unknown remote history, opens or reuses a blocked/manual-resolution PR, and blocks the task while the original run still terminalizes as failed.

### Consequences
- Normal success and retry checkpoints remain unchanged; the failure handoff is an explicit, auditable last-chance side effect.
- A conflict-blocked run is distinguishable through its task status event, PR body, and failure-activity audit even though the original step failure remains authoritative.
- Cost: JobV2 gains another lifecycle hook and task shipment maintains a dedicated deterministic recovery action with conservative Git/remote rules.
- Cost: External push or PR service outages can still prevent publication, but dirty work is committed locally before those fallible operations so terminal runs do not strand uncommitted changes.

## Pipeline steps consume a base commit pinned at worktree setup, never a moving ref name

**Recorded:** 2026-07-25 18:30:35.873033Z · [ORB-10380]
**Paths:** `crates/orbit-engine/src/executor/automation/vcs/**`, `crates/orbit-core/assets/jobs/**`, `crates/orbit-core/assets/activities/**`

**Context.** `worktree_setup` published only the *name* of its start point (`origin/<base>`), and the `commit` step re-resolved that name to decide whether HEAD descended from the base. `refs/remotes/origin/<base>` is shared by every worktree hanging off one `.git`, and any sibling run's setup fetch, any rescue fetch, and every merge moves it. Once it moved, `merge-base --is-ancestor <new tip> HEAD` was false by construction and the commit step failed — so each new run's fetch retroactively invalidated every older in-flight run, making concurrent dispatch unsafe (five failures on 2026-07-25, verified 7/7 against the reflog and reproduced end-to-end). The alternatives were to make `commit` tolerate a moved base by inspecting divergence at commit time — which leaves the step reading state that keeps changing underneath it — or to move base reconciliation earlier, which only relocates the same race.

**Decision.** `worktree_setup` resolves its start point exactly once (`rev-parse --verify <start_point>^{commit}`), creates the worktree at that commit, and emits `base_sha` beside `base_ref`. Both task pipelines pass `base_sha` into `commit`, which reconciles history only against that pinned id; `input.base_sha` must be a full object id, and a ref name is rejected as invalid input so the moving-base failure cannot be reintroduced by wiring. `base_ref` keeps flowing as the moving name to the steps that legitimately want live base state (`sync_base`, `pr_open`), and `sync_base` remains the sole pipeline-owned reconciliation with a base that moved.

Three related repairs land with it. A non-descendant HEAD falls back to `merge-base(base_sha, HEAD)` for commit counting instead of failing; only genuinely unrelated histories are a hard failure. The empty-stage and ancestry diagnostics no longer share one string: each reports observed state (staged/unstaged/untracked counts, HEAD, resolved base, merge-base result) and asserts no cause it did not measure. [No-diff-expected tasks bypass repository change gates](../auto-tasks/4_decisions.md#no-diff-expected-tasks-bypass-repository-change-gates)'s `no-diff-expected` carve-out is evaluated before both failure branches rather than only the empty-stage one, and no failure path mutates the checkout on its way out (the `git reset HEAD` on the empty path is gone).

Commits found above the pinned base are adopted with a loud `warn` naming the shas. Under the pinned base and the pipeline-owned-git-context rule, no sanctioned actor commits during implementation; the known live source is an external editor `Stop` hook that auto-commits inside the worktree. Adoption preserves the work and its authorship, which beats discarding or rewriting it. **Revisit trigger:** once implementing agents and their host hooks are provably non-committing (the activity contract is ORB-10381's), this becomes a hard failure instead of a warning.

**Consequences.**

- Concurrent dispatch is safe again: a sibling run's fetch, a rescue fetch, or a merge landing mid-run cannot fail an older run's commit step.
- A failure in this step now names what was observed, so triage stops inferring a cause from a shared string — the previous message cost three diagnosis cycles, two of which reached confidently wrong conclusions.
- A side-effect-only task no longer hard-fails when its history cannot be reconciled; it skips the phase as [No-diff-expected tasks bypass repository change gates](../auto-tasks/4_decisions.md#no-diff-expected-tasks-bypass-repository-change-gates) intended.
- Cost: `git_commit`'s contract is stricter — a caller that passes only `base_ref` attributes no history at all, rather than silently resolving a name. Direct/leaf invocations must pass a pinned `base_sha` to get commit adoption.
- Cost: unsanctioned commits inside a worktree are tolerated (loudly) until the revisit trigger fires.

Long-form narrative: `docs/design/activity-job/4_decisions.md`.

## The runtime reports its deterministic-action registry, and job validation gates on it

**Recorded:** 2026-07-26 21:51:42.879508Z · [ORB-10385], [ORB-10458]

### Context

Catalog assets and the installed binary are independently versioned artifacts. `pr_failure_handoff` shipped as an activity asset bound to `task_pr_pipeline`'s `failure_activity` ([Terminal PR shipment uses a job-level failure handoff](#terminal-pr-shipment-uses-a-job-level-failure-handoff)) while orbit-core's v2 dispatch table never gained a forwarding arm for it, so the hook answered `deterministic action not registered` on every invocation — jrun-20260725-1620-4, -1642-3, and -1620-10, each after the job had admitted a task, built a worktree, and spent 18–42 minutes implementing and validating it. Nothing was committed, pushed, or published. `worktree_gc` carried the identical gap. A failure hook is the last preservation boundary, so discovering incompatibility *inside* it is the worst possible time. The real alternatives were to version installed assets against the binary (a distribution mechanism for what is really a runtime-capability question, and it cannot see workspace-local catalogs at all), or to make the dispatcher tolerate unknown actions (silently skipping a preservation hook is strictly worse than failing).

### Decision

Make the runtime host report its capability: `V2RuntimeHost::has_deterministic_action(action)` — defaulting to `true` so hosts that cannot enumerate a registry keep surfacing misses at dispatch. `validate_job_deterministic_actions` consults it for every reachable resolved deterministic activity (job- and step-level `recovery_activity`, job `failure_activity`, and every target, recursing through `parallel:`/`fan_out:`/`loop:`) and runs inside `execute_job_with_resume` before the first step, so an unavailable action fails the run ahead of `worktree_setup`'s workflow admission with a `DeterministicActionUnavailable` naming the activity and the action. orbit-core's dispatch table publishes its names as one list that also rejects unlisted actions up front, so the advertised capability cannot exceed what dispatch accepts, and the missing `pr_failure_handoff` / `worktree_gc` arms are registered.

### Consequences


- Catalog/runtime skew is a load-time failure with no task-lifecycle or worktree side effect, instead of a terminal failure that strands completed work.
- A seeded-asset sweep pins the direction that actually broke: every shipped job's reachable actions and every seeded deterministic activity's action must be dispatchable, so a future asset can no longer reference an action the binary lacks without CI failing.
- The failure hook's contract is unchanged: an action that becomes unavailable after admission still leaves the original failed-step error authoritative.
- Cost: hosts now own a capability list that must track their dispatch arms. An over-claiming list rejects nothing at validation and still fails at dispatch (a `debug_assert` catches it in dev); an under-claiming one rejects a healthy job.
- Cost: the check is per-run rather than cached, and a workspace-local activity naming a genuinely new action must ship with a runtime that implements it.


## A worktree's identity is derived once, and a bundle is only collectable when every member has settled

**Recorded:** 2026-07 · [ORB-10427]

**Context.** `orbit gc worktrees` had never reclaimed a byte. `setup_worktree` derived the worktree directory from `task_ids_from_input` (array-first, singular fallback) and named it `orbit-<run_id>`; the collector spelled the same rule out a second time, probing only the singular `task_id` that `task_pr_pipeline` does not emit, missing, and deriving the shared `parallel-batch-<run_id>` path instead. No entry in the collector's known-path map ever matched a real directory, so every worktree fell through to the on-disk sweep and was reported `skipped:unrecognized` — a terminal classification no flag can act on, which reads as "nothing to do" rather than "the collector is broken". Measured on dk-server-1 (binary `0.9.2`, gc source current): 6/6 worktrees in `codebases/orbit`, 4/4 in `knowledgebase/polaris`, 3/3 in `constellation`, `total_bytes_reclaimed=0` in each. The same singular probe attributed the worktree to a task, so repairing only the path derivation would have moved every worktree from `skipped:unrecognized` to `skipped:unattributed` and still reclaimed nothing. Two independent spellings of one rule is the defect; fixing the key probe at both sites would have left the shape intact, and a third site would drift the same way.

**Decision.** One derivation, `WorktreeIdentity::from_input`, owns the task ids, the branch prefix, and the run token; `setup_worktree` creates from it and gc re-derives from it. The token resolves as `input.run_id`, then the engine's run id, then the task-derived fallback (`task-<id>` / `bundle-<hash>`) — setup passes no engine id because it only sees its own input, gc passes the run record's id because a stored `initial_input` never carries the one the engine injected at dispatch. That precedence closes the first divergence; gc also probes the fallback-token path as a second candidate, closing the second (a worktree setup named `task-<id>` was previously invisible to gc). Attribution reads the whole `task_ids` array, and the **bundle rule is unanimity**: a worktree serving several tasks is eligible only when *every* member resolves and is settled to `rejected`/`archived`/`done`, so a bundle is never easier to discard than its least-settled member. The first member that blocks becomes the reported `task_id`/`task_status`; an eligible bundle names all of them.

Recognition is the entire change: no safety gate moved. Non-terminal run, `--older-than-hours`, symlink-or-not-a-directory, unregistered-with-Git, unresolvable task, ineligible status, dirty-rescue, and unknown-branch each still return their own `skipped:*` action, each now pinned by a test that fails if the gate is deleted, and `remove_worktree` is still called without `--force` so a last-moment dirtying makes Git fail closed. `skipped:unrecognized` is preserved for the case it was meant for — a directory matching no run record at all — and is still never deleted.

**Consequences.**
- gc works. Same three repos, dry run, branch build: zero `skipped:unrecognized`, every worktree attributed with a real `run_id`/`run_state`/`task_id`/`task_status`, `would_remove` on 10 of 12 with the other two correctly held by the non-terminal gate — 3.29 GB in `codebases/orbit` alone.
- A dry run now reports the byte estimate per report and in the total, so `--dry-run` answers "how much would this reclaim" instead of always `0`. The envelope's `dry_run: true` remains the statement that nothing was freed.
- Regression fixtures use the shape `task_pr_pipeline` actually emits (`task_ids` array, no `branch_prefix`, no `run_id`), captured from run `jrun-20260726-0305-2`. A fixture hand-built with a singular `task_id` passes against the broken code and proves nothing.
- Cost: gc considers up to two candidate paths per run, so two runs over the same task both claiming the fallback name collide into `skipped:ambiguous_run_path` rather than either being collected. That is the fail-closed direction.
- Out of scope, deliberately: no `--force`/`--include-unrecognized` reap flag for genuinely orphaned directories. Widening what gc will delete is a separate decision from making it see.

---

## Step completion is a separate contract from response content

**Recorded:** 2026-07-26 20:47:48.459433Z · [ORB-10449], [ORB-10454]
**Paths:** `crates/orbit-types/src/workflow/activity_job/activity_v2.rs`, `crates/orbit-agent/src/types/response/envelope.rs`, `docs/design/activity-job/*`

**Context.** [CLI response envelopes are optional for artifact-backed activities](#cli-response-envelopes-are-optional-for-artifact-backed-activities) made CLI response envelopes best-effort for artifact-backed activities, leaving `require_response_envelope` as the only flag that reads an agent-loop invocation's stdout. That flag answers *"do downstream templates consume the response?"* — but it was silently also the only thing answering *"did the invocation finish at all?"*, and for artifact-backed activities (`require_response_envelope: false`) nobody was asking. Every `backend: cli` invocation is prompted with the response-envelope contract, so a provider that yields mid-turn still exits 0: `implement_one` in `task_pr_pipeline` checkpointed as success on work that stopped halfway, and the failure surfaced several steps later at whatever deterministic gate noticed first, attributed to the wrong step. Two real alternatives were rejected. Flipping `require_response_envelope: true` everywhere conflates the content question with the completion question and forces full content validation — exit alignment, `status: success`, object `result` — onto activities whose responses nothing reads, re-breaking exactly what [CLI response envelopes are optional for artifact-backed activities](#cli-response-envelopes-are-optional-for-artifact-backed-activities) fixed. Classifying the violation as a retryable `DispatchError` so `step_failure_recovery` fires inverts the fix: that hook exists to repair the delivery path for *completed* work, so it would publish a stalled implementer's partial candidate.

**Decision.** Split the two questions into two orthogonal flags. `require_completion_envelope` (new, default `true`) is the step-completion protocol contract: under `backend: cli` it reads the envelope *frame* only — presence, supported `schemaVersion`, one of the three protocol status tokens — and never `result` or `error`, so an agent declaring `status: "failed"` satisfies it. `require_response_envelope` (default `false`) keeps its [CLI response envelopes are optional for artifact-backed activities](#cli-response-envelopes-are-optional-for-artifact-backed-activities) meaning as the content contract. The doctrine is therefore intact: agent-loop output stays advisory, because "did the contract complete" is a property of the invocation, not a claim the agent makes about its work. A violation fails the step where it happened (`DispatchOutcome { success: false }`, message naming step and violation), is not retried, does not invoke `recovery_activity`, and still lets the job-level `failure_activity` ([Terminal PR shipment uses a job-level failure handoff](#terminal-pr-shipment-uses-a-job-level-failure-handoff)) preserve recoverable work. `backend: http` is out of scope — the engine's own loop has its own termination accounting. `dispatch_agent` is the single declared opt-out. This amends [CLI response envelopes are optional for artifact-backed activities](#cli-response-envelopes-are-optional-for-artifact-backed-activities) rather than superseding it; that flag keeps its meaning and its status. The flag table, doctrine argument, and full failure/recovery semantics live in §7.6a of the activity-job `2_design.md` and are not restated here.

**Consequences.**
- A stalled CLI agent fails its own step, so triage reads the failure at the site where it happened instead of inferring it from a downstream gate's symptom.
- The default is `true`, so every new `agent_loop` activity inherits the check; opting out is a deliberate, test-pinned edit with a recorded justification in the asset rather than an omission.
- Cost: the completion check is deliberately *more* permissive about stdout stream shape than the content parser — when the JSON document stream will not parse (a wrapped tool sharing stdout, a stray warning line) it falls back to scanning raw text for an embedded envelope, which the content parser does not do. The two gates can therefore return different verdicts on the same invocation: an activity with both flags set can pass completion and fail content. The asymmetry is intentional — failing a step that genuinely completed, over stdout tidiness, is a worse defect than the one this check exists to catch — but nothing in the decision statement implies it, and a reader debugging a mismatched pair has to know it is by design.
- Cost: activity authors now classify two independent questions per activity instead of one, and a miscarried classification is silent in the direction that matters least (an over-strict completion flag fails loudly; an over-permissive one restores the original blind spot).

## Provider launchers resolve at the shared CLI spawn boundary

**Recorded:** 2026-08-01 19:17:24.893725Z · [ORB-10456], [ORB-10479], [ORB-11808]

**Context.** Dashboard shipment reproduced the same provider-launcher ENOENT previously seen in routine sweeps: independent process entry points inherited different `PATH` values even though every `backend: cli` provider invocation converges on one engine spawn boundary. The alternatives were to keep pinning `PATH` in each service/entry point or resolve configured launcher names once at that shared boundary. [ORB-11808] then showed the same seam still missed Apple Silicon Homebrew: launchd's default `PATH` is `/usr/bin:/bin:/usr/sbin:/sbin`, and the `$HOME` fallback list does not include `/opt/homebrew/bin`. Interactive MCP and login-shell pilot inherit that prefix; scheduled drain does not. Pinning Homebrew into the launchd unit would have been a scheduler change; hardcoding a user home would have broken isolation.

**Decision.** Resolve every bare provider launcher at the orbit-engine CLI spawn boundary. Search the process `PATH` first, then portable per-user fallback directories derived from `HOME`, then portable supported system prefixes (`/opt/homebrew/bin`, `/usr/local/bin`). Preserve explicitly pathed commands unchanged. Searched candidates must be regular executable files; missing-launcher failures remain permanent and name the provider plus every searched path.

**Consequences.**
- Dashboard, routine, CLI ship, and direct job dispatch share one provider-launcher resolution mechanism rather than depending on each parent environment being curated.
- Explicit command paths and `PATH` precedence remain authoritative, while common user-local installations and Homebrew prefixes work from scrubbed service environments, including macOS launchd.
- Cost: Orbit now recognizes a small ordered set of conventional user-local and supported-system bin directories outside `PATH`, so moving a launcher into a new convention requires extending and testing that list.

## Resume is a durable submission scoped by explicit retry lineage

**Recorded:** 2026-07 · [ORB-10470]

No separate narrative body survives; the title, date, and [ORB-10470] task reference above are the complete surviving record.
## Workflow admission verifies dependency delivery into the pinned base, not just lifecycle completion

**Recorded:** 2026-07 · [ORB-10464]

No separate narrative body survives; the title, date, and [ORB-10464] task reference above are the complete surviving record.

---

## Primary fast-forward acceptance is decided by interference with the run, not primary dirty-state byte-identity

**Recorded:** 2026-07 · [ORB-10471]

No separate narrative body survives; the title, date, and [ORB-10471] task reference above are the complete surviving record.

---

## Re-dispatched implement attempts self-cancel on a write-gated task

**Decision summary · 2026-07 · [ORB-10499]**

**Context.** A post-recovery implement attempt is deliberately re-dispatched
after a failed attempt. While it runs, another actor can promote the task to a
status that refuses the implementer's final write, so the dispatch-time task
snapshot cannot establish that work remains writable.

**Decision.** Keep the bounded re-dispatch, but include `status` and
`terminal` in the task envelope. The implement instruction stops before costly
work for a terminal task, re-reads durable task status before edits and
validation, and treats a write refusal as a stop rather than retrying around
it.

**Consequences.** The executor retains recovery for failures that leave work
unfinished, while agents avoid producing unpersistable late work. The
completion signal stays advisory; durable task state remains the write gate.

---

## Preserve failed worktree state before cleanup and admit only proven task commits

**Superseded by:** [Workflow alone creates shipment commits while dirty failures remain recoverable](#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable)
**Recorded:** 2026-07 · [ORB-10519]

No separate narrative body survives; the title, date, and [ORB-10519] task reference above are the complete surviving record.

---

## Workflow alone creates shipment commits while dirty failures remain recoverable

**Recorded:** 2026-07-27 04:32:22.796240Z · [ORB-10519]
**Supersedes:** [Preserve failed worktree state before cleanup and admit only proven task commits](#preserve-failed-worktree-state-before-cleanup-and-admit-only-proven-task-commits), [Workflow commit authors use the persisted crew model](../auditability/4_decisions.md#workflow-commit-authors-use-the-persisted-crew-model)
**Paths:** `crates/orbit-engine/src/activity_job/workspace.rs`, `crates/orbit-engine/src/executor/automation/vcs/commit/**`, `docs/design/activity-job/*.md`, `docs/design/auditability/*.md`

### Context
Orbit admitted provider-created commits after external Stop hooks could move an assigned worktree HEAD, which duplicated history, trailer, task-scope, and adoption policy across the provider boundary and commit phase. The alternatives were to retain narrow commit adoption, add another compatibility layer, or restore a single workflow-owned committer while preserving the independent dirty-work recovery and process-scoped attribution decisions.

### Decision
Providers may edit assigned worktree files but must not create commits or otherwise move the assigned HEAD or branch; the provider boundary rejects every such movement with a typed integrity diagnostic. `commit_batch_changes` compares HEAD directly with the immutable setup SHA, stages the worktree diff, and creates exactly one workflow-owned commit without traversing or adopting provider history, parsing provenance trailers, or proving paths against task context. Dirty integrity failures retain the run-keyed tracked patch, untracked payload, and manifest. Workflow commits retain the persisted crew-model author and process-scoped Orbit committer established compatibly by [Automated git commits carry implementer authorship](../auditability/4_decisions.md#automated-git-commits-carry-implementer-authorship) and [Workflow git commit identity is process-scoped](../auditability/4_decisions.md#workflow-git-commit-identity-is-process-scoped), without exporting hook-specific commit-message state.

### Consequences
- A successful implementation leaves only worktree changes for the workflow commit phase, and that phase returns the one SHA it creates.
- Provider-created commits fail at the provider boundary and are never inspected for admission or adopted downstream.
- Dirty integrity failures remain byte-for-byte recoverable after forced linked-worktree cleanup.
- Git authorship remains attributable to the persisted crew model, the committer remains process-scoped, and durable task/run records carry workflow provenance without mandatory Git trailers.
- Cost: a legitimate manual or provider-side commit in an assigned worktree is rejected even when its contents and attribution could have been proven safe; recovery requires returning the candidate to an uncommitted worktree diff before rerunning the workflow.

## Resumed shipment accepts only its durable Orbit preservation commit

**Recorded:** 2026-09-07 · [ORB-11456]
**Extends:** [Workflow alone creates shipment commits while dirty failures remain recoverable](#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable), [Resume is a durable submission scoped by explicit retry lineage](#resume-is-a-durable-submission-scoped-by-explicit-retry-lineage)
**Paths:** `crates/orbit-types/src/workflow/run_state.rs`, `crates/orbit-engine/src/activity_job/job_executor/**`, `crates/orbit-engine/src/executor/automation/vcs/**`, `crates/orbit-core/src/adapter/engine_host/runtime_host.rs`

### Context

The single-committer guard introduced by `48d52bf04` ([ORB-10519]) correctly rejected provider or user movement away from the immutable worktree base. A later terminal `pr_failure_handoff`, however, is itself an Orbit-owned committer: it must commit the dirty candidate before publishing a blocked PR. Resume reused the original successful worktree checkpoint, including its original `base_sha`, but had no structured record authorizing the preservation commit now at HEAD. A resumed implementation could therefore finish new edits only for `git_commit` to misclassify the earlier Orbit preservation as unknown movement. Relaxing the global HEAD guard, replacing the historical base, or trusting the open PR would also admit arbitrary commits or unrelated retries.

### Decision

Persist each successful terminal failure activity in `PipelineState.failure_activity_checkpoint`, separately from successful step outputs so the failed step remains the resume cursor. `pr_failure_handoff` includes the task id, handoff run id, reused worktree owner, original base, exact resulting HEAD, and whether that action created the commit.

When a resumed task-PR workflow has not completed its `commit` step, authenticate a moved HEAD before the first unfinished step runs. The evidence must be an Orbit-created candidate commit from `pr_failure_handoff`; it must match the immutable source-run state, original worktree base, exact current HEAD, task target and current owner; and the active run must descend from the handoff run without crossing jobs. `git_commit` repeats the durable-state, ownership, lineage, base, and exact-HEAD checks immediately before staging. It then treats that one preserved HEAD as the effective starting HEAD while retaining the original `base_sha` as the delivery base. No other changed HEAD is admitted.

### Consequences

- A failed implementation can be preserved, resumed, extended by new edits, committed by the workflow, and delivered through the same PR.
- Original worktree-base evidence and the Orbit preservation commit remain immutable and visible; the resumed workflow commit is a child of the preservation commit and retains normal crew-model authorship.
- Missing or forged evidence, unknown later commits, unrelated retry lineage, and changed task ownership fail before resumed implementation or before staging, without resetting candidate or user changes.
- Completion-tail resumes whose `commit` step already succeeded retain their existing checkpoints and do not reinterpret historical delivery commits.
- Cost: terminal failure activity output becomes durable run state, and resume performs read-only Git and task/run provenance checks before dispatching unfinished work.

## Ship duplicate-dispatch guard lives in the shared submission path

**Recorded:** 2026-08-01 21:00:01.786577Z · [ORB-10544]
**Paths:** `crates/orbit-core/src/application/job/pipeline.rs`, `crates/orbit-dashboard/src/api/runs.rs`, `crates/orbit-core/src/adapter/tool_host/workflow_tools.rs`

**Context.** [ORB-10444] added a duplicate-dispatch guard for explicitly-selected ship tasks and placed it inside `POST /api/workflows/ship`, whose ADR claimed the server-side check "covers every surface". It did not. [ORB-10540] added the MCP `orbit.workflow.ship` tool as a second front door onto the same `OrbitRuntime::submit_ship_run` service, and that tool bypassed the endpoint-local check entirely: two runs could be dispatched for one task and then contend for the same worktree and task reservation. The asymmetry was visible in the equivalence test itself, which had to call HTTP first because that was the only order in which both surfaces could dispatch the same ids.

**Decision.** The in-flight guard is a property of ship submission, not of any adapter. It moves into `OrbitRuntime::submit_ship_run`, ahead of the independent-review preflight and any run insert, and refuses with a typed `OrbitError::ShipRunInFlight { task_id, run_id }` naming the contended task and the run holding it. HTTP and MCP become thin projections of that one conflict: the dashboard maps it in `map_runtime_error` to its stable `409` with code `ship_run_in_flight` plus `run_id` and `task_id`, and the MCP error mapper emits the same code and pair in its structured payload. The guard keys on the explicit selection only — auto (backlog-discovery) submission names no tasks and is untouched, so `orbit run ship-sweep` keeps dispatching.

Rejected alternative: duplicate the check into the MCP adapter and leave one copy per surface. Rejected because it is a policy that must hold for the operation, not for a caller: a third submission adapter would silently start without it, which is exactly how the MCP gap arose. The guard's own regression test now runs against `submit_ship_run` directly rather than through any surface, so a future adapter inherits it by construction.

Rejected alternative: enforce uniqueness at the store layer as a constraint on non-terminal runs carrying a task id. Rejected as a larger change to run persistence for the same coverage, and it would have made auto-mode runs (which carry no task ids at submission but adopt tasks later) an awkward exception.

**Consequences.**
- Every current and future explicit-task ship submission surface refuses a duplicate identically, and the refusal names both ids so a caller can wait on or cancel the in-flight run instead of retrying.
- The HTTP/MCP equivalence test no longer depends on call ordering; each surface ships a disjoint selection, which is what makes the comparison about derived behavior (resolved mode, job) rather than about who went first.
- Cost: the bounded 200-run history scan now runs inside every explicit-task submission, including CLI and future non-HTTP callers that previously skipped it, and a task whose non-terminal run is genuinely stuck cannot be re-shipped from any surface until that run is cancelled — there is deliberately no per-caller override. `OrbitError` also gains a variant that each surface must project or fall back to a generic 500 / `internal_error`.

## Dispatch admission separates unmet dependencies from unsatisfiable ones

**Recorded:** 2026-08-02 22:25:12.480309Z · [ORB-10593]
**Paths:** `crates/orbit-types/src/task/model.rs`, `crates/orbit-core/src/adapter/engine_host/v2_host/dispatch.rs`

### Context

A `blocked_by` edge is satisfied only when the target reaches `done` (`TaskStatus::satisfies_dependency`). Everything else counted uniformly as "unmet", and dispatch treated unmet as "wait": `reserve_locks` returned `reserved: false`, and `task_gate_pipeline` polled 120 times at 30-second intervals before `gate_starvation_fail` ended the run.

That conflates two different situations. A dependency in `backlog` / `in-progress` / `review` can still reach `done`, so waiting is correct. A dependency that is `archived`, `rejected`, or no longer resolves to any task cannot reach `done` by the passage of time — only an operator editing the task graph can clear it. Observed on ORB-10586, which kept `blocked_by: ORB-10576` after ORB-10576 was archived: the run polled for an hour and then failed with a message reporting `conflicting_files`, which was empty, so the diagnostic named no blocker at all and pointed at the wrong subsystem.

### Decision

Admission classifies every non-satisfying dependency edge as either a wait or a dead end, and refuses dead ends immediately.

`TaskStatus::dependency_dead_end()` returns `Some(DependencyDeadEnd)` for `archived` and `rejected` and `None` for every other status; a dependency ID that resolves to no task is `DependencyDeadEnd::Missing`. `unsatisfiable_task_dependencies()` in `orbit-types` applies this per task, and `reserve_locks` fails the activity before the first poll when the set is non-empty, with a `task.dependencies.unsatisfiable:` message naming each offending task/dependency pair, the dependency's status, and its remedy.

What counts as *satisfied* is unchanged: `Done`-only. This decision makes an unsatisfiable edge fail loudly, and deliberately does not widen admission. `Archived` remains a soft-delete that does not satisfy a dependency.

The waiting path is untouched — a reachable dependency still yields `reserved: false` with `waiting_on_deps`, the same poll cadence, and the same eventual `gate.starvation` timeout. That timeout now also reports `waiting_on_deps`, since it previously named no blocker for a dependency-starved bundle either.

Rejected alternatives:

- **Widen `satisfies_dependency()` to accept `archived`/`rejected`.** Treats a soft-deleted or declined task as delivered work, and would silently ship a task whose stated prerequisite never happened. The edge is stale data; the fix is to report it, not to accept it.
- **Leave enforcement to `gate_starvation_fail` and only improve its message.** Cheaper, but still costs a full wait budget (an hour at seeded defaults) per stale edge and reports the failure as starvation, which it is not.
- **Validate at edge-creation time only.** Does not help: the edge was valid when written and became unsatisfiable later, when the dependency was archived.

### Consequences

- A stale `blocked_by` edge now fails in seconds with the blocking IDs named, instead of after the full poll budget with an empty file-conflict list.
- The failure text is distinguishable by prefix: `task.dependencies.unsatisfiable` means the task graph is wrong; `gate.starvation` means waiting was legitimate but ran out of budget.
- The epic-rollup predicate `is_feature_child_terminal_status` in `backlog_exclusion.rs` still folds `archived` and `review` into `"done"` for orchestration state. That surface is deliberately not converged here — it answers "should this child be dispatched again?", not "is this prerequisite delivered?" — so two different notions of terminal now coexist in the codebase and a reader must not assume either one generalizes.
- **Cost:** dependency semantics now live in two predicates that must stay in sync. `satisfies_dependency()` answers "is this edge closed?" and `dependency_dead_end()` answers "could it ever close?", and they partition `TaskStatus` between them. A future status variant that is added to neither is silently treated as a legitimate wait forever — reproducing exactly the hour-long stall this decision removes, but for a status nobody classified. `dependency_dead_end()` matches variants exhaustively rather than using a wildcard so that adding a status breaks the build instead.
- **Cost:** the change converts a class of slow timeouts into fast hard failures. An operator who was restoring an archived dependency inside the old one-hour poll window now sees the gate run fail before the restore lands, and must re-dispatch.

## Delivery fails closed against a base branch that can no longer carry work to the landing branch

**Recorded:** 2026-08-09 06:29:42.495132Z · [ORB-10644]
**Paths:** `crates/orbit-engine/src/executor/automation/vcs/**`, `crates/orbit-core/assets/activities/pr_*.yaml`, `crates/orbit-core/assets/jobs/task_pr_pipeline.yaml`

### Context

A resumed PR pipeline could report successful delivery against a base branch that had already merged and been deleted. The base name flows `input.base_branch -> prepare_branch.output.base -> sync_base.output.base -> pr_open.base` and is never re-derived, and `resolve_worktree_start_point` is satisfied by any `origin/<base>` that resolves — a leftover or restored branch resolves to its pre-merge tip. `open_or_reuse_pr` validated only local divergence of head against the pinned base sha (`branch_freshness_against_ref`) and then handed the base name straight to PR creation. Every step reported success, and the resulting merge did not put the commit on the landing branch. This is the failure mode hardest to notice, because every signal says the work landed.

The question "is this work actually merged into that commit" was already answered once, for dependencies, in `worktree::dependency_delivery` ([Workflow admission verifies dependency delivery into the pinned base, not just lifecycle completion](#workflow-admission-verifies-dependency-delivery-into-the-pinned-base-not-just-lifecycle-completion)): match the `[ORB-NNNNN]` marker every Orbit commit message carries, because squash and rebase rewrite the sha and preserve the message.

### Decision

A base branch is **obsolete** when it can no longer carry work to the branch that work lands on, by either of two tests:

1. **Deleted** — the repository has an `origin` remote and the base branch is gone from it. A PR cannot merge into a branch the remote does not have, so a local or stale remote-tracking ref that still resolves is a leftover, not a target. Always on; it costs one `ls-remote`, which the pipeline already pays for the head branch in `pr_prepare`.
2. **Already landed** — a `landing_branch` input is declared, differs from the base, and the base carries nothing the landing branch does not already have: either the pinned base sha is an ancestor of the landing tip (merge / fast-forward), or every commit unique to the base is already delivered on the landing branch under its task marker (squash / rebase — the shape Orbit's own `merge_batch_pr` produces).

Test 2 reuses `vcs::delivery_marker`, lifted out of `worktree::dependency_delivery` so both gates share one marker rule rather than two that can drift. The pinned `base_sha` is the subject of both tests ([Pipeline steps consume a base commit pinned at worktree setup, never a moving ref name](#pipeline-steps-consume-a-base-commit-pinned-at-worktree-setup-never-a-moving-ref-name), L-0113); only the landing branch is resolved live, because its current tip is exactly what the question is about.

The gate runs at `pr_open` (refuse to create or reuse a PR) and again at `pr_promote` (a resume can enter the pipeline there, with a PR opened before its base landed). `input.base_obsolescence='ignore'` is the escape hatch, mirroring `dependency_delivery`.

Rejected alternatives: probing the base branch's own PR through GitHub (couples the gate to the API, and a local-history rule verifies PR-backed and local-only bases identically); defaulting `landing_branch` to the repository default branch (an integration branch fully merged into `main` right after a release promotion would then be read as obsolete and every ordinary delivery refused).

### Consequences

- The silent failure becomes a loud, phase-labeled refusal (`[phase=obsolete-base]`) naming the stale base, the landing branch, the marker that already landed, and a recovery path.
- Ordinary non-stacked delivery is unchanged: with no `landing_branch`, or with it equal to the base, only the remote-existence probe runs.
- The obsolescence half of the gate is opt-in by declaration. Orbit has no durable notion of a landing branch, and inventing a default is the one change that could refuse healthy production delivery. Stacked dispatchers must set `landing_branch`; without it the pipeline is no worse off than before.
- Cost: two false-positive classes, both escapable with `base_obsolescence='ignore'` — a live base whose only unique commits repeat an already-landed task id (a task re-opened and re-run), and a base deliberately kept off `origin` while an `origin` remote exists.

## Typed deterministic action declaration spans core and engine

**Recorded:** 2026-08-09 06:46:48.177638Z · [ORB-10630]
**Paths:** `crates/orbit-types/src/workflow/activity_job/mod.rs`, `crates/orbit-core/src/adapter/engine_host/v2_host/dispatch.rs`, `crates/orbit-engine/src/executor/automation/mod.rs`

### Context
Deterministic action names had independent core advertisement, core forwarding, engine constants, and engine dispatch lists. The resulting skew shipped asset actions that were not invocable.

### Decision
Declare action names and their core-or-engine ownership once in `orbit-types`, generating typed core and engine action enums plus parsing and advertised names. Core dispatches exhaustively by the shared type, while engine implementation dispatch exhaustively matches the engine enum.

### Consequences
- Adding a declared core or engine action without its respective dispatch arm fails compilation through a non-exhaustive match; implementations cannot name an undeclared typed action.
- Runtime assets still use string action names, and catalog coverage remains responsible for catching invalid external asset references.
- The redundant core dispatch asset-scan guard and its debug assertion are removed because the duplicated registries no longer exist.
- Cost: adding an action requires selecting its ownership in the common declaration, which deliberately makes the cross-crate boundary explicit.

## Job execution crosses one RuntimeHost boundary

**Recorded:** 2026-08-09 07:21:03.861806Z · [ORB-10633]
**Paths:** `crates/orbit-engine/src/context/hosts.rs`, `crates/orbit-core/src/adapter/engine_host/runtime_host.rs`

### Context
The job executor depended on a dispatcher host and a separate deterministic/task/environment/run host family, both implemented by OrbitRuntime. Keeping both families with a documented ownership rule was a real alternative, but it would preserve two call graphs and let capabilities drift between them.

### Decision
Declare one orbit-engine RuntimeHost capability boundary with one OrbitRuntime implementation. The engine parses the shared typed deterministic-action declaration: engine-owned actions execute directly, while core-owned actions cross RuntimeHost once. Orbit-core owns workflow-admission policy.

### Consequences
- A deterministic engine action no longer round-trips through orbit-core before reaching its engine implementation.
- The boundary is readable in one declaration and has one production implementor.
- Cost: the single trait is broad, and focused test hosts must rely on defaults or implement the capabilities they exercise instead of choosing among smaller public host traits.

## Track bundled activity and job ownership by content digest before retirement

**Recorded:** 2026-08-09 22:01:13.919070Z · [ORB-10684]
**Paths:** `crates/orbit-core/src/application/mod.rs`, `crates/orbit-core/src/bootstrap/activity.rs`, `crates/orbit-core/src/application/job/catalog.rs`, `crates/orbit-core/src/bootstrap/init.rs`

### Context
Embedded activity and job assets are materialized into the global resource catalog, but an additive refresh cannot distinguish a retired bundled file from an operator-authored file by name alone. Filename-only pruning would deactivate stale shipped subsystems, but it could also destroy legitimate local resources on legacy installations.

### Decision
Persist a per-resource-kind managed manifest containing the SHA-256 digest last written for each bundled activity or job. Refresh removes retired files only when their bytes still match that digest, moves locally modified retired managed files into a non-catalog backup area, preserves untracked legacy YAML in place, and emits actionable recovery warnings; the same reconciliation implementation governs both resource kinds.

### Consequences
- A current release can retire assets seeded by an earlier manifest-aware release without leaving them active in catalog construction.
- Existing installations without a manifest migrate safely: exact current bundled bytes gain provenance, while untracked YAML remains active and is named in a manual-recovery warning.
- Locally modified retired assets remain recoverable outside active catalog directories instead of breaking unrelated catalog/list operations.
- Cost: managed manifests and preserved-retirement backups add local state and make the first legacy refresh potentially require operator review before an ambiguous stale file can be removed.

## All five definition-artifact kinds carry managed provenance, and doctor reports it

**Recorded:** 2026-08 · [ORB-10800]
**Code anchors:** `crates/orbit-core/src/application/mod.rs::ManagedAssetLayout`, `crates/orbit-core/src/application/skill.rs::seed_default_skills`, `crates/orbit-core/src/application/routine.rs::seed_default_routines`, `crates/orbit-core/src/application/auto_tasks/mod.rs::seed_default_auto_tasks`, `crates/orbit-core/src/application/artifact_health.rs`

### Context

[Track bundled activity and job ownership by content digest before retirement](#track-bundled-activity-and-job-ownership-by-content-digest-before-retirement) gave activities and jobs a digest manifest so a bundled asset dropped
from a release could be retired by content provenance rather than by filename
guessing. The other three definition-artifact kinds — skills, auto-tasks, and
routines — kept the older additive `seed_embedded_assets` path, which skips any
file that already exists and retires nothing. A default skill, auto-task, or
routine removed from a release therefore stayed on disk in every existing
workspace forever: still loadable, still dispatchable, and reported by nothing.

Two obstacles kept the mechanism from generalizing. Skills are directory trees
(`<id>/SKILL.md` plus reference files) while the manifest keyed on `<name>.yaml`.
Routines render placeholders — a host id and a workspace-scoped name — before
being written, so a digest over the *embedded* template would never match disk.

Separately, [Track bundled activity and job ownership by content digest before retirement](#track-bundled-activity-and-job-ownership-by-content-digest-before-retirement)'s reconciliation warnings were emitted once during
bootstrap and then lost, and faulty definitions failed silently: the auto-task
loader's per-file errors were dropped by `auto_task_list`, and `SkillCatalog::list`
swallowed every load error.

### Decision

Extend the [Track bundled activity and job ownership by content digest before retirement](#track-bundled-activity-and-job-ownership-by-content-digest-before-retirement) mechanism to all five kinds rather than giving the three
stragglers a sibling mechanism.

`reconcile_managed_assets` takes a `ManagedAssetLayout`: `YamlStem` keys on
`<name>.yaml` for the four flat catalogs, and `RelativePath` keys on the path
itself for skill trees, so a single skill *reference file* can be retired
independently of its `SKILL.md`. Manifest keys are validated per layout, which
is what keeps a manifest from ever steering a write or a removal outside the
directory it manages. The digest is always taken over the **rendered** document,
so routines and skills — both of which substitute placeholders before writing —
get honest provenance and re-seed as a genuine no-op.

`orbit doctor` gains one row per artifact kind, classifying each artifact as
*faulty* (fails to load), *deprecated* (digest proves Orbit wrote a default this
binary no longer ships), or *stale* (an Orbit-written copy of an older release,
or an untracked file colliding with a bundled default's name). Classification
reads the manifest only. That is deliberate: loader precedence is not uniform —
skills merge workspace-over-global while activities keep shipped defaults
authoritative over workspace copies — so a rule phrased in terms of which copy
wins would misreport at least one kind, while provenance of the file Orbit wrote
is the same question everywhere.

Repair is narrower than diagnosis. `--fix-stale-artifacts` retires only
*deprecated* artifacts whose digest still proves Orbit wrote them; a locally
modified one is preserved outside the active catalog exactly as init-time
reconciliation does, and faulty or user-authored files are never touched. A
faulty artifact is a `Warning` unless it is an unloadable *shipped default*,
which is a broken install and the only artifact fault that escalates
`orbit doctor` to a nonzero exit.

### Consequences

- A default skill, auto-task, or routine dropped from a release now leaves
  existing workspaces instead of lingering indefinitely as dispatchable state.
- The reconciliation signals [Track bundled activity and job ownership by content digest before retirement](#track-bundled-activity-and-job-ownership-by-content-digest-before-retirement) produced only at bootstrap are
  re-derivable on demand, and every non-ok row names the exact repair command.
- Faulty definitions are visible without running the one narrow command that
  happens to touch them; `auto_task_list` now errors only when *nothing* loaded,
  matching what its doc comment always claimed.
- Existing workspaces keep their `orbit doctor` exit code: a malformed
  workspace-authored definition warns rather than failing, so cron and CI
  callers that pass today keep passing.
- Cost: three more managed manifests and a wider retirement blast radius. A
  skill tree is now reconciled file-by-file, so an operator who edits a shipped
  reference file and later sees that file dropped from a release gets a
  preserved copy under `.retired-managed/` to reconcile by hand — where
  previously the file would simply have stayed put and kept working.

## Timeout recovery is not conflict or failure-handoff recovery

**Recorded:** 2026-09-08 · [ORB-11606]

### Context

Host Git helpers shared a 30s deadline. A timeout during `worktree add` could leave a registered incomplete checkout that a retry then admitted, and a timeout during `git rebase` could leave rebase-merge without unmerged paths that a retry misclassified as an unexplained wedge. Ordinary conflict recovery (`RecoverableVcsConflict` / `pr_conflict_recovery`) and terminal failure-handoff (`pr_failure_handoff`) already preserve candidates; they must not be reused as a blanket reset/clean/abort.

### Decision

Give heavyweight Git children explicit finite budgets through `ExecRequest.timeout_ms`, with per-operation defaults and optional activity overlays (`git_timeout_ms` / `git_timeouts`). Reject 0, unknown keys, and values above 600s; never omit the deadline. Keep the secured Git environment and hook policy.

On timeout, mutate only state this attempt owns. Remove a newly registered worktree only when ownership and lack of retained work are established; otherwise refuse with evidence and never admit an incomplete checkout. Abort an interrupted rebase only when provenance shows this attempt started it; leave pre-existing or foreign rebase state intact. Deleted tracked files, dirty files, and retained candidate commits are not completeness failures and must not be restored or deleted as a shortcut. Unexplained stale-branch provenance is [Worktree setup refuses unexplained stale branch reuse before publishing checkpoints](#worktree-setup-refuses-unexplained-stale-branch-reuse-before-publishing-checkpoints).

### Consequences

- A retry after an owned worktree-add timeout either recreates a complete checkout or refuses the leftover; it does not silently reuse a broken tree.
- A retry after an owned rebase timeout is not permanently wedged: the leftover rebase is aborted and documented as timeout recovery.
- Conflicted rebases still take the typed conflict path; failure-handoff still publishes the candidate.
- Cost: operators who previously relied on `git clean -fd` during worktree reuse keep dirty files until they reconcile them; that is the intended preservation.

## Worktree setup refuses unexplained stale branch reuse before publishing checkpoints

**Recorded:** 2026-09-08 · [ORB-11639]

### Context

`worktree_setup` resolves a fresh `base_sha` and then may attach an existing orphan branch or reuse a registered checkout without checking that HEAD already equals that commit. The published checkpoint therefore names a base the worktree is not on, and downstream `git_commit` rejects the mismatch as `worktree_head_changed`. Completeness checks from [Timeout recovery is not conflict or failure-handoff recovery](#timeout-recovery-is-not-conflict-or-failure-handoff-recovery) already refuse to reset retained commits or dirty files; they do not own this provenance question. Validated failure-handoff resume and epic `allow_moved_head` already accept a moved HEAD against the *original* setup checkpoint; they must not be implemented as a setup-time reuse that republishes old history under a new `base_sha`.

### Decision

Before creating a worktree, admitting a task, or returning checkpoints, compare the existing branch tip or registered checkout HEAD with the freshly resolved `base_sha`. Reuse only when they already match. An unexplained mismatch fails closed, naming branch, tip, and requested base, and leaves the leftover untouched — including a retained candidate that has been pushed. Setup does not reset, clean, or attach that history, and does not invent an unchecked branch-reuse escape. `base_ref` stays the moving start-point name; `base_sha` stays the immutable commit a successful setup's HEAD equals.

Retrying setup while the stale leftover remains keeps refusing. Recovery is operator-led: inspect the branch and checkout, then move them aside or delete them only after confirming no retained candidate is needed. Authorized moved-head and failure-handoff continue to run at commit/resume against the original checkpoint.

### Consequences

- A temporary repo whose base moved while an orphan branch or registered checkout stayed behind fails before admission instead of publishing a checkpoint commit will reject.
- Retained candidate commits and dirty work survive the refusal; being pushed is not a license to discard them.
- Cost: a fresh setup that finds unexplained leftover history requires an explicit recovery step. That is cheaper than admitting a run whose later commit cannot reconcile.

## Task References

- **[ORB-11639]** — Validate reused worktree branch provenance before publishing setup checkpoints ([Worktree setup refuses unexplained stale branch reuse before publishing checkpoints](#worktree-setup-refuses-unexplained-stale-branch-reuse-before-publishing-checkpoints)).
- **[ORB-11606]** — Bound heavyweight Git operations and recover only owned interrupted mutations ([Timeout recovery is not conflict or failure-handoff recovery](#timeout-recovery-is-not-conflict-or-failure-handoff-recovery)).
- **[ORB-11456]** — Preserve terminal failure-activity evidence and admit only
  its exact Orbit-created candidate across resume
  ([Resumed shipment accepts only its durable Orbit preservation commit](#resumed-shipment-accepts-only-its-durable-orbit-preservation-commit)).
- **[ORB-10800]** — Extend managed provenance to skills, auto-tasks, and
  routines, and add the `orbit doctor` definition-artifact check plus
  `--fix-stale-artifacts` ([All five definition-artifact kinds carry managed provenance, and doctor reports it](#all-five-definition-artifact-kinds-carry-managed-provenance-and-doctor-reports-it), extending [Track bundled activity and job ownership by content digest before retirement](#track-bundled-activity-and-job-ownership-by-content-digest-before-retirement)).
- **[ORB-10684]** — Reconcile retired managed activity and job assets by
  content provenance while preserving modified and ambiguous legacy files
  ([Track bundled activity and job ownership by content digest before retirement](#track-bundled-activity-and-job-ownership-by-content-digest-before-retirement), Proposed).
- **[ORB-10644]** — Refuse to open or promote a PR against a base branch that is gone from `origin` or has already landed on the declared landing branch ([Delivery fails closed against a base branch that can no longer carry work to the landing branch](#delivery-fails-closed-against-a-base-branch-that-can-no-longer-carry-work-to-the-landing-branch), extending [Workflow admission verifies dependency delivery into the pinned base, not just lifecycle completion](#workflow-admission-verifies-dependency-delivery-into-the-pinned-base-not-just-lifecycle-completion)'s marker rule to the base itself).
- **[ORB-10593]** — Fail dispatch immediately when a `blocked_by` target is archived, rejected, or dangling, naming the blocker ([Dispatch admission separates unmet dependencies from unsatisfiable ones](#dispatch-admission-separates-unmet-dependencies-from-unsatisfiable-ones)).
- **[ORB-10544]** — Move the ship in-flight duplicate-dispatch guard into `submit_ship_run` so HTTP and MCP are thin projections of one typed conflict ([Ship duplicate-dispatch guard lives in the shared submission path](#ship-duplicate-dispatch-guard-lives-in-the-shared-submission-path), correcting the surface-local check from [One-Click Task Ship and Human-Attributed Dashboard Comments](../user-interface/4_decisions.md#one-click-task-ship-and-human-attributed-dashboard-comments)).
- **[ORB-10631]** — Route interactive `orbit run ship` through `submit_ship_run` as well, completing the shared submission boundary for CLI, dashboard, MCP, and routine dispatch ([Ship duplicate-dispatch guard lives in the shared submission path](#ship-duplicate-dispatch-guard-lives-in-the-shared-submission-path)).
- **[ORB-10630]** — Derive deterministic action advertisement, forwarding, and engine dispatch from one typed common declaration ([Typed deterministic action declaration spans core and engine](#typed-deterministic-action-declaration-spans-core-and-engine), Proposed).

- **[ORB-10519]** — Restore one workflow-owned shipment commit, reject every provider-side HEAD change, and preserve dirty-work recovery plus process-scoped attribution ([Workflow alone creates shipment commits while dirty failures remain recoverable](#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable), superseding [Preserve failed worktree state before cleanup and admit only proven task commits](#preserve-failed-worktree-state-before-cleanup-and-admit-only-proven-task-commits) and [Workflow commit authors use the persisted crew model](../auditability/4_decisions.md#workflow-commit-authors-use-the-persisted-crew-model)).
- **[ORB-10499]** — Confirm the duplicate implement invocation as the executor's bounded post-recovery attempt, and let the re-dispatched attempt exit on a write-gated task (resolving [F2026-07-174]).
- **[ORB-10468]** — Introduce run-keyed dirty integrity recovery plus the now-superseded provider-commit admission policy ([Preserve failed worktree state before cleanup and admit only proven task commits](#preserve-failed-worktree-state-before-cleanup-and-admit-only-proven-task-commits), superseded by [Workflow alone creates shipment commits while dirty failures remain recoverable](#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable)).
- **[ORB-10471]** — Scope the worktree boundary guard's primary dirt check to paths the run touched, so unrelated primary dirt no longer defeats a benign fast-forward ([Primary fast-forward acceptance is decided by interference with the run, not primary dirty-state byte-identity](#primary-fast-forward-acceptance-is-decided-by-interference-with-the-run-not-primary-dirty-state-byte-identity)).
- **[ORB-10470]** — Make resume submit a detached run that starts at the failed checkpoint, and reconcile blocked/re-stamped tasks against the run's retry lineage ([Resume is a durable submission scoped by explicit retry lineage](#resume-is-a-durable-submission-scoped-by-explicit-retry-lineage)).
- **[ORB-10456]** — Resolve provider launchers at the shared CLI spawn boundary and add provider-aware missing-launcher diagnostics ([Provider launchers resolve at the shared CLI spawn boundary](#provider-launchers-resolve-at-the-shared-cli-spawn-boundary)).
- **[ORB-11808]** — Search `/opt/homebrew/bin` and `/usr/local/bin` after `PATH` and `$HOME` bins so launchd-minimal Mac drains resolve Homebrew provider CLIs at the same seam.
- **[ORB-10454]** — Allocate [Step completion is a separate contract from response content](#step-completion-is-a-separate-contract-from-response-content) for the step-completion / response-content split and retire the IOU in [CLI response envelopes are optional for artifact-backed activities](#cli-response-envelopes-are-optional-for-artifact-backed-activities)'s amendment block.
- **[ORB-10427]** — Share one worktree-path derivation between `setup_worktree` and gc; collect bundles only when every member has settled.
- **[ORB-10393]** — Port planning-duel planner and arbiter legs to seeded v2
  assets with per-slot model overrides and retire `DeterministicActionHost::invoke_activity`.
- **[ORB-10385]** — Gate job admission on the runtime's deterministic-action registry; register `pr_failure_handoff` and `worktree_gc`.
- **[ORB-10380]** — Pin `base_sha` from `worktree_setup` through `git_commit`; split the commit step's failure diagnostics.
- **[T20260418-2018]** — Add `JobV2` DAG constructs (`parallel`, `fan_out`, `loop`, `retry`, `when`).
- **[T20260418-2019]** — Add v2 activity name resolution and pipeline skeleton assets.
- **[T20260418-2143]** — Wire `V2RuntimeHost` in orbit-core and add `orbit activity run-v2`.
- **[T20260418-2210]** — Reshape `V2RuntimeHost` to keep `orbit-agent` types out of orbit-core.
- **[T20260419-0002]** — Add `workspace_path` provenance to the v2 audit envelope.
- **[T20260419-0104]** — Add `backend: cli` dispatch for v2 `agent_loop`.
- **[T20260419-0622-3]** — Add `task_gate_pipeline`.
- **[T20260419-0623]** — Add `task_auto_pipeline`.
- **[T20260419-0623-2]** — Add `task_epic_pipeline` (surface later removed in [ORB-10332]).
- **[T20260419-2014]** — Merge `orbit-types` into `orbit-common`.
- **[T20260419-2156]** — Retire v1 assets and drop the transitional v2 naming.
- **[T20260419-2347]** — Seed activities and workflows on `orbit init`.
- **[T20260420-0510-2]** — Add the Groundhog v1 activity runner (kind later removed in [ORB-10332]).
- **[T20260421-0542-2]** — Add structured `list_backlog_tasks` output for context-lock exclusions.
- **[T20260423-0114]** — Expose the `backend: cli` executor-args gap during a local task ship run.
- **[T20260423-0445]** — Merge object-valued job defaults over explicit run input and persist synthetic failed job steps for early v2 pipeline failures.
- **[T20260423-0447]** — Restore usable `orbit run duel` read-only surfaces after duel workflow retirement.
- **[T20260423-2004-4]** — Persist direct v2 `orbit job run` executions into durable job-run records and state.
- **[T20260425-0204]** — Make v2 job catalog discovery honor workspace-over-global `MergeByKey` precedence.
- **[T20260425-2010]** — Refactor `orbit run` task workflow commands and revive `duel-plan` as a seeded run workflow.
- **[T20260426-0047]** — Make v2 activity catalog discovery honor workspace-over-global `MergeByKey` precedence and remove the public `orbit activity run` command.
- **[T20260426-0526]** — Restore v2 job invocation trace persistence so dashboard metrics surfaces can report agent and tool usage.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under `.orbit/state/audit`.
- **[T20260426-0705]** — Expose v2 run audit events through `orbit run events` and `orbit run trace`.
- **[T20260426-0709]** — Align run step selectors on activity `step.id` and move CLI invocation log reading behind orbit-core runtime accessors.
- **[T20260426-0742]** — Remove duplicate job-level run inspection aliases and keep run inspection under `orbit run`.
- **[T20260426-2313]** — Stream CLI subprocess stdout/stderr through structured tracing events while retaining the existing audit/blob path.
- **[T20260426-2349]** — Move CLI tracing output redaction from `cli_runner` call sites into the default tracing formatter layer.
- **[T20260427-33]** — Remove the audit-only `dispatch_agent` step from `task_auto_pipeline`.
- **[T20260427-34]** — Add seeded pipeline success guards so non-succeeded child runs fail parent shipment workflows.
- **[T20260427-36]** — Align task-gate reservation TTL with the child dispatch wait budget.
- **[T20260427-38]** — Treat review as a shipped stop state for epic automation.
- **[T20260427-40]** — Move epic child-run waiting out of the orchestrator agent and into a deterministic workflow step.
- **[T20260427-45]** — Use freshly fetched remote base refs for default task-shipping worktrees.
- **[T20260427-48]** — Thread provider config into the v2 CLI backend and keep Codex dynamic flags exec-compatible.
- **[T20260428-8]** — Add workflow-specific task admission for task-starting workflows.
- **[T20260428-9]** — `orbit init` writes per-role agent settings to `[agent.<role>]` in `config.toml`.
- **[T20260428-12]** — Wire `[agent.<role>]` config into `agent_loop` dispatch via the `role:` field and a host-backed resolver.
- **[T20260430-9]** — Add a job-level recovery activity hook for retry-exhausted v2 step failures.
- **[T20260430-12]** — Ship a generic deterministic recovery activity for direct task shipment workflows.
- **[T20260430-14]** — Make default step recovery agent-driven and step-scoped.
- **[T20260509-14]** — Reuse the configured reviewer role for step-failure recovery.
- **[T20260430-15]** — Embed task-aware input and run context in backend: cli agent envelopes.
- **[T20260430-19]** — Shorten the Activity / Job design docs while preserving required structure.
- **[T20260430-26]** — Release task-gate reservations after terminal child shipment runs and expose active reservations through the lock view.
- **[T20260430-27]** — Make auto shipment output distinguish empty backlog, gated no-op, and waiting gate children.
- **[T20260430-30]** — Make auto shipment default text output human-readable while preserving JSON fields.
- **[T20260430-31]** — Require populated execution summaries before opening task PRs.
- **[T20260505-2]** — Admit accepted backlog friction reports in automatic backlog listing.
- **[T20260505-8]** — Add dashboard/runtime controls to cancel active job runs.
- **[T20260505-10]** — Release run-owned task lock reservations through engine-owned terminal cleanup and reserve-pressure reconciliation.
- **[T20260505-22]** — Rewrite Claude's `--debug-file` static arg at dispatch time so the log lands at a sandbox-allowed absolute path.
- **[T20260506-16]** — Replace raw `orbit init` agent prompts with a recommendation-first setup wizard.
- **[T20260506-17]** — Make `orbit init` recommend Codex for reviewer and implementer when available.
- **[T20260506-18]** — Compact activity-job ADRs via rollups.
- **[T20260508-3]** — Revise generated task PR bodies around the one-task-per-PR workflow.
- **[T20260508-8]** — Resolve backend: cli subprocess cwd from workspace context and record it in audit/tracing.
- **[T20260509-2]** — Split the v2 job executor into responsibility-focused modules without changing runtime behavior.
- **[T20260509-7]** — Establish focused test coverage for the activity/job DAG executor (linear, retry, parallel, fan-out, loop, pipeline durability) and the macOS sandbox / policy boundary.
- **[T20260509-9]** — Auto-populate `task.context_files` from the winning planning-duel plan after resolution.
- **[T20260509-11]** — Keep condition guards on equality-only grammar and repair the `task_auto_pipeline` empty-backlog guard.
- **[ORB-00075]** — Unify ship aliases into async `orbit run ship`.
- **[T20260509-38]** — Run legacy parallel-batch workers through cancellable pipeline runs so timeout failure paths return promptly.
- **[T20260509-40]** — Run CLI subprocesses in killable process groups and bound timeout-path output reader joins.
- **[ORB-00363]** — Security bug: `run_shell` spawned unsandboxed subprocesses behind a tautological allowlist.
- **[ORB-00374]** — Remove the `shell` activity variant and `run_shell` dispatch (fail-closed resolution of [ORB-00363]).
- **[ORB-10202]** — Remove the retired friction task status while preserving workflow admission and triage behavior.
- **[ORB-10232]** — Model recoverable PR handoff as checkpointed job activities with exact-SHA force-push provenance.
- **[ORB-10313]** — Fail delivery before Git mutation when the durable execution outcome is not `Outcome: success`.
- **[ORB-10363]** — Rebase task candidates after concurrent base advances and publish blocked PRs instead of stranding failed work.
- **[ORB-10332]** — Remove the unused Groundhog activity kind and the epic/parallel pipeline layer (`task_epic_pipeline`, `epic_orchestrator`, `pipeline_wait`, legacy parallel-batch executor).
- **[ORB-10449]** — Split step-completion protocol from response content so a stalled agent-loop step fails where it happened ([Step completion is a separate contract from response content](#step-completion-is-a-separate-contract-from-response-content), amending [CLI response envelopes are optional for artifact-backed activities](#cli-response-envelopes-are-optional-for-artifact-backed-activities); see [§7.6a of `2_design.md`](./2_design.md)).
- **[ORB-10746]** — Add the prevention layer behind [Step completion is a separate contract from response content](#step-completion-is-a-separate-contract-from-response-content)'s detector: Claude CLI invocations are constrained by a canonical response-envelope JSON Schema via `--json-schema`, generated from one protocol definition. The status/error correlation stays in Rust because Anthropic's structured-output subset rejects top-level conditional subschemas — a constraint, not a choice — and the completion/status checks remain provider-neutral fail-closed backstops that no wrapper signal can turn into a success. Narrative in [§7.6b of `2_design.md`](./2_design.md); no new ADR was allocated, since this decides nothing [Step completion is a separate contract from response content](#step-completion-is-a-separate-contract-from-response-content) left open.
- **[ORB-10464]** — Refuse workflow admission when a done dependency's work is not in the base the worktree would be cut from ([Workflow admission verifies dependency delivery into the pinned base, not just lifecycle completion](#workflow-admission-verifies-dependency-delivery-into-the-pinned-base-not-just-lifecycle-completion), resolving [F2026-07-038]).
- **[ORB-10604]** — Split remote worktree construction from local merge reconciliation so post-merge failures do not cascade through a shipment session.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
