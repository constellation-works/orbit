---
type: context
summary: "Layered crate graph, dependency rules, stability tiers and scoping; per-crate detail lives in docs/architecture/"
last_validated: 2026-11-23
---

# Architecture

This page holds the layering rules and the crate edge table. What each crate owns, its modules
and entry points live in [`docs/architecture/`](docs/architecture/README.md), one page per
crate; design intent lives in `docs/design/`. Add crate detail there, not here.

Orbit is a Rust workspace of layered crates. **Lower layers never depend on higher ones.**
[`scripts/check-dependency-direction.sh`](scripts/check-dependency-direction.sh) enforces every
crate edge, so a new edge means updating that script and the table below in the same PR.

```text
Surfaces      orbit-cli (command/mcp) · orbit-web
Composition   orbit-cmd
Transport     orbit-mcp
Application   orbit-core
Runtime       orbit-runtime
Domain        orbit-engine · orbit-automation · orbit-agent · orbit-tools · orbit-store
              orbit-search · orbit-registry · orbit-config · orbit-clock
Kernel        orbit-exec · orbit-policy · orbit-auth
Foundation    orbit-common · orbit-types
```

Domain crates own their data and transport. Application layers compose them. Kernel crates
expose reusable mechanisms and never depend back on a feature. `orbit-types` does no I/O;
its serde shapes are persisted contracts.

## Crates

| Crate | Tier | Depends on (internal) |
|---|---|---|
| [`orbit-types`](docs/architecture/orbit-types.md) | stable | — |
| [`orbit-common`](docs/architecture/orbit-common.md) | stable | types |
| [`orbit-config`](docs/architecture/orbit-config.md) | internal | common, types |
| [`orbit-policy`](docs/architecture/orbit-policy.md) | internal | common, types |
| [`orbit-exec`](docs/architecture/orbit-exec.md) | internal | common, types |
| [`orbit-auth`](docs/architecture/orbit-auth.md) | internal | common, types |
| [`orbit-search`](docs/architecture/orbit-search.md) | internal | common, types |
| [`orbit-store`](docs/architecture/orbit-store.md) | stable | common, types |
| [`orbit-registry`](docs/architecture/orbit-registry.md) | internal | common, config, types |
| [`orbit-tools`](docs/architecture/orbit-tools.md) | internal | common, exec, policy, types (dev: tools) |
| [`orbit-agent`](docs/architecture/orbit-agent.md) | internal | common, types |
| [`orbit-clock`](docs/architecture/orbit-clock.md) | internal | common |
| [`orbit-automation`](docs/architecture/orbit-automation.md) | internal | clock, common, store, types |
| [`orbit-engine`](docs/architecture/orbit-engine.md) | internal | agent, common, exec, store, tools, types (dev: engine) |
| [`orbit-runtime`](docs/architecture/orbit-runtime.md) | internal | common, config, engine, exec, policy, search, store, tools, types (dev: runtime) |
| [`orbit-mcp`](docs/architecture/orbit-mcp.md) | internal | common, registry, tools, types (dev: mcp) |
| [`orbit-core`](docs/architecture/orbit-core.md) | internal | automation, clock, common, config, engine, exec, registry, runtime, search, store, tools, types (dev: core, policy) |
| [`orbit-cmd`](docs/architecture/orbit-cmd.md) | internal | common, config, core, mcp, registry, store, tools, types |
| [`orbit-web`](docs/architecture/orbit-web.md) | internal | auth, cmd, common, config, core, registry, types (dev: engine, store, web) |
| [`orbit-cli`](docs/architecture/orbit-cli.md) | internal | cmd, common, config, core, mcp, registry, types, web (dev: engine, exec, tools) |

`desktop/` is the Tauri shell, a separate Cargo workspace outside this graph that depends on
no workspace crate.

Edge rules the guard enforces beyond the table:

- **Self dev-edges** (`dev: <self>`) only switch on the crate's own `test-support` feature for
  its tests. Test seams (scoped clocks, null hosts, substitute workers, fixture `gh`) exist only
  behind `test-support` and are absent from shipped builds.
- **Core → Policy is dev-only.** Shipped code reaches policy through `orbit-runtime` and
  `orbit-tools`; a normal, build or target-normal `orbit-core` → `orbit-policy` edge fails the
  guard.
- **CLI has no `orbit-auth` edge**, normal or dev; credential code reaches the binary only
  through `orbit-web`.
- **Other dev-edges** (Web → Engine/Store, CLI → Engine/Exec/Tools) exist for isolated HTTP
  fixtures and the sandbox broker integration test.
- Dependencies declared in `[workspace.dependencies]` are inherited in member manifests with
  `workspace = true`.

Inside `orbit-store`, `driver/file` and `driver/sqlite` are private, implement one technology
each and never import each other; `repository` enforces cross-driver invariants, `workflow`
holds explicit import/export/repair operations, and `compose` builds the stores. The same
guard enforces these arrows; see [orbit-store](docs/architecture/orbit-store.md).

## Stability tiers

Each crate declares `stability` under `[package.metadata.orbit]` in its `Cargo.toml`.
[`scripts/check-stability.sh`](scripts/check-stability.sh) fails if the marker is missing or
invalid. There is no automated API diff; the tier signals refactor scope to reviewers.

- **stable**: breaking changes need owner sign-off.
- **experimental**: free to refactor, and downstream depends at its own risk.
- **internal**: refactor freely.

## Scoping rules

Orbit has two roots: global `~/.orbit/` and the workspace `.orbit/`. When registered
repositories share one data root, the selected workspace binding scopes runtime identity and
every workspace-only artifact below.

| Artifact | Strategy | Notes |
|---|---|---|
| Tasks, job runs, run traces | Workspace only | Per-repo backlog and execution artifacts |
| Search index | Workspace only | Lexical task index |
| Activities, jobs, skills | Merge by key | Global defaults, workspace overrides by name |
| Policies | Merge by key | Workspace overrides profiles; global deny rules accumulate |
| Command audit | Global only | One authoritative SQLite trail |
| Global defaults stamp | Global only | Lets a warm open skip re-hashing managed catalogs |

## Product identity

Orbit is the only supported application profile. A `.orbit-product` marker, checked in
[`application::workspace::product_profile`](crates/orbit-core/src/application/workspace/product_profile.rs),
stops another product from initializing or reconciling an Orbit root. A second profile would
have to be fixed before state opens and carried through every entry point, not filtered in
tool discovery.
