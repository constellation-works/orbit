# orbit-runtime

The runtime kernel `orbit-core` builds `OrbitRuntime` on: `RuntimeKernel` and the mechanisms it owns (tool dispatch, audit, plugins, workspace binding, host probes, worker coordination), plus the actor context, root paths and cost types. It's an internal crate, split out so an application or adapter edit doesn't recompile the kernel. `orbit-core` re-exports it as `orbit_core::runtime`, and consumers keep using that path.

## Rules

- Never depends on `orbit-core`, `orbit-cmd`, `orbit-web`, `orbit-cli` or `orbit-registry`; `scripts/check-dependency-direction.sh` encodes the allowed list. The kernel may not load config or open the store: composition resolves the config, opens the store and hands both in.
- The kernel reaches application and adapter code only through `ApplicationPort` (`src/application_port.rs`), which `orbit-core` implements and passes to the `build_*` constructors. Each port method has a same-named `pub(crate)` forwarder on `RuntimeKernel`. Prefer moving a mechanism into this crate over adding a port method; add one only for an operation that must stay a use case.
- Runtime state lives in four layer structs, `Base`, `Stored`, `Host` and `Run` (`src/{base,stored,host,run}/mod.rs`), each holding the one below. A new field goes to the lowest layer in which every operation could use it and gets a row in [the field inventory](../../docs/architecture/orbit-runtime.md#runtime-layers); `RuntimeKernel` gains no field. Reach a lower layer by its accessor (`rt.base()`), never by `Deref`.
- Keep the public surface to what `orbit-core` uses. `orbit-core` adds methods on its `OrbitRuntime` wrapper, never on `RuntimeKernel`, and the wrapper re-declares a kernel builder or a name shadowed by one of its trait impls (see `crates/orbit-core/src/runtime/builders.rs` and `crates/orbit-core/src/runtime/forwarders.rs`).
- `assets.rs` embeds the bundled catalog from `../orbit-core/assets/`, and that tree stays canonical there.
- Test-only seams are gated `#[cfg(any(test, feature = "test-support"))]` so `orbit-core`'s tests can reach them; `test-support` is enabled only from `[dev-dependencies]` or `orbit-core`'s own `test-support` feature. Unit tests sit in sibling `tests/` dirs. A test that needs a composed `OrbitRuntime` lives under `orbit-core`'s `src/runtime/tests/`.
- Two roots, always: global `~/.orbit/` plus nearest workspace `.orbit/`, scoped per [`ARCHITECTURE.md`](../../ARCHITECTURE.md).
