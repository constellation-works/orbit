---
type: context
summary: Architecture map for orbit-exec: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-exec]
---

# orbit-exec

## What it owns

Sandboxes and runner entry points for commands run under an `FsProfile`; process supervision, capture, timeouts.

## Main modules and types

`runner` (`run_process`, `ExecRequest`, `ExecutionResult`), `sandbox` (`Sandbox`), `browser_kit`, `linux_sandbox`, `macos_sandbox`, `build_sandbox`, `supervision`. See ``crates/orbit-exec/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-core`, `orbit-engine`, `orbit-runtime`, `orbit-tools`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-exec/src/lib.rs`.

Design intent: [docs/design/policy-sandbox/](../../docs/design/policy-sandbox/) — this page describes the code as it is; the design folder holds intent.
