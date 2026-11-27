---
type: context
summary: "Layered crate graph, dependency rules, stability tiers and scoping; per-crate detail lives in docs/architecture/"
last_validated: 2026-11-25
---

# Architecture

This page holds the layering rules and the crate edge table. What each crate owns, its modules
and entry points live in [`docs/architecture/`](docs/architecture/README.md), one page per
crate; design intent lives in `docs/design/`. Add crate detail there, not here.

Orbit is a Rust workspace of layered crates. **Lower layers never depend on higher ones.**
[`scripts/check-dependency-direction.sh`](scripts/check-dependency-direction.sh) enforces every
crate edge, so a new edge means updating that script and the table below in the same PR.
[`scripts/check-architecture-map.py`](scripts/check-architecture-map.py) enforces STD-02 §R43:
every workspace crate appears in the table below, every top-level module appears in its crate
page's `## Ownership by module` section, every code-formatted repository path exists, and every
crate root and top-level module opens on line 1 with a `//!` doc comment.

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

| Crate | Tier | Owns | Depends on (internal) |
|---|---|---|---|
| [`orbit-types`](docs/architecture/orbit-types.md) | stable | I/O-free shared serde contracts, pure constructors, and narrow domain errors. | — |
| [`orbit-common`](docs/architecture/orbit-common.md) | stable | `OrbitError` and shared workspace mechanisms (fs, process, security, storage, protocol, governance, observability, text, migration, model defaults). | types |
| [`orbit-config`](docs/architecture/orbit-config.md) | internal | `config.toml` schema admission, global-over-workspace layering, provenance, resolved views, atomic edits, and seeding over explicit roots. | common, types |
| [`orbit-policy`](docs/architecture/orbit-policy.md) | internal | Filesystem-profile policy resolution and deny-read/deny-modify path evaluation. | common, types |
| [`orbit-exec`](docs/architecture/orbit-exec.md) | internal | Sandboxed and supervised subprocess execution, capture, timeouts, and build/browser-kit environments. | common, types |
| [`orbit-auth`](docs/architecture/orbit-auth.md) | internal | Lease-listener credentials (`orbl_` tokens, scopes, token store, environment/client-address checks, and request rate limiting). | common, types |
| [`orbit-search`](docs/architecture/orbit-search.md) | internal | SQLite FTS5 lexical indexing and BM25 retrieval over synchronously maintained task chunks. | common, types |
| [`orbit-store`](docs/architecture/orbit-store.md) | stable | Record contracts, private file and SQLite drivers, concrete repositories, one-shot migrations, and store composition. | common, types (dev: store) |
| [`orbit-registry`](docs/architecture/orbit-registry.md) | internal | Machine identity, logical workspace catalog, local checkout/publication bindings, and registered remote hosts. | common, config, types |
| [`orbit-tools`](docs/architecture/orbit-tools.md) | internal | Built-in and external tool registry, plugin tool loading/execution, MCP tool rules, and the `gh` CLI contract. | common, exec, policy, types (dev: tools) |
| [`orbit-agent`](docs/architecture/orbit-agent.md) | internal | Provider CLI runtimes, request/response projections, and redacted audit blob sinks. | common, types |
| [`orbit-clock`](docs/architecture/orbit-clock.md) | internal | Host sweep clock (`clock.toml`, launchd/systemd templates, install, pause/resume, cadence, convergence, and health). | common |
| [`orbit-automation`](docs/architecture/orbit-automation.md) | internal | Deterministic scheduling domain for routines, auto-tasks, CEL condition triggers, delivery/review rules, and state checkpoints. | clock, common, store, types |
| [`orbit-engine`](docs/architecture/orbit-engine.md) | internal | Activity/job execution engine, template/condition evaluation, sandboxed provider dispatch, and deterministic VCS/PR automation actions. | agent, common, exec, store, tools, types (dev: engine) |
| [`orbit-runtime`](docs/architecture/orbit-runtime.md) | internal | Runtime kernel (`RuntimeKernel`), stores, event bus, audit, reservations, plugin host, host probes, and tool execution below application use cases. | common, config, engine, exec, policy, search, store, tools, types (dev: runtime) |
| [`orbit-mcp`](docs/architecture/orbit-mcp.md) | internal | Model Context Protocol transport over rmcp (stdio, TCP listener, SSH proxy, federated mux, toolsets, and presentation). | common, registry, tools, types (dev: mcp) |
| [`orbit-core`](docs/architecture/orbit-core.md) | internal | Directional application operations, protocol adapters, workspace bootstrap, metrics, and composition over `orbit-runtime`. | automation, clock, common, config, engine, exec, registry, runtime, search, store, tools, types (dev: core, policy) |
| [`orbit-cmd`](docs/architecture/orbit-cmd.md) | internal | Shared command composition layer joining Core runtimes to machine-local Registry state for CLI and dashboard surfaces. | common, config, core, mcp, registry, runtime, store, tools, types |
| [`orbit-web`](docs/architecture/orbit-web.md) | internal | Axum HTTP API, embedded dashboard assets, SSH tunnel connect/forwarding, and lease-intake transport. | auth, cmd, common, config, core, registry, types (dev: web) |
| [`orbit-cli`](docs/architecture/orbit-cli.md) | internal | Clap binary entry point (`orbit`), command dispatch, plugin CLI groups, audit middleware, and output formatting. | cmd, common, config, core, mcp, registry, types, web (dev: exec, tools) |

`desktop/` is the Tauri shell, a separate Cargo workspace outside this graph that depends on
no workspace crate.

Edge rules the guard enforces beyond the table:

- **Self dev-edges** (`dev: <self>`) only switch on the crate's own `test-support` feature for
  its tests. Test seams (scoped clocks, host fixtures, substitute workers, fixture `gh`) exist only
  behind `test-support` and are absent from shipped builds.
- **Core → Policy is dev-only.** Shipped code reaches policy through `orbit-runtime` and
  `orbit-tools`; a normal, build or target-normal `orbit-core` → `orbit-policy` edge fails the
  guard.
- **CLI has no `orbit-auth` edge**, normal or dev; credential code reaches the binary only
  through `orbit-web`.
- **Surfaces reach a workspace only through `orbit-core`'s apps.** `orbit-core` exports
  `app`, `Core` and `Error` and nothing else, and `orbit-cli` and `orbit-web` have no edge,
  normal or dev, to `orbit-runtime`, `orbit-store` or `orbit-engine`, so a surface that names
  the runtime, the store or the engine does not compile. Their fixtures reach those crates
  through `orbit-core`'s `test-support` feature.
- **Composition names the kernel.** `orbit-cmd` → `orbit-runtime` exists because `orbit-cmd`
  implements the kernel's workspace-catalog trait and resolves roots before a workspace opens.
- **Other dev-edges** (CLI → Exec/Tools) exist for the sandbox broker integration test.
- Dependencies declared in `[workspace.dependencies]` are inherited in member manifests with
  `workspace = true`.

Inside `orbit-store`, `crates/orbit-store/src/driver/file` and `crates/orbit-store/src/driver/sqlite` are private, implement one technology
each and never import each other; SQLite writer connections stay private to their driver. Record shapes live in `contract` and concrete records live in `repository`. Only `boundary`
sequences writes across the canonical task files, the registry index and host reservations.
The `migrate` layer runs import, export, repair and layout upgrades, and `compose` builds the stores. The same
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
