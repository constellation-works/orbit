---
type: context
summary: Architecture map for orbit-exec: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-exec]
---

# orbit-exec

## What it owns

Sandboxes and runner entry points for commands run under an `FsProfile`; process supervision, capture, timeouts.

## Main modules and types

`runner` (`run_process`, `ExecRequest`), `result` (re-exports `ExecutionResult` from `orbit-types`), `sandbox` (`Sandbox`), `browser_kit`, `linux_sandbox`, `macos_sandbox`, `build_sandbox`, `supervision`. See ``crates/orbit-exec/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-core`, `orbit-engine`, `orbit-runtime`, `orbit-tools`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-exec/src/lib.rs`.

Design intent: [docs/design/policy-sandbox/](../../docs/design/policy-sandbox/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `browser_kit`: Host headless browser kit layout, state probes, and worker environment variables; imports `orbit-common` and `serde` only.
- `build_sandbox`: Install-time plugin build sandbox profile (`spec.build`); imports `credential_paths`, `linux_landlock`, `linux_sandbox`, `macos_sandbox`, `path_identity`, `supervision`, `orbit-common`, and `orbit-types`.
- `credential_paths`: Canonical list of host credential locations denied to confined worker children across platforms; imports `std` and `orbit-common` only.
- `linux_landlock`: Linux Landlock read/write/TCP confinement primitives for plugin backends; imports `linux_sandbox`, `path_identity`, `process`, `runner`, `orbit-common`, and `orbit-types`.
- `linux_sandbox`: Linux Bubblewrap write-confinement compiler and spawn helper for CLI-backed agents; imports `credential_paths`, `linux_landlock`, `orbit-common`, and `orbit-types`.
- `macos_sandbox`: macOS `sandbox-exec` SBPL profile compilation and sandboxed spawn; imports `credential_paths`, `path_identity`, `process`, `orbit-common`, and `orbit-types`.
- `path_identity`: Canonical host-directory identity resolution for granted write roots; imports `orbit-common` only.
- `process`: Low-level child-process spawn and pre-exec inherited file-descriptor mapping (`InheritedFd`); imports `runner` and `orbit-common`.
- `result`: Re-export of `ExecutionResult` from `orbit-types`; imports `orbit-types` only.
- `runner`: Subprocess execution request (`ExecRequest`), environment/stdin modes, and `run_process` entry points; imports `process`, `sandbox`, `supervision`, `orbit-common`, and `orbit-types`.
- `sandbox`: `Sandbox` strategy trait and `NoSandbox` default implementation; imports `process`, `runner`, and `orbit-common`.
- `supervision`: Exec stream-capture and stop-cause policy over `orbit_common::process::supervisor`; imports `orbit-common` only.
- `validation_env`: Environment assembly for owner-side repository validation and `local_shell` tooling; imports `runner` and `orbit-common`.
