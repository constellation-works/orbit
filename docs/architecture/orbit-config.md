---
type: context
summary: Architecture map for orbit-config: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-27
tags: [architecture, orbit-config]
---

# orbit-config

## What it owns

Owner of `config.toml`: key registry, global-over-workspace layering, provenance, resolved views, comment-preserving atomic edits via `ConfigStore`. Security keys replace rather than merge, and `[machine]` is global-only. Callers pass explicit roots, so the crate never discovers the cwd or `$HOME`.

## Main modules and types

`registry` (`ConfigSnapshot`), `layering`, `store` (`ConfigStore` in `crates/orbit-config/src/store/persistence.rs`), `crew`, `operation`, `provider_limit`, `pilot_note`, `document` (`paths`, `io`, `parse`), `crew_defaults`. See ``crates/orbit-config/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-cmd`, `orbit-core`, `orbit-registry`, `orbit-runtime`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-config/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `alias`: Read-time translation aliases for deprecated configuration spellings; imports `document`, `crew`, `operation`, and `orbit-common`.
- `completion_hold`: Parsing and validation for `[[completion.hold]]` rules; imports `orbit-common` and `orbit-types` only.
- `crew`: Crew admission, task types, auto-effort policies, and weighted crew pools; imports `alias`, `orbit-common`, and `orbit-types`.
- `crew_defaults`: Built-in default crew registry leaf; imports `orbit-common` and `orbit-types` only.
- `document`: Admitted `config.toml` document paths, no-follow I/O, and TOML parsing below layering; imports `orbit-common` only.
- `experiment_keys`: Workflow keys explicitly admitted for task-local experiment overrides; imports `orbit-common` and `serde_json` only.
- `human_crew_seed`: Global `config.toml` seeding of the operator's human crew during `orbit init`; imports `crew_defaults`, `raw`, `resolved`, `store`, `orbit-common`, and `orbit-types`.
- `layering`: Global-over-workspace configuration merging and source provenance; imports lower config modules, `orbit-common`, and `orbit-types`.
- `lock_modes`: Ordered per-path footprint lock modes (`workflow.lock_modes`); imports `orbit-common` and `serde` only.
- `memory_limit`: Grammar and parser for worker memory limits (`machine.worker_memory_*`); imports `std` and `serde` only.
- `operation`: Typed review timing and operation preferences; imports `alias`, `document`, `orbit-common`, and `orbit-types`.
- `pilot_note`: Admission of `[workflow.pilot_notes.<label>]` instruction-file mappings; imports `document`, `orbit-common`, and `orbit-types`.
- `persistence`: Resolved persistent artifact store paths derived from explicit roots; imports `orbit-types` and `serde_json` only.
- `plugin_enablement`: Workspace-scoped `[plugin_enablement]` toggle admission; imports `roots`, `document`, `orbit-common`, and `orbit-types`.
- `plugins`: Dynamically named `[plugins.<ns>]` configuration section admission; imports `jsonschema`, `orbit-common`, and `serde_json` only.
- `provider_limit`: Provider usage-limit admission settings (`workflow.provider_limit_*`); imports `orbit-common` and `orbit-types` only.
- `raw`: Private serde schema for dynamic crew tables and retired keys; imports `registry` and `unknown`.
- `registry`: Admission registry (`define_config_settings!`) and snapshots for fixed `config.toml` keys; imports leaf config modules, `orbit-common`, and `orbit-types`.
- `regression_layers`: Ordered path classifier (`workflow.regression_layers`) for regression analytics; imports `orbit-common` and `serde` only.
- `resolved`: Consumer-facing `ResolvedConfig` view over admitted settings and policies; imports `layering`, `registry`, `store`, `unknown`, `orbit-common`, and `orbit-types`.
- `review_skip_tags`: Workspace `[workflow.review_skip_tags.<tag>]` admission; imports `document`, `tag_instructions`, `orbit-common`, and `orbit-types`.
- `roots`: Explicit global and workspace configuration directory roots (`ConfigRoots`); imports `std` only.
- `seed`: Rendering and writing a fresh default `config.toml`; imports `crew_defaults`, `layering`, `plugin_enablement`, `orbit-common`, and `orbit-types`.
- `seeding`: `seeding.routines` and `seeding.auto_tasks` workspace default policy; imports `roots`, `document`, `orbit-common`, and `orbit-types`.
- `store`: Surgical, comment-preserving `config.toml` reads and atomic writes (`ConfigStore`); imports `document`, `layering`, `registry`, `resolved`, `orbit-common`, and `orbit-types`.
- `tag_instructions`: `[workflow.tag_instructions.<tag>]` extra-instruction file mapping; imports `document`, `orbit-common`, and `orbit-types`.
- `unknown`: Forward-compatible diagnostics for unrecognized `config.toml` keys; imports `document`, `operation`, `registry`, and `tag_instructions`.
