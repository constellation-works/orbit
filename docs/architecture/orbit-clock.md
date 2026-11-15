---
type: context
summary: Architecture map for orbit-clock: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-clock]
---

# orbit-clock

## What it owns

Host sweep clock: `clock.toml`, tick budget `TICK_DEADLINE`, launchd/systemd templates, install/pause/resume/cadence/health. Names only `orbit-common`.

## Main modules and types

`settings` (`ClockSettings`), `install` (`install_clock`), `converge`, `inspect`, `status` (`ClockStatus`), `manager`, `program`. See ``crates/orbit-clock/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`.
- Depended on by: `orbit-automation`, `orbit-core`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-clock/src/lib.rs`.

Design intent: [docs/design/routines/](../../docs/design/routines/) — this page describes the code as it is; the design folder holds intent.
