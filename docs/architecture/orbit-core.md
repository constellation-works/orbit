---
type: context
summary: Architecture map for orbit-core: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-core]
---

# orbit-core

## What it owns

Composes the runtime. `OrbitRuntime` owns a `RuntimeKernel` and derefs to it. Internal graph `runtime ← application ← adapter`, `composition` joins config/bootstrap/runtime/adapters..

## Layering

- `bootstrap` depends on `application` and never the reverse; application and adapter never name `bootstrap` or `composition`. Runtime names no higher layer.
- Application modules are ordered `contracts < task < job < automation < review < distributed < pull_drain < admission < workspace_auto < backlog_forecast < sweep_filing`. A lower module reaches a higher one only through a port trait the lower module owns and the higher one implements on `OrbitRuntime` (for example `task::TaskNoticePort`, `job::JobExperimentPort`).
- Modules outside that chain (`routines`, `auto_tasks`, `experiment`, `landing`, `codebase`, `resident`, `notice`, `evolution`, `agent_corpus`, `tag_instructions`) call downward freely and are reached from below only through such ports.
- The adapter sits above all of application, which reaches it only through application-owned ports (`contracts::ToolInvocationPort`, `pull_drain::drain::PullTransportPort`).
- `scripts/check-dependency-direction.sh` refuses paths, grouped trees, `super::` chains and calls to higher-module inherent methods that break these rules. Core never depends on `orbit-agent`, `orbit-cmd` or transport crates.

## Main modules and types

`application` (task, job, review, landing, distributed, pull_drain, admission, routines, search, notice), `adapter`, `composition`, `bootstrap`, `runtime` (`OrbitRuntime` in `crates/orbit-core/src/runtime/mod.rs`). See ``crates/orbit-core/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-automation`, `orbit-clock`, `orbit-common`, `orbit-config`, `orbit-engine`, `orbit-exec`, `orbit-registry`, `orbit-runtime`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-cmd`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-core/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.
