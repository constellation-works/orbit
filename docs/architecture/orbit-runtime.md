---
type: context
summary: Architecture map for orbit-runtime: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-runtime]
---

# orbit-runtime

## What it owns

Runtime kernel: `RuntimeKernel`, stores, engine handles, event bus, audit, claims, reservations, plugin host, workspace binding, host probes, process/tool execution. Never depends on `orbit-core`; finishes use-cases via `ApplicationPort`.

## Main modules and types

`orbit_runtime` (`RuntimeKernel`), `builder`, `engine`, `audit`, `event_bus`, `plugin`, `workspace`, `application_port` (`ApplicationPort`), `context`, `cost`. See ``crates/orbit-runtime/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-config`, `orbit-engine`, `orbit-exec`, `orbit-policy`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-core`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-runtime/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `activity_catalog`: Activity catalog resolving `target: activity:<name>` definitions; imports `assets` and `orbit-engine`.
- `application_consent`: Application consent validation, host authority, and constrained call admission; imports `orbit_runtime`, `plugin`, `orbit-common`, `orbit-store`, `orbit-tools`, and `orbit-types`.
- `application_port`: Callback port (`ApplicationPort`) the kernel invokes into Core use cases without depending on `orbit-core`; imports `orbit_runtime`, `orbit-common`, `orbit-engine`, `orbit-store`, and `orbit-types`.
- `assets`: Embedded built-in activity definitions available to runtime and bootstrap layers; imports `orbit-types` only.
- `audit`: Runtime-owned audit trail reads, projections, CLI log profiles, and system audit seams; imports `orbit_runtime`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `authorization`: Runtime half of the capability chokepoint resolving session grants and audit records; imports `orbit_runtime`, `run_input`, `tool_exec`, `orbit-common`, `orbit-store`, `orbit-tools`, and `orbit-types`.
- `builder`: `RuntimeKernel` assembly wiring stores, policy, tools, plugins, and workspace bindings; imports `context`, `plugin`, `task`, and lower workspace crates.
- `command_exec`: Claim-gated remote command execution entry point (`orbit.command.exec`); imports `orbit_runtime`, `orbit-common`, and `orbit-types`.
- `config_path`: Selection of the effective `config.toml` path from explicit runtime roots; imports `orbit-common` only.
- `context`: Actor identity, shared runtime state, stores, execution assets, and admitted settings; imports `host_signal`, `plugin`, `task`, and lower workspace crates.
- `cost`: Managed-execution cost and token accounting plus shared dashboard time windows; imports `chrono`, `orbit-common`, `orbit-types`, and `serde` only.
- `cwd`: Shared working-directory confinement for operator host tools; imports `orbit_runtime` and `orbit-common`.
- `engine`: Engine execution helpers, crew projections, environment host, and invocation metrics on `RuntimeKernel`; imports `orbit_runtime`, `cost`, `run_input`, and lower workspace crates.
- `event_bus`: Bounded in-process session `EventLog` ring buffer; imports `orbit-types` only.
- `friction`: Runtime-owned access to the workspace-partitioned friction repository; imports `orbit_runtime`, `workspace`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `git_sandbox`: Host-owned Git write boundaries for macOS and Linux provider sandboxes; imports `orbit-common` and `orbit-types` only.
- `host_resource`: Host resource probes, shared pressure history, learned profiles, and load forecasts; imports `run_input`, `orbit-common`, `orbit-config`, `orbit-store`, and `orbit-types`.
- `host_signal`: Host lifecycle signal probes (such as scheduled shutdown/reboot) gating new admissions; imports `std`, `chrono`, and `serde` only.
- `mutation`: `RuntimeKernel` mutation wrappers that append session `OrbitEvent` entries; imports `orbit_runtime`, `orbit-common`, and `orbit-types`.
- `orbit_runtime`: `RuntimeKernel` core type and method groups for construction, identity, host probes, persistence, and configuration; imports sibling runtime modules and lower workspace crates.
- `paths`: Path normalization, tilde/token expansion, and Git-root path helpers; imports `orbit-common` only.
- `plugin`: Host plugin contributions, recorded grants, install layout, and runtime load pass; imports `assets`, `authorization`, `config_path`, `recovery_authority`, and lower workspace crates.
- `recovery_authority`: Host-only authority for rebase-recovery evidence; imports `git_sandbox`, `orbit-common`, `orbit-engine`, `orbit-store`, and `orbit-types`.
- `recovery_decision`: Durable step-recovery decisions (`step_failure_recovery`); imports `orbit-common`, `orbit-engine`, and `orbit-types`.
- `resolve`: Global and workspace Orbit root discovery (`resolve_global_root`, workspace root lookup); imports `paths`, `run_input`, `orbit-common`, and `orbit-config`.
- `run_input`: Managed-run environment marker parsing (`ORBIT_MANAGED_RUN_CONTEXT`, run/task IDs); imports `std`, `serde_json`, and `orbit-common` only.
- `scripts`: Host-local script grant consent keyed by workspace script content digest; imports `orbit-common`, `orbit-engine`, `orbit-exec`, `orbit-tools`, and `orbit-types`.
- `task`: Task reservation, PR forge closure, and run-coupling behavior on `RuntimeKernel`; imports `orbit_runtime`, `audit`, `run_input`, and lower workspace crates.
- `tool_exec`: Registered tool execution, capability enforcement, and redaction on `RuntimeKernel`; imports `orbit_runtime`, `paths`, `run_input`, `orbit-common`, `orbit-engine`, `orbit-tools`, and `orbit-types`.
- `upgrade_handover`: Detached pipeline worker drain participation during Orbit binary upgrades; imports `orbit_runtime`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `worker_coordination`: Managed-worker owner-coordinator RPC retries and test wait seams on `RuntimeKernel`; imports `orbit_runtime`, `orbit-common`, `orbit-tools`, and `orbit-types`.
- `workspace`: Registry-neutral workspace binding, cross-workspace catalog seam, and exclusive workspace claims; imports `orbit_runtime`, `audit`, `task`, `orbit-common`, `orbit-store`, and `orbit-types`.
