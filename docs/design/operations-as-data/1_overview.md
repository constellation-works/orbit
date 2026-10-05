---
title: Operations as Data — Overview
owner: claude
last_updated: 2026-07-26
last_validated: 2026-09-23
status: Accepted
feature: operations-as-data
doc_role: overview
type: design
summary: Declaring each verb of a noun once as data so CLI, MCP, dashboard, and runtime handlers are derived adapters instead of four hand-copied layers.
tags: [operations-as-data, architecture, adr-0209]
paths: ["crates/orbit-common/src/governance/operation.rs", "crates/orbit-common/src/governance/friction/**", "crates/orbit-tools/src/builtin/orbit/operation.rs", "crates/orbit-cli/src/command/operation_args.rs"]
related_features: [operations-as-data, orbit-core]
related_artifacts: [ORB-10358]
---

# Operations as Data — Overview

Orbit exposes the same underlying operations through four surfaces: the CLI, MCP,
the web dashboard (`orbit-web`), and the in-runtime tool host. Historically each surface
restated every verb by hand — its name, its parameters, its help text, its
field-to-JSON mapping — so a noun with seven verbs was written out four times and
drifted four ways. **Operations as data** replaces that with one declaration per
verb: a `const` spec listing the wire name, the parameters, how each parameter
binds to the command line, whether MCP advertises it, and how the CLI renders the
result. Every surface then derives its wiring from that spec. This is [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry)
bearing 1, piloted end to end on the **friction** noun in [ORB-10358].

## 1. Motivation

The cost was per-operation and paid up to four times. Adding `orbit.friction.foo`
meant: a `Tool` impl in `orbit-tools` with a hand-written `ToolSchema`, a clap
`Args` struct plus an `Execute` impl in `orbit-cli` that rebuilt the same JSON by
hand, a route and an input-map in `orbit-web`, a variant in
`OrbitBuiltinAction`, a dispatch arm in `orbit-core`, and an arm in the CLI's
audit-metadata match. Six of those seven edits carried no information the other
five did not already have.

The drift that follows is not hypothetical: before this pilot, `orbit.friction.show`
and `orbit.friction.update` described the same `id` field two different ways
because two different files owned the wording, and each surface trimmed blank
optional values on its own schedule. Nothing was wrong enough to break, which is
exactly why it accumulated.

## 2. Core Concepts

- **Operation** — one verb on one noun (`friction add`). The unit of declaration.
- **`OperationSpec<V>`** — the declaration itself: names, parameters, MCP
  exposure, CLI rendering. Pure data, no transport types, no runtime handle.
- **Verb enum (`V`)** — the noun's typed verb list (`FrictionVerb`). The join
  between the spec table and the handler table.
- **Registry** — a noun's `&'static [OperationSpec<V>]`, in declaration order.
  Order is contract: it is `--help` order and MCP schema order.
- **Derived adapter** — a surface that reads the registry instead of restating
  it. Generic over `V`, so the next noun reuses it unchanged.
- **Handler table** — the `orbit-core` half: one exhaustive `match` on `V`.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| The kernel: spec, parameter, and exposure vocabulary | [crates/orbit-common/src/governance/operation.rs](../../../crates/orbit-common/src/governance/operation.rs) | [ORB-10358] |
| The friction registry (single declaration site) | [crates/orbit-common/src/governance/friction/operations.rs](../../../crates/orbit-common/src/governance/friction/operations.rs) | [ORB-10358] |
| MCP adapter: spec → `ToolSchema` + optional workspace scope | [crates/orbit-tools/src/builtin/orbit/operation.rs](../../../crates/orbit-tools/src/builtin/orbit/operation.rs) | [ORB-10358] |
| Friction MCP tools, derived | [crates/orbit-tools/src/builtin/orbit/friction/mod.rs](../../../crates/orbit-tools/src/builtin/orbit/friction/mod.rs) | [ORB-10358] |
| CLI adapter: spec → `clap::Command` + tool input | [crates/orbit-cli/src/command/operation_args.rs](../../../crates/orbit-cli/src/command/operation_args.rs) | [ORB-10358] |
| Friction CLI, derived (renderers only) | [crates/orbit-cli/src/command/friction.rs](../../../crates/orbit-cli/src/command/friction.rs) | [ORB-10358] |
| Web handlers over registry field names | [crates/orbit-web/src/api/frictions.rs](../../../crates/orbit-web/src/api/frictions.rs) | [ORB-10358] |
| Handler table, keyed on `FrictionVerb` | [crates/orbit-core/src/adapter/tool_host/friction_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/friction_tools.rs) | [ORB-10358] |
| Migration cookbook | [references/cookbook.md](references/cookbook.md) | [ORB-10358] |

## Task References

- [ORB-10358] — piloted [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry) bearing 1 on the friction noun: built the
  operations-as-data kernel, the friction registry, and the derived CLI, MCP,
  dashboard, and runtime adapters.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
