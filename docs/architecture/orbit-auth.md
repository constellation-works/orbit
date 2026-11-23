---
type: context
summary: Architecture map for orbit-auth: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-auth]
---

# orbit-auth

## What it owns

All credential code for the lease listener: `orbl_` tokens, principals/scopes, allow-or-deny, token store, environment and client-address checks, plus the keyed request limiter and coalesced rejection audits.

## Main modules and types

`token`, `scope`, `store`, `environment`, `client_address`, `limiter` (see `crates/orbit-auth/src/lib.rs`). See ``crates/orbit-auth/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-auth/src/lib.rs`.

Design intent: [docs/design/policy-sandbox/](../../docs/design/policy-sandbox/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `token`: the token format (`orbl_` + 256 random bits from `getrandom`), generation, SHA-256 hashing and lease-token checks using the shared `orbit_common::security::token` minting and constant-time comparison primitives.
- `scope`: principals, classes, verbs, workspace scope, priority ceilings, `hard` permission and the allow-or-deny decision with typed refusals.
- `store`: the listener user's own SQLite store `lease-tokens.db`, its schema, forward-only migrations and audit rows, including the listener's refusals.
- `environment`: the execution-environment check every listener-user command and the listener run first.
- `client_address`: the listener's client-address rule, the only reader of forwarding headers.
- `limiter`: the bounded keyed request limiter and coalesced rejection audits.
