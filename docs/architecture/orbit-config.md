---
type: context
summary: Architecture map for orbit-config: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-config]
---

# orbit-config

## What it owns

Owner of `config.toml`: key registry, global-over-workspace layering, provenance, resolved views, comment-preserving atomic edits via `ConfigStore`.

## Main modules and types

`registry` (`ConfigSnapshot`), `layering`, `store` (`ConfigStore` in `crates/orbit-config/src/store/persistence.rs`), `crew`, `operation`, `provider_limit`, `config_document`, `config_io`, `crew_defaults`. See ``crates/orbit-config/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-cmd`, `orbit-core`, `orbit-registry`, `orbit-runtime`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-config/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.
