---
type: context
summary: Architecture map for orbit-automation: what it owns, main modules, dependencies and entry points.
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
