# orbit-core

Assemble the lower subsystems into `OrbitRuntime` and own the coordinated use cases. The layer that says yes or no: domain validation, authorization, auditing, lifecycle decisions. Transports only collect input.

- Depends on nothing above it: never `orbit-cmd`, `orbit-cli`, `orbit-web`, `orbit-registry`, or `orbit-agent`.
- Internal graph `runtime ← application ← adapter`, `composition` the only joiner, enforced by `scripts/check-dependency-direction.sh`. The runtime kernel is the `orbit-runtime` crate ([its guide](../orbit-runtime/CLAUDE.md)). `src/runtime/mod.rs` re-exports it and defines the `OrbitRuntime` wrapper; `src/runtime/application_port.rs` holds the `ApplicationPort` implementation the kernel calls back through. Put application and adapter methods on the wrapper, never in the kernel. `adapter/` translates and delegates — a decision in an adapter is in the wrong place; `bootstrap/` runs once at open. The retired path `src/command` must not reappear.
- Every `pub use` in `lib.rs` must match a real import in a consumer crate; remove the re-export when the consumer goes.
- Operation handlers match exhaustively on the verb enum from `orbit_common::governance` — no default arm.
- Redaction is shared (`scripts/check-artifact-redaction-guardrail.sh`); no local `fn redact_*`.
- Two roots, always: global `~/.orbit/` plus nearest workspace `.orbit/`. Scoping per [`ARCHITECTURE.md`](../../ARCHITECTURE.md); never a third root.
- Test a use case through the public runtime API in crate-root `tests/` by default, whichever adapter calls it. A unit test under `src/**/tests/` is admitted only under the [unit-test admission criteria](../../docs/design-patterns/test_strategy.md#unit-test-admission-criteria); [test_layout.md](../../docs/design-patterns/test_layout.md) says where an admitted test lives.
