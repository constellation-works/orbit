# orbit-core

Project instructions for the runtime composition crate.

## One job

Assemble every lower subsystem into `OrbitRuntime` and own the coordinated
use cases that need more than one of them. It is the authoritative place for
domain validation, authorization, auditing, and lifecycle decisions — the layer
that says *yes or no*, where transports only collect input.

It depends on the kernels below it and on nothing above: never `orbit-cmd`,
never `orbit-cli`/`orbit-web`, never `orbit-registry` (see
[orbit-cmd](../orbit-cmd/CLAUDE.md) for where Core and Registry meet), and never
`orbit-agent` (the engine's `cli_runner` owns that edge).

## The internal graph is enforced

`runtime <- application <- adapter`, with `composition` as the only joiner.
[`scripts/check-dependency-direction.sh`](../../scripts/check-dependency-direction.sh)
fails the build on a violation:

- [`runtime/`](src/runtime) — mechanisms: stores, event bus, audit, claims and
  reservations, tool and process execution, root resolution, the workspace
  catalog. It may not import `crate::application` or `crate::adapter`, and it
  may not call `ResolvedConfig::load` or construct `ConfigRoots` — resolved
  configuration is *handed to it* by composition.
- [`application/`](src/application) — shared use cases and their DTOs. It may
  not import `crate::adapter`.
- [`adapter/`](src/adapter) — protocol translation only:
  [`tool_host`](src/adapter/tool_host) for Orbit tools,
  [`engine_host`](src/adapter/engine_host) implementing
  `orbit_engine::RuntimeHost`, and [`command`](src/adapter/command) for command
  dispatch. Adapters translate and delegate; a decision made in an adapter is a
  decision in the wrong place.
- [`composition.rs`](src/composition.rs) — the only module that loads resolved
  config and joins bootstrap, runtime construction, and adapter registration.
- [`bootstrap/`](src/bootstrap) — initialization, managed-asset seeding, policy
  seeding, forward-only startup migrations. Runs once, at open.

The scheduling domain (routines, auto-tasks, delivery/state automation) lives in
`orbit-automation` and evaluates against its `AutomationHost` port
[ORB-11330, ORB-12262]. Core's side of that port is
[`adapter/automation_host`](src/adapter/automation_host) — the only
`impl AutomationHost for OrbitRuntime`, plus the thin `runtime.auto_task_*`
facade. Add a capability there by adding a port method, never by moving a
scheduling decision back into Core. `metrics` owns derived measurement. All use
the existing v2 job machinery.

The guardrail also fails if a retired path reappears: `src/command`,
`src/runtime/orbit_tool_host`, `src/runtime/engine/runtime_host.rs`, and the
scheduling glue that moved down — `src/application/{automation,auto_tasks,routines}`.
Do not recreate them; their successors are `orbit-cmd`, `adapter/tool_host`,
`adapter/engine_host`, and `orbit-automation`.

## Root re-exports are justified, not convenient

Every `pub use` at [`lib.rs`](src/lib.rs) must correspond to a real import in a
consumer crate (`orbit-cli`, `orbit-web`, `orbit-cmd`). Do not add one to
shorten a path. Anything else is imported from its owning module
(`orbit_core::application::…`, `orbit_core::runtime::…`) or, when the type is
not Core's, from its owning crate (`orbit_common`, `orbit_store`,
`orbit_engine`). When you remove a consumer, remove the re-export with it.

## Crate-specific invariants

- **Core decides; MCP and the CLI ask.** New authorization, quota, or
  validation logic belongs here even when the caller is a transport — do not
  mirror a Core rule as client-side orchestration.
- **Operation handlers are joined by the verb enum.**
  [`adapter/tool_host/friction_tools.rs`](src/adapter/tool_host/friction_tools.rs)
  matches exhaustively on `FrictionVerb`, whose spec table lives in
  `orbit_common::governance::friction`. The compiler is what proves every
  declared verb is wired, so a new verb must never get a default arm.
- **Redaction stays shared.** `scripts/check-artifact-redaction-guardrail.sh`
  forbids surface-local `fn redact_*` in
  [`adapter/tool_host`](src/adapter/tool_host) and
  [`application/task/add.rs`](src/application/task/add.rs).
- **Two roots, always.** Runtime resolution is a global root (`~/.orbit/`) plus
  a workspace root (nearest ancestor `.orbit/`). Scope decisions follow the
  table in [`ARCHITECTURE.md`](../../ARCHITECTURE.md) §Scoping Rules; do not
  invent a third root or read one root's artifact from the other's path.

## Tests

Sibling `tests/` directories per module
([`test_layout.md`](../../docs/design-patterns/test_layout.md)). Crate-root
[`tests/`](tests) is reserved for integration tests that drive a composed
runtime end-to-end, such as the fake-agent backend smokes. Test a use case at
its owning boundary — an `application` operation through `application`, not
through an adapter that happens to call it.
