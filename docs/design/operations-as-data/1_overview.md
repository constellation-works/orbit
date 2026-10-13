---
title: Operations as Data — Overview
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
status: Draft
feature: operations-as-data
doc_role: overview
type: design
summary: Declaring each verb of a noun once as data so CLI, MCP, dashboard, and runtime handlers are derived adapters instead of hand-copied layers; friction is migrated, kernel v2 and a planned noun-by-noun migration are proposed.
tags: [operations-as-data, architecture, adr-0209]
paths: ["crates/orbit-common/src/governance/operation.rs", "crates/orbit-common/src/governance/friction/**", "crates/orbit-tools/src/builtin/orbit/operation.rs", "crates/orbit-cli/src/command/operation/args.rs"]
related_features: [operations-as-data, orbit-core]
related_artifacts: [ORB-10358]
---

# Operations as Data — Overview

Orbit exposes the same underlying operations through four surfaces: the CLI, MCP,
the web dashboard (`orbit-web`), and the in-runtime tool host. Historically each
surface restated every verb by hand — its name, its parameters, its help text, its
field-to-JSON mapping — so a noun with seven verbs was written out four times and
drifted four ways. **Operations as data** replaces that with one declaration per
verb: a `const` spec that every surface derives its wiring from. This is bearing 1
of [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry).

**Status.** The friction noun was migrated end to end in [ORB-10358] and is still
the only migrated noun. The rest of the tool surface (task, auto-task, workflow
run, drain, workspace claim, and the single-verb tools) is hand-wired. A
2026-10-12 review found that the friction kernel declares a verb's *names* once
but still restates its *behaviour* — read/write class, redaction, accepted
aliases, cross-field rules — on several surfaces, and that it cannot yet express
what the task and auto-task commands need. This folder is `Draft` while the
revised design (kernel v2) and a planned migration replace the touch-it-move-it
ratchet, which migrated no noun in the eleven weeks it governed. Each section of
[2_design.md](./2_design.md) is marked **Implemented** or **Proposed**.

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
survives one level down: the tool host classifies friction `show`, `stats` and
`tags` as read-only while the CLI opens the full read-write runtime for them,
because the read/write fact is still declared in four places
([2_design.md §5](./2_design.md#5-effect-derived-runtime-wiring)). Unmigrated
nouns pay the full price: `TaskAddParams` is assembled from input once in the CLI
and again in the tool handler.

## 2. Core Concepts

- **Operation** — one verb on one noun (`friction add`). The unit of declaration.
- **`OperationSpec<V>`** — the declaration: names, parameters, effect, MCP
  placement. Pure data; no transport types, no runtime handle.
- **`NounSpec<V>`** *(proposed)* — the noun-level declaration: CLI name, tool
  prefix, `--help` summary, audit target type, and the operation table.
- **Verb enum (`V`)** — the noun's typed verb list (`FrictionVerb`). The join
  between the spec table and every per-surface table.
- **Registry** — a noun's `&'static [OperationSpec<V>]`, in declaration order.
  Order is contract: it is `--help` order and MCP schema order.
- **Effect** *(proposed)* — whether a verb reads or writes. The one behavioural
  fact every surface needs, so it lives in the spec.
- **Derived adapter** — a surface that reads the registry instead of restating
  it. Generic over `V`, so the next noun reuses it unchanged.
- **Surface table** — a per-surface exhaustive `match` on `V` for facts only that
  surface needs: the `orbit-core` handler table, the CLI renderers, MCP schema
  extensions. The placement rule is
  [The spec holds cross-surface facts; single-surface facts are an exhaustive match in that surface](./4_decisions.md#the-spec-holds-cross-surface-facts-single-surface-facts-are-an-exhaustive-match-in-that-surface).
- **Operation input** *(proposed)* — the tool input after the spec has
  validated it once at the tool entry: aliases resolved, unknown fields refused,
  types and required fields checked.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| The kernel: spec, parameter, and placement vocabulary | [crates/orbit-common/src/governance/operation.rs](../../../crates/orbit-common/src/governance/operation.rs) | [ORB-10358] |
| The friction registry (single declaration site) | [crates/orbit-common/src/governance/friction/operations.rs](../../../crates/orbit-common/src/governance/friction/operations.rs) | [ORB-10358] |
| MCP adapter: spec → `ToolSchema` + optional workspace scope | [crates/orbit-tools/src/builtin/orbit/operation.rs](../../../crates/orbit-tools/src/builtin/orbit/operation.rs) | [ORB-10358] |
| Friction MCP tools, derived | [crates/orbit-tools/src/builtin/orbit/friction/mod.rs](../../../crates/orbit-tools/src/builtin/orbit/friction/mod.rs) | [ORB-10358] |
| CLI adapter: spec → `clap::Command` + tool input | [crates/orbit-cli/src/command/operation/args.rs](../../../crates/orbit-cli/src/command/operation/args.rs) | [ORB-10358] |
| Friction CLI, derived (renderers only) | [crates/orbit-cli/src/command/friction.rs](../../../crates/orbit-cli/src/command/friction.rs) | [ORB-10358] |
| CLI audit metadata and runtime need | [crates/orbit-cli/src/command/operation/registry.rs](../../../crates/orbit-cli/src/command/operation/registry.rs) | [ORB-10358] |
| Web handlers over registry field names | [crates/orbit-web/src/api/frictions.rs](../../../crates/orbit-web/src/api/frictions.rs) | [ORB-10358] |
| Handler table, keyed on `FrictionVerb` | [crates/orbit-core/src/adapter/tool_host/friction_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/friction_tools.rs) | [ORB-10358] |
| Per-verb redaction policy (to derive from the spec) | [crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs](../../../crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs) | — |
| Shared tool-input parsing and unknown-field refusal | [crates/orbit-common/src/protocol/tool_input.rs](../../../crates/orbit-common/src/protocol/tool_input.rs) | — |
| Migration cookbook | [references/cookbook.md](./references/cookbook.md) | [ORB-10358] |

## Task References

- [ORB-10358] — piloted bearing 1 of [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry)
  on the friction noun: built the operations-as-data kernel, the friction
  registry, and the derived CLI, MCP, dashboard, and runtime adapters.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
