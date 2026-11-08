---
type: context
summary: Architecture map for orbit-cmd: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-cmd]
---

# orbit-cmd

## What it owns

Composes the application for CLI and web: command groups, runtime assembly, routines, managed-worker transports, `orbit doctor`, `orbit host`, update, browser-kit.

## Main modules and types

`doctor` (`DoctorCommands`), `hosts`, `update`, `browser_kit`, `mcp_clients`, `log_format`, `registry`, `task`. See ``crates/orbit-cmd/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-config`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-cmd/src/lib.rs`.

Design intent: [docs/design/terminal-interface/](../../docs/design/terminal-interface/) — this page describes the code as it is; the design folder holds intent.
