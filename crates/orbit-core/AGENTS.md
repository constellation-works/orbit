# orbit-core

Assemble the lower subsystems into `OrbitRuntime` and own the coordinated use cases. The layer that says yes or no: domain validation, authorization, auditing, lifecycle decisions. Transports only collect input.

## Rules

- Depends on nothing above it: never `orbit-cmd`, `orbit-cli`, `orbit-web`, or `orbit-agent`.
- Internal graph `runtime ← application ← adapter`, `composition` the only joiner, with `bootstrap → application` and never the reverse, enforced by `scripts/check-dependency-direction.sh`. Application and adapter reach composition and bootstrap only through application-owned ports.
- The runtime kernel is the `orbit-runtime` crate ([its guide](../orbit-runtime/AGENTS.md)). `src/runtime/mod.rs` re-exports it inside the crate (publicly only under `test-support`) and defines the `OrbitRuntime` wrapper. The kernel never calls back into this crate: pass it the value it needs, or keep the flow that needs a use case here.
- `Core` (`composition/core.rs`) has no public field or method. An app is `app/<subject>/`: `Command`, `Outcome`, `Error` and one public `exec`, with each verb's flow a private child module. Every `exec` runs through `app/exec/`, the only place that authorizes (`app/exec/admit.rs`) and audits (`app/exec/audit/`) a command: a command declares what it requires on its `Operation`, and a flow asks for a record-dependent requirement with `exec::require`; `app/invocation.rs` settles a command line's row after rendering and writes the row of an invocation no `exec` recorded. A flow does neither. `app` takes only `Core` from `composition`; the `Caller` a `Core` carries is `app/exec/caller.rs`'s.
- Put moved application operations in functions under their owning `app/<subject>/` or `unit/<name>/`; other modules call their `mod.rs` exports. Legacy application and adapter methods live on the wrapper. `adapter/` translates and delegates — a decision in an adapter is in the wrong place; `bootstrap/` runs once at open.
- `lib.rs` exports `app`, `Core` and `Error` and nothing else. A type or function a surface or `orbit-cmd` must name is re-exported from the app that owns its subject (the "Surface re-exports" block in `app/<subject>/mod.rs`), or comes from `orbit-types`; remove the re-export when the consumer goes.
- Operation handlers match exhaustively on the verb enum from `orbit_common::governance` — no default arm.
- Only `composition/store.rs` opens the SQLite store; `clippy.toml` disallows `Store::open` and `Store::open_read_only` elsewhere. Use the runtime's handle; code that must open another database receives a `StoreOpener` from its caller.
- Redaction is shared (`scripts/check-artifact-redaction-guardrail.sh`); no local `fn redact_*`.
- Two roots, always: global `~/.orbit/` plus nearest workspace `.orbit/`. Scoping per [`ARCHITECTURE.md`](../../ARCHITECTURE.md); never a third root.
- Test a use case through the public runtime API in crate-root `tests/` by default, whichever adapter calls it. A unit test under `src/**/tests/` is admitted only under the [unit-test admission criteria](../../docs/design-patterns/test_strategy.md#unit-test-admission-criteria); [test_layout.md](../../docs/design-patterns/test_layout.md) says where an admitted test lives.
