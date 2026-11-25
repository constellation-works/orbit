---
type: context
summary: Architecture map for orbit-types: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-types]
---

# orbit-types

## What it owns

Shared serde contracts, pure constructors and narrow domain errors. It does no I/O of any kind. Its serde shapes are persisted contracts.

## Main modules and types

`identity` (`OrbitId`), `task` (`Task`), `workflow`, `policy`, `tool`, `workspace`, `record`, `resource`, `plugin`, `telemetry`, `desktop`. See ``crates/orbit-types/src/lib.rs``.

## Dependencies

- Depends on (internal, production): —.
- Depended on by (internal, production): `orbit-agent`, `orbit-auth`, `orbit-automation`, `orbit-cli`, `orbit-cmd`, `orbit-common`, `orbit-config`, `orbit-core`, `orbit-engine`, `orbit-exec`, `orbit-mcp`, `orbit-policy`, `orbit-registry`, `orbit-runtime`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-types/src/lib.rs`; start at `crates/orbit-types/src/task/model/task.rs` (`Task`).

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Domain modules

The domain modules are `desktop`, `identity`, `plugin`, `policy`, `record`, `resource`, `task`, `telemetry`, `timestamp`, `tool`, `workflow` and `workspace`. `desktop` = guarded desktop task contracts; `plugin` = plugin manifests, pins, and namespace rules; `record` = durable authored artifacts; `telemetry` = measurement of runs. `timestamp` owns the shared legacy UTC wire format for policy and executor records.

## Ownership by module

- `desktop`: Guarded desktop task contracts where caller JSON never supplies actor authority; imports `task` within `orbit-types` only (I/O-free).
- `identity`: Stable identity contracts (`OrbitId`, `ActorIdentity`, machine/workspace/agent/human crew identifiers); imports `task` and `workflow` within `orbit-types` only (I/O-free).
- `plugin`: Plugin standard contracts (`plugin.yaml` v2 manifest, `.orbit/plugins.yaml` pin file, install record, and namespace rules); imports no sibling modules (I/O-free).
- `policy`: Filesystem policy profile, glob matching, role, and policy-definition contracts; imports `resource` and `timestamp` within `orbit-types` only (I/O-free).
- `record`: Durable record contracts (`OrbitEvent`, `Audit`, friction, notice, evolution, and crew discovery); imports `identity`, `telemetry`, and `workflow` within `orbit-types` only (I/O-free).
- `resource`: Generic resource envelope, header, and executor/policy resource contracts; imports `policy`, `task`, `timestamp`, and `workflow` within `orbit-types` only (I/O-free).
- `task`: Task model, lifecycle status, plan, decision, artifact, readiness, and regression contracts; imports `identity` and `workflow` within `orbit-types` only (I/O-free).
- `telemetry`: Audit event, actor attribution, invocation trace, pricing, and provider-limit telemetry contracts; imports `identity`, `plugin`, `record`, and `tool` within `orbit-types` only (I/O-free).
- `timestamp`: UTC timestamp contracts for persisted records using the legacy date format; imports no sibling modules (I/O-free).
- `tool`: Tool schema, family, invocation, MCP toolset/rules, and application-consent contracts; imports `identity` and `task` within `orbit-types` only (I/O-free).
- `workflow`: Activity-job, automation, auto-task, routine, experiment, and delivery workflow contracts; imports sibling `orbit-types` modules only (I/O-free).
- `workspace`: Workspace registry, checkout role, path layout, and task-publication binding contracts; imports `identity` and `plugin` within `orbit-types` only (I/O-free).
