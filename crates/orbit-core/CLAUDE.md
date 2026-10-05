# orbit-core

Assemble the lower subsystems into `OrbitRuntime` and own the coordinated use cases. The layer that says yes or no: domain validation, authorization, auditing, lifecycle decisions. Transports only collect input.

- Depends on nothing above it: never `orbit-cmd`, `orbit-cli`, `orbit-web`, `orbit-registry`, or `orbit-agent`.
- Internal graph `runtime ← application ← adapter`, `composition` the only joiner, enforced by `scripts/check-dependency-direction.sh`. `runtime/` may not load config (it's handed in); `adapter/` translates and delegates — a decision in an adapter is in the wrong place; `bootstrap/` runs once at open. Retired paths (`src/command`, `src/runtime/orbit_tool_host`, `src/runtime/engine/runtime_host.rs`) must not reappear.
- Every `pub use` in `lib.rs` must match a real import in a consumer crate; remove the re-export when the consumer goes.
- Operation handlers match exhaustively on the verb enum from `orbit_common::governance` — no default arm.
- Redaction is shared (`scripts/check-artifact-redaction-guardrail.sh`); no local `fn redact_*`.
- Two roots, always: global `~/.orbit/` plus nearest workspace `.orbit/`. Scoping per [`ARCHITECTURE.md`](../../ARCHITECTURE.md); never a third root.
- Crate-root `tests/` is for end-to-end composed-runtime integration only. Test a use case at its owning boundary (`application`, not the adapter calling it).
