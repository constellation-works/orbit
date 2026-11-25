---
type: context
summary: Architecture map for orbit-policy: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-policy]
---

# orbit-policy

## What it owns

Filesystem-profile policy: resolves `FsProfile` and evaluates deny-read/deny-modify rules.

## Main modules and types

`engine` (`PolicyEngine` and `FsPolicyEvaluation` in `crates/orbit-policy/src/engine.rs`), `evaluator`. See ``crates/orbit-policy/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-runtime`, `orbit-tools`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-policy/src/lib.rs`.

Design intent: [docs/design/policy-sandbox/](../../docs/design/policy-sandbox/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `engine`: `PolicyEngine` facade and symlink-resolved filesystem policy checks (`FsPolicyEvaluation`); imports `evaluator`, `orbit-common`, and `orbit-types`.
- `evaluator`: Pure policy-definition path evaluator invoked by `engine`; imports `orbit-common` and `orbit-types` only.
