---
type: context
summary: Architecture map for orbit-common: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-common]
---

# orbit-common

## What it owns

`OrbitError` and shared mechanisms: fs/path, process supervision, storage, protocol/YAML codecs, observability, security (release signing, redaction, `security::child_env`).

## Main modules and types

`error` (`OrbitError`), `fs`, `process` (`process::OwnedChild`, `process::supervisor`), `security`, `storage`, `protocol`, `observability`, `env_vars` (fixed process environment names and their reference entries), `text`, `governance`, `migration`, `model`, `model_defaults`. See ``crates/orbit-common/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-types`.
- Depended on by: all other workspace crates except `orbit-types`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-common/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `env_vars`: Names and reference entries for the fixed environment variables Orbit reads; imports no internal modules.
- `error`: Workspace surface error type (`OrbitError`), structured refusal payloads, and error classification helpers; imports `fs` and `orbit-types`.
- `fs`: Shared filesystem, locking, path-safety, atomic I/O, Git, glob, and selector primitives; imports `error`, `process`, and `orbit-types`.
- `governance`: Shared operation registries, capability authorization, tool schemas, and governance specs; imports `error`, `protocol`, and `orbit-types`.
- `migration`: Forward-only schema migrations for YAML artifacts; imports `error`.
- `model`: Shared model pricing tables and cost-lookup helpers; imports `orbit-types` only.
- `model_defaults`: Single source of truth for Orbit's default model names; imports `std` only.
- `observability`: Tracing subscriber setup, log rotation, and audit execution IDs; imports `fs` and `security`.
- `process`: Bounded child-process execution, process-group supervision, build-budget admission, and signal helpers; imports `error`, `fs`, `security`, and `text`.
- `protocol`: Canonical JSON ordering, YAML codecs, and shared tool-input parsing; imports `error` and `orbit-types`.
- `security`: Secret redaction, child-environment allowlisting, token minting, and release signature verification; imports `error`, `fs`, `process`, and `orbit-types`.
- `storage`: Content-addressed blob storage, sweep helpers, timestamps, and shared SQLite utilities; imports `error`, `fs`, and `security`.
- `test_env`: Ambient-process isolation and validation helpers for re-executed child tests; imports `fs`, `process`, `security`, and `orbit-types`.
- `test_fixtures`: Frozen model-name constants for deterministic tests; imports `model_defaults` only.
- `test_process`: Bounded retries for launching freshly written test executables on Linux; imports `std` only.
- `text`: UTF-8-safe byte-index bounding and Unicode-scalar edit-distance helpers; imports `std` only.
