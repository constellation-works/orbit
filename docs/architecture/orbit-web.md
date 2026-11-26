---
type: context
summary: Architecture map for orbit-web: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
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

## Ownership by module

- `api`: JSON HTTP handlers for the dashboard delegating to shared projections; imports lower web modules and workspace domain/composition crates.
- `assets`: Embedded dashboard HTML, CSS, JS, and font asset routes with gzip compression; imports `state` and `orbit-core` (never `api` or `serve`).
- `connect`: Client-side SSH tunnel convenience behind `orbit web connect`; imports `ssh_tunnel`, `orbit-common`, and `orbit-core` (never `api` or `serve`).
- `doctor_report`: Bounded, cached on-demand `orbit doctor` probe report for the dashboard; imports `orbit-cmd` and `orbit-core` (never `api` or `serve`).
- `health`: `/healthz` liveness and opt-in readiness diagnostics; imports `log_format`, `state`, `orbit-cmd`, `orbit-common`, and `orbit-core` (never `api` or `serve`).
- `heap`: Heap allocation policy for the long-running dashboard server; imports `axum`, `tokio`, and platform/libc bindings only (never `api` or `serve`).
- `host_tunnels`: Per-host SSH tunnels backing `/api/on/<host>/...` dashboard forwarding; imports `connect`, `ssh_tunnel`, and `orbit-common` (never `api` or `serve`).
- `lease`: Lease listener and operator-owned Unix-socket intake transport; imports `api::leases`, `shutdown`, `state`, `orbit-auth`, `orbit-common`, `orbit-core`, and `orbit-types` (never `serve`).
- `log_format`: Web log scanning and HTML rendering for `/api/log` and `/api/diagnostics`; imports `parse`, `orbit-cmd`, `orbit-common`, and `orbit-core` (never `api` or `serve`).
- `parse`: Dashboard query duration and timestamp parsers; imports `orbit-common` and `orbit-core` (never `api` or `serve`).
- `projections`: Dashboard `*_to_json` detail projections built on shared Core show records; imports `runtime_memo`, `state`, `why_stuck`, `orbit-core`, and `orbit-types` (never `api` or `serve`).
- `run_failure`: The diagnostic line for a held or failed run, rendered from `app::run` evidence; imports `orbit-core` and `orbit-cmd` (never `api` or `serve`).
- `runtime_memo`: Process-local single-flight TTL cache keyed by runtime identity or host inputs; imports `axum`, `orbit-core`, and `serde_json` only (never `api` or `serve`).
- `serve`: Axum HTTP server bootstrap, loopback bind enforcement, middleware, and graceful shutdown; imports `api`, `assets`, `connect`, `health`, `heap`, `lease`, `shutdown`, `state`, and composition/core crates.
- `shutdown`: SIGINT and SIGTERM future shared by the dashboard server and lease listener; imports `orbit-core` only (never `api` or `serve`).
- `ssh_tunnel`: Client-side SSH local-forward tunnel implementation for `orbit web connect`; imports `host_tunnels`, `orbit-common`, and `orbit-core` (never `api` or `serve`).
- `state`: Multi-workspace dashboard state and registry reconciliation; imports `doctor_report`, `host_tunnels`, `runtime_memo`, and lower workspace crates (never `api` or `serve`).
- `why_stuck`: Read-only "Why stuck" diagnostic facts for blocked tasks and held delivery runs; imports `run_failure` and `orbit-core` (never `api` or `serve`).
