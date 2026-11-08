---
type: context
summary: Architecture map for orbit-engine: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-engine]
---

# orbit-engine

## What it owns

Executes activities/jobs: template rendering, retries, subprocess + tool-aware automation, CLI agent runner; candidate identity, reviewer repair commits, `pr_open`/`pr_complete` rechecks, CI evidence (`ci_log_signature`).

## Main modules and types

`activity_job` (`dispatcher`, `job_executor`, `cli_runner`, `crew`, `audit_writer`), `executor`, `context` (`RuntimeHost`), `template`, `condition`. See ``crates/orbit-engine/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-agent`, `orbit-common`, `orbit-exec`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-core`, `orbit-runtime`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-engine/src/lib.rs`.

Design intent: [docs/design/activity-job/](../../docs/design/activity-job/) — this page describes the code as it is; the design folder holds intent.
