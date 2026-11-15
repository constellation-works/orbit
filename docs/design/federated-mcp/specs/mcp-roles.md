---
type: design
summary: "Spec: MCP roles — every tool in one toolset, roles as named toolset sets, --role and machine.mcp.role, refusal, remote and federated forwarding, size budgets"
last_updated: 2026-11-05
last_validated: 2026-11-14
title: Spec — MCP roles and toolsets
owner: claude
status: Implemented
feature: federated-mcp
tags: [federated-mcp, mcp, spec, resident-orchestrator]
related_features: [federated-mcp, mcp-bridge, resident-orchestrator, fleet-notices]
related_artifacts: [ORB-21263, ORB-21245, ORB-21211, ORB-20234]
---

# Spec: MCP roles and toolsets

An `orbit mcp serve` server lists only the toolsets it enables. A **role**
is a named set of toolsets for one kind of session (operator console,
manager, resident cycle, managed agent, read-only viewer), so each session
carries only the tools its job needs. The decision is recorded in
[4_decisions.md](../4_decisions.md#an-mcp-role-limits-the-surface-never-the-authority).
The role grouping was implemented by [ORB-21263], extending the opt-in
toolset surface introduced by [ORB-20234].

## Toolsets

- Every tool belongs to exactly one toolset (`McpToolset`,
  `crates/orbit-types/src/tool/mcp_toolset.rs`). `orbit tool list` and
  `orbit tool show` report it as `mcp_toolset`.
- Builtin MCP tools are placed by one table,
  `crates/orbit-tools/src/mcp/toolsets.rs::builtin_mcp_toolset`. A builtin
  MCP tool missing from it belongs to no toolset, and the registry refuses
  to build MCP definitions (`McpToolDefinitionError::UnclassifiedToolset`),
  so no tool ships unplaced.
- Plugin tools are in `plugins`. A plugin manifest does not choose its
  toolset yet; a manifest field would be a persisted contract and is left
  for a later change.
- Tools with no MCP scope (`proc.spawn`, `github.*`, the drain tools,
  external tools) are in `local`. No server lists them, whatever is enabled.
- `core` is always on and holds only workspace discovery
  (`orbit.workspace.list` and the federated list). No registry tool is in
  it.
- The default profile combines `core` with `tasks-read`, `tasks-write` and
  `search`. The name `default` selects those toolsets. Other toolsets are
  opt-in, or included in a named role.

## Roles

Roles are defined in one place, `McpRole::toolsets`:

| Role | Toolsets beside `core` |
|---|---|
| `operator` | `tasks-read`, `tasks-write`, `task-repair`, `decisions`, `runs`, `dispatch`, `automation`, `codebase`, `search`, `ui`, `exec`, `plugins` |
| `manager` | `tasks-read`, `runs`, `decisions`, `notice`, `resident`, `search` |
| `resident` | `tasks-read`, `tasks-write`, `runs`, `dispatch`, `decisions`, `notice`, `resident`, `search` |
| `agent` | `tasks-read`, `tasks-write`, `decisions`, `codebase`, `search`, `ui`, `leases`, `notice` |
| `reader` | `tasks-read`, `runs`, `codebase`, `search` |

`agent` follows the managed activity baseline allowlist; activities keep
their own allowlists, which still decide what a managed run may call. The
`resident` toolset (`orbit.resident.grant`, `show` and `list` from [ORB-21242];
`relay` and `control` from [ORB-21245]) is in `manager` and `resident`.
The roles, not another add-on toolset, are how those surfaces are served.

## Selection

1. The role is `--role`, else `operator` when `--operator` is set, else the global `machine.mcp.role`, else none.
2. The base is the role's toolsets plus `core`, or with no role `core` plus
   the default tier.
3. `--toolset`, else `machine.mcp.toolsets`, adds to the base (union).
4. A managed run uses the default profile plus explicitly allowed activity tools and its admitted required and granted tools, ignoring the host selection.

With no role the server advertises the core profile, recorded in
`crates/orbit-cli/tests/snapshots/mcp_tools_list.json`. An Orbit SSH route (`--remote-caller-machine-id`) that names
neither `--role` nor `--toolset` serves every toolset, because its caller
filters. `orbit mcp listen` takes the same `--role` and `--toolset`.

## Enforcement

- A role limits what is listed and dispatched over MCP, nothing else. A call
  to a tool outside the selection is refused `toolset_not_enabled`; the
  payload names the `tool`, its `toolset`, the serving `role` (or null), and
  `enable.flag`, `enable.config_key` and `enable.roles` (every role that
  includes the toolset).
- A role never grants authority. Operator-only tools stay hidden from and
  refused to agent sessions under every role, `operator` included; the
  capability check runs independently of the toolset filter.
- `orbit tool run`, activities and plugin callbacks are unaffected.

## Remote and federated

- `--mode remote` asks the destination for the same surface:
  `--role <role>` when a role is set, and `--toolset <extra>` for the
  toolsets beyond the role (or beyond the default tier without one). With no
  role the argv is the `--toolset` list it always was, so a destination that
  predates roles is asked for nothing new.
- `--mode federated` applies the selection at the mux, so every destination
  row, local and remote, is filtered by the same role; each destination's
  own SSH session serves every toolset.

## Measurement

- `orbit tool list --role <role>` lists the role's tools and reports its
  tool count and approximate token size (serialized name, description and
  input schema bytes over four) over the canonical builtin and discovery
  definitions. Plugin tools vary per workspace and are not counted.
- `crates/orbit-mcp/tests/boundary/mcp_role_budgets.json` records each
  role's budget (and the no-role default surface's). The `role_surface_budget`
  boundary test fails when a role exceeds it; raising a budget is a reviewed
  change that says why the role needs the extra surface.

## Task References

- [ORB-21263] — roles, the toolset split, `--role` and `machine.mcp.role`
- [ORB-21242] — added resident tools that the role grouping serves
- [ORB-20234] — opt-in toolsets and the `toolset_not_enabled` refusal
- [ORB-21245] — the resident toolset that joins the manager and resident roles
- [ORB-21211] — the `notice` toolset

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
