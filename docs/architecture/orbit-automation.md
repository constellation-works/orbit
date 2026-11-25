---
type: context
summary: Architecture map for orbit-automation: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-automation]
---

# orbit-automation

## What it owns

Scheduling domain for routines and auto-tasks: definition validation, due evaluation, overlap/retry, coverage acceptance, state-triggered consumers, condition triggers (CEL), review coverage rules. No loop/store of its own.

## Main modules and types

`routines`, `auto_tasks`, `condition`, `delivery`, `review`, `members`, `checkpoint`. See ``crates/orbit-automation/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-clock`, `orbit-common`, `orbit-store`, `orbit-types`.
- Depended on by: `orbit-core`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-automation/src/lib.rs`.

Design intent: [docs/design/auto-tasks/](../../docs/design/auto-tasks/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `auto_tasks`: Auto-task definition loading and scheduling policy; imports `condition`, `definition_directory`, `routines`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `checkpoint`: One generation-fenced checkpoint and receipt projection for all consumers; imports `orbit-store` and `orbit-types` only.
- `condition`: Condition triggers, the CEL environment for `task` and `workspace` scopes, load-time validation, and pure evaluation; imports `error`, `delivery`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `definition_directory`: Safe resolution of routine and auto-task definition directories under a workspace root; imports `std` only.
- `delivery`: One bounded delivery evaluator shared by task and job consumers; imports `error`, `checkpoint`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `error`: Typed failures (`AutomationError`, `DeferredReason`) and `OrbitError` translation for deterministic automation rules; imports `orbit-common` only.
- `members`: State consumers sharing the sweep, generation-fenced store, and receipt path; imports `error`, `checkpoint`, `delivery`, `orbit-common`, `orbit-store`, and `orbit-types`.
- `review`: Shared before-PR review coverage rules; imports `error`, `delivery`, `orbit-common`, and `orbit-types`.
- `routines`: Routine discovery and deterministic sweep policy; imports `auto_tasks`, `condition`, `definition_directory`, `orbit-clock`, `orbit-common`, `orbit-store`, and `orbit-types`.
