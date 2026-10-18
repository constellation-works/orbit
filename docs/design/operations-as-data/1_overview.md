---
title: Operations as Data — Overview
owner: claude
last_updated: 2026-10-18
last_validated: 2026-10-18
status: Accepted
feature: operations-as-data
doc_role: overview
type: design
summary: Declaring each verb of a noun once as data so CLI, MCP, dashboard, and runtime handlers are derived adapters instead of hand-copied layers; friction, auto-task, drain, workspace claim, every task tool and the workflow-run tools are migrated onto kernel v2 and validate input at the tool entry, and a planned noun-by-noun migration is accepted and in progress.
tags: [operations-as-data, architecture, adr-0209]
paths: ["crates/orbit-common/src/governance/operation.rs", "crates/orbit-common/src/governance/drain.rs", "crates/orbit-common/src/governance/workspace_claim.rs", "crates/orbit-common/src/governance/friction/**", "crates/orbit-common/src/governance/auto_task/**", "crates/orbit-tools/src/builtin/orbit/operation.rs", "crates/orbit-tools/src/builtin/orbit/auto_task.rs", "crates/orbit-cli/src/command/operation/args.rs", "crates/orbit-cli/src/command/auto_task/**", "crates/orbit-common/src/governance/task/**", "crates/orbit-cli/src/command/task/operation.rs", "crates/orbit-common/src/governance/workflow.rs", "crates/orbit-tools/src/builtin/orbit/workflow.rs", "crates/orbit-core/src/adapter/tool_host/workflow_tools.rs", "crates/orbit-common/src/governance/search.rs"]
related_features: [operations-as-data, orbit-core]
related_artifacts: [ORB-10358, ORB-15932, ORB-15933, ORB-15935, ORB-15936, ORB-15937, ORB-15938, ORB-15940, ORB-15941, ORB-15942]
---

# Operations as Data — Overview

Orbit exposes the same underlying operations through four surfaces: the CLI, MCP,
the web dashboard (`orbit-web`), and the in-runtime tool host. Historically each
surface restated every verb by hand — its name, its parameters, its help text, its
field-to-JSON mapping — so a noun with seven verbs was written out four times and
drifted four ways. **Operations as data** replaces that with one declaration per
verb: a `const` spec that every surface derives its wiring from. This is bearing 1
of [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry).

**Status.** The friction noun was migrated end to end in [ORB-10358], and the
auto-task noun followed in [ORB-15935] (phase 1). Drain and workspace claim
followed in [ORB-15936] (phase 2), migrating nine tool-only verbs. The task
read verbs (`show`, `list`, `eligible`, `lint`) followed in [ORB-15937]
(phase 3a), which also settled that ID routing stays keyed by tool name, and
task `add` and `update` followed in [ORB-15938] (phase 3b), with the guarded
creation schema as a surface table. Task artifacts and locks followed in
[ORB-15939] (phase 3c), adding registry-derived nested CLI groups, and the
review, reject and delete tools in [ORB-15940] (phase 3d), adding CLI action
subcommands; the task subcommands with no tool stay hand-wired. The five
workflow-run tools followed in [ORB-15941] (phase 4) as a tool-only noun; the
`orbit run` CLI stays hand-wired. Phase 5 migrated search in [ORB-15942]. The scoped migration is complete;
routine control and the four host invocation/execution tools remain hand-wired
with per-tool verdicts in [2_design.md §9](./2_design.md#9-migration-plan). A
2026-10-12 review found that the friction kernel declared a verb's *names* once
but still restated its *behaviour* — read/write class, redaction, accepted
aliases, cross-field rules — on several surfaces, and that it could not yet
express what the task and auto-task commands need. The revised design (kernel
v2) and a planned migration were accepted on 2026-10-12, replacing the
touch-it-move-it ratchet, which migrated no noun in the eleven weeks it
governed. [ORB-15932] built kernel v2's structure and ported friction onto it:
read/write class and redaction are now declared in the spec. [ORB-15933]
declared accepted aliases, retired fields and the "change something" rule, and
validates friction input once at the tool entry, warning about unknown fields
before a later step refuses them. [ORB-15935] added the switch, enum, object
and CLI-only parameter kinds, flag defaults and aliases, and moved every
`orbit auto-task` verb onto the tool, including the six that were CLI-only. The
work is tracked as
Orbit tasks tagged `operations-as-data`, starting with [ORB-15932]. Each section
of [2_design.md](./2_design.md) is marked **Implemented** or **Planned**.

## 1. Motivation

The cost was per operation and paid up to four times. Adding `orbit.friction.foo`
meant: a `Tool` impl in `orbit-tools` with a hand-written `ToolSchema`, a clap
`Args` struct plus an `Execute` impl in `orbit-cli` that rebuilt the same JSON by
hand, a route and an input map in `orbit-web`, a variant in
`OrbitBuiltinAction`, a dispatch arm in `orbit-core`, and an arm in the CLI's
audit-metadata match. Six of those seven edits carried no information the other
five did not already have.

The drift that follows is not hypothetical. Before the pilot, `orbit.friction.show`
and `orbit.friction.update` described the same `id` field two different ways
because two files owned the wording. After the pilot, the same kind of drift
survived one level down: the tool host classified friction `show`, `stats` and
`tags` as read-only while the CLI opened the full read-write runtime for them,
because the read/write fact was declared in four places until kernel v2 made it
one `effect` field ([2_design.md §5](./2_design.md#5-effect-derived-runtime-wiring)). Unmigrated
nouns pay the full price: `TaskAddParams` is assembled from input once in the CLI
and again in the tool handler.

## 2. Core Concepts

- **Operation** — one verb on one noun (`friction add`). The unit of declaration.
- **`OperationSpec<V>`** — the declaration: names, parameters, effect, MCP
  placement. Pure data; no transport types, no runtime handle.
- **`NounSpec<V>`** — the noun-level declaration: CLI name, tool prefix,
  `--help` summary, audit target type, CLI groups, and the operation table.
- **Verb enum (`V`)** — the noun's typed verb list (`FrictionVerb`). The join
  between the spec table and every per-surface table.
- **Registry** — a noun's `&'static [OperationSpec<V>]`, in declaration order,
  emitted with the verb enum by `operation_registry!`. Order is contract: it is
  `--help` order and MCP schema order.
- **Effect** — whether a verb reads or writes. The one behavioural fact every
  surface needs, so it lives in the spec.
- **Derived adapter** — a surface that reads the registry instead of restating
  it. Generic over `V`, so the next noun reuses it unchanged.
- **Surface table** — a per-surface exhaustive `match` on `V` for facts only that
  surface needs: the `orbit-core` handler table, the CLI renderers, MCP schema
  extensions. The placement rule is
  [The spec holds cross-surface facts; single-surface facts are an exhaustive match in that surface](./4_decisions.md#the-spec-holds-cross-surface-facts-single-surface-facts-are-an-exhaustive-match-in-that-surface).
- **Operation input** — the tool input after the spec has validated it once at
  the tool entry: aliases resolved, retired fields refused, types and required
  fields checked, and unknown fields reported (refused in a later step).

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| The kernel: noun, operation, parameter, effect and redaction vocabulary; the `OperationVerb` trait and `operation_registry!` | [crates/orbit-common/src/governance/operation.rs](../../../crates/orbit-common/src/governance/operation.rs) | [ORB-10358], [ORB-15932] |
| The friction registry (single declaration site) | [crates/orbit-common/src/governance/friction/operations.rs](../../../crates/orbit-common/src/governance/friction/operations.rs) | [ORB-10358] |
| The auto-task registry (single declaration site) | [crates/orbit-common/src/governance/auto_task/operations.rs](../../../crates/orbit-common/src/governance/auto_task/operations.rs) | [ORB-15935] |
| The drain registry (six active tool-only verbs, including `orbit.task.pull`) | [crates/orbit-common/src/governance/drain.rs](../../../crates/orbit-common/src/governance/drain.rs) | [ORB-15936] |
| The workspace-claim registry (three inactive tool-only verbs) | [crates/orbit-common/src/governance/workspace_claim.rs](../../../crates/orbit-common/src/governance/workspace_claim.rs) | [ORB-15936] |
| The task registry and task enum value tables | [crates/orbit-common/src/governance/task/operations.rs](../../../crates/orbit-common/src/governance/task/operations.rs) | [ORB-15937] |
| Task nested artifact and lock operations | [crates/orbit-common/src/governance/task/nested_operations.rs](../../../crates/orbit-common/src/governance/task/nested_operations.rs) | [ORB-15939] |
| Task review and lifecycle operations (`reconcile_review` and its CLI actions, `review_reset`, inactive `reject` and `delete`) | [crates/orbit-common/src/governance/task/lifecycle_operations.rs](../../../crates/orbit-common/src/governance/task/lifecycle_operations.rs) | [ORB-15940] |
| The workflow-run registry (five MCP-advertised tool-only verbs, including the multiplexed `orbit.workflow.auto`) | [crates/orbit-common/src/governance/workflow.rs](../../../crates/orbit-common/src/governance/workflow.rs) | [ORB-15941] |
| The search registry (root CLI query and tool-only friction listing) | [crates/orbit-common/src/governance/search.rs](../../../crates/orbit-common/src/governance/search.rs) | [ORB-15942] |
| The codebase registry (read-only `analytics` and `show`, on the CLI and MCP) | [crates/orbit-common/src/governance/codebase.rs](../../../crates/orbit-common/src/governance/codebase.rs) | [ORB-16787] |
| MCP adapter: generic `OperationTool<V>`, schema, scope and execution kind | [crates/orbit-tools/src/builtin/orbit/operation.rs](../../../crates/orbit-tools/src/builtin/orbit/operation.rs) | [ORB-10358], [ORB-15932] |
| Auto-task guarded modes (bounded list, acknowledged mint, checked toggle), keyed on `AutoTaskVerb` | [crates/orbit-tools/src/builtin/orbit/auto_task.rs](../../../crates/orbit-tools/src/builtin/orbit/auto_task.rs) | [ORB-15935] |
| Task guarded modes (bounded `show`/`list`, versioned `show` snapshot), keyed on `TaskVerb` | [crates/orbit-tools/src/builtin/orbit/task/operation.rs](../../../crates/orbit-tools/src/builtin/orbit/task/operation.rs) | [ORB-15937] |
| Workflow guarded modes (managed-run refusals, bounded run reads, auto `status`), keyed on `WorkflowVerb` | [crates/orbit-tools/src/builtin/orbit/workflow.rs](../../../crates/orbit-tools/src/builtin/orbit/workflow.rs) | [ORB-15941] |
| CLI adapter: generic clap impls for `Invocation<V>`, tool input | [crates/orbit-cli/src/command/operation/args.rs](../../../crates/orbit-cli/src/command/operation/args.rs) | [ORB-10358], [ORB-15932] |
| Friction CLI surface tables (request shaping, renderers) | [crates/orbit-cli/src/command/friction.rs](../../../crates/orbit-cli/src/command/friction.rs) | [ORB-10358], [ORB-15932] |
| Auto-task CLI surface tables (flag composition, renderers) | [crates/orbit-cli/src/command/auto_task/](../../../crates/orbit-cli/src/command/auto_task/) | [ORB-15935] |
| Task CLI surface tables (request shaping, renderers) | [crates/orbit-cli/src/command/task/operation.rs](../../../crates/orbit-cli/src/command/task/operation.rs) | [ORB-15937] |
| CLI audit metadata and runtime need (`registry_operation`; `task_operation` adds task-owner binding and `lint`'s read-only runtime) | [crates/orbit-cli/src/command/operation/spec.rs](../../../crates/orbit-cli/src/command/operation/spec.rs) | [ORB-15932], [ORB-15937] |
| Web handlers over registry field names | [crates/orbit-web/src/api/frictions.rs](../../../crates/orbit-web/src/api/frictions.rs) | [ORB-10358] |
| Handler table, keyed on `FrictionVerb` | [crates/orbit-core/src/adapter/tool_host/friction_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/friction_tools.rs) | [ORB-10358] |
| Handler table, keyed on `AutoTaskVerb` | [crates/orbit-core/src/adapter/tool_host/auto_task_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/auto_task_tools.rs) | [ORB-15935] |
| Handler table, keyed on `DrainVerb` | [crates/orbit-core/src/adapter/tool_host/drain_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/drain_tools.rs) | [ORB-15936] |
| Handler table, keyed on `WorkspaceClaimVerb` | [crates/orbit-core/src/adapter/tool_host/workspace_claim_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/workspace_claim_tools.rs) | [ORB-15936] |
| Handler table, keyed on `TaskVerb` | [crates/orbit-core/src/adapter/tool_host/task_operation_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/task_operation_tools.rs) | [ORB-15937] |
| Handler table, keyed on `WorkflowVerb` | [crates/orbit-core/src/adapter/tool_host/workflow_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/workflow_tools.rs) | [ORB-15941] |
| ID routing, keyed by tool name (host prefix routing; owner-workspace binding is `ID_RESOLVED_WORKSPACE_TOOLS` in the CLI's MCP server) | [crates/orbit-tools/src/mcp_rules.rs](../../../crates/orbit-tools/src/mcp_rules.rs) | [ORB-15937] |
| Redaction policy, derived from each parameter's `redact` class | [crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs](../../../crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs) | [ORB-15932] |
| Claimed-worker routing, keyed on the migrated nouns' verb enums | [crates/orbit-core/src/adapter/tool_host/worker_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/worker_tools.rs) | [ORB-15932], [ORB-15935] |
| Shared tool-input parsing and unknown-field refusal | [crates/orbit-common/src/protocol/tool_input.rs](../../../crates/orbit-common/src/protocol/tool_input.rs) | — |
| Migration cookbook | [references/cookbook.md](./references/cookbook.md) | [ORB-10358] |

## Task References

- [ORB-10358] — piloted bearing 1 of [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry)
  on the friction noun: built the operations-as-data kernel, the friction
  registry, and the derived CLI, MCP, dashboard, and runtime adapters.
- [ORB-15932] — first task of the accepted kernel v2 migration: built phase 0a's
  structure and ported friction onto it.
- [ORB-15933] — phase 0b: input validation at the tool entry in warn mode, with
  declared aliases, retired fields and `at_least_one_of`.
- [ORB-15935] — phase 1: migrated the auto-task noun, adding the switch, enum,
  object and CLI-only parameter kinds.
- [ORB-15936] — phase 2: migrated the drain and workspace-claim tool-only nouns,
  preserving active/inactive registration, schemas and governance.
- [ORB-15937] — phase 3a: migrated the task read verbs and kept ID routing keyed
  by tool name.
- [ORB-15938] — phase 3b: migrated task `add` and `update`, adding the array
  parameter kind, refuse-mode unknown fields and the schema extension table.
- [ORB-15941] — phase 4: migrated the workflow-run tools as a tool-only noun
  and settled that a multiplexed tool's action is an enum parameter.
- [ORB-16787] — declared the read-only codebase analytics noun in the
  registry, with its CLI and MCP surfaces derived from it.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
