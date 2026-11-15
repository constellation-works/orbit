---
type: context
summary: Architecture map for orbit-common: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-common]
---

# orbit-common

## What it owns

`OrbitError` and shared mechanisms: fs/path, process supervision, storage, protocol/YAML codecs, observability, security (release signing, redaction, `security::child_env`).

## Main modules and types

`error` (`OrbitError`), `fs`, `process` (`process::OwnedChild`, `process::supervisor`), `security`, `storage`, `protocol`, `observability`, `text`, `governance`, `migration`, `model`. See ``crates/orbit-common/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-types`.
- Depended on by: all except `orbit-types`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-common/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.
