---
type: context
summary: Architecture map for orbit-web: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-web]
---

# orbit-web

## What it owns

HTTP API + embedded dashboard (axum), `web connect` SSH tunnel, `/api/on/<host>` forwarding, lease intake socket/listener (inactive while path leases not configurable).

## Main modules and types

`api`, `serve` (`serve`, `serve_from_env` in `crates/orbit-web/src/serve.rs`), `state`, `lease`, `connect`, `host_tunnels`, `health`, `projections`. See ``crates/orbit-web/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-auth`, `orbit-cmd`, `orbit-common`, `orbit-config`, `orbit-core`, `orbit-registry`, `orbit-types`.
- Depended on by: `orbit-cli`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-web/src/lib.rs`.

Design intent: [docs/design/user-interface/](../../docs/design/user-interface/) — this page describes the code as it is; the design folder holds intent.
