---
type: context
summary: Architecture map for orbit-cli: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-cli]
---

# orbit-cli

## What it owns

Clap entry point (`orbit` binary) and client configuration. Assembles MCP, Registry, Web, Core; accepting machine resolves its own state and dispatches through Core.

## Main modules and types

`main.rs` (`fn main`), `command`, `entry`, `output`, `parse`, `help`, `plugin_cli`, `audit_middleware`. See ``crates/orbit-cli/src/main.rs``.

`command/tool/run.rs` owns `ToolRunShape`, the prepared operation name and
output policy shared by local execution, host routing and workspace-less notice
aggregation. `ToolRunArgs` requests write sidecars before workspace binding,
then builds the shape from prepared input and applies the compact list hint to
the execution input. Routed calls alone carry the declared model to the owner.

## Dependencies

- Depends on (internal, production): `orbit-cmd`, `orbit-common`, `orbit-config`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-types`, `orbit-web`.
- Depended on by: — (binary).

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-cli/src/main.rs`.

Design intent: [docs/design/terminal-interface/](../../docs/design/terminal-interface/) — this page describes the code as it is; the design folder holds intent.

## Part-migrated nouns

`lease` and `agents` flatten `Operation(Invocation<V>)`, and `task` flattens `Operation(TaskInvocation)`, where `TaskInvocation` is `Registry(Invocation<TaskVerb>)` plus the hand-wired `locks contention` command. A `TaskVerb` with an empty `cli_path` (`BlockTriage`, `Reject`, `Delete`) gets no `orbit task` subcommand.

## Ownership by module

- `audit_middleware`: RAII SQLite CLI-invocation audit guard (`AuditGuard`); imports `command`, `orbit-common`, and `orbit-core`.
- `command`: Top-level CLI subcommand argument definitions and dispatch handlers; imports `output`, `parse`, `plugin_cli`, and workspace domain/composition crates.
- `entry`: Process entry for the `orbit` binary that parses argv, classifies commands, admits a generation, dispatches, and reports results; imports sibling CLI modules and `orbit-cmd`, `orbit-common`, `orbit-core`, `orbit-mcp`, `orbit-types`.
- `help`: Compact inherited options and shared grouped command-list help templates; imports `clap` only.
- `output`: CLI output formatting, color, table, JSON, pipe, and sink rendering; imports `orbit-cmd`, `orbit-common`, and `orbit-core`.
- `parse`: Shared CLI argument parsers (CSV lists, positive limits, durations, and timestamps); imports `orbit-common` and `orbit-core`.
- `plugin_cli`: Dynamic `orbit <ns> <verb>` command groups contributed by installed plugins; imports `command`, `orbit-common`, `orbit-core`, and `orbit-types`.
- `root_check`: Early validation of an explicit Orbit root (`--root` / `ORBIT_ROOT`) before generation admission; imports `orbit-core` only.
- `usage_error`: Usage errors reported in the invocation's output mode before clap produces matches; imports `output` and `orbit-common`.
