---
type: context
summary: Architecture map for orbit-clock: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
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

## Ownership by module

- `clock_settings`: Host-local `clock.toml` schema and sweep cadence persistence; imports `orbit-common` and `serde` only.
- `converge`: Repair of an installed clock unit that has drifted from the running binary; imports sibling clock modules and `orbit-common`.
- `enable`: Pausing and resuming the OS clock through its native manager; imports `manager` and `orbit-common`.
- `inspect`: Inspection of the installed OS sweep-clock unit against the running binary; imports `install`, `manager`, `program`, and `orbit-common`.
- `install`: Rendering and installation of the platform clock unit and its sweep log path; imports `manager`, `settings`, and `orbit-common`.
- `manager`: Native unit manager (launchd or systemd) command invocation seam; imports `orbit-common` only.
- `program`: Parsing of the executable program configured in an installed launchd or systemd clock unit; imports `std` only.
- `settings`: Sweep-cadence updates and `clock_settings` re-exports; imports `clock_settings`, `install`, and `orbit-common`.
- `status`: Native manager queries for the clock's loaded, enabled, and health state; imports `inspect`, `install`, `manager`, `settings`, and `orbit-common`.
