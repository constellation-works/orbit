---
type: context
summary: Architecture map for orbit-auth: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-auth]
---

# orbit-auth

## What it owns

All credential code for the lease listener: `orbl_` tokens, principals/scopes, allow-or-deny, token store, environment and client-address checks.

## Main modules and types

`token`, `scope`, `store`, `environment`, `client_address`, `limiter` (see `crates/orbit-auth/src/lib.rs`). See ``crates/orbit-auth/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-auth/src/lib.rs`.

Design intent: [docs/design/policy-sandbox/](../../docs/design/policy-sandbox/) — this page describes the code as it is; the design folder holds intent.
