---
type: pattern
summary: "Crate-Boundary Error Translation"
last_validated: 2026-09-12
---
# Crate-Boundary Error Translation

Each crate defines its own typed error (`thiserror`-derived) for internal use; `OrbitError` in `orbit-common` is the workspace-public error surface. A single translation function — `*_error_to_orbit` — lives next to the typed error and is called at every cross-crate boundary via `.map_err(...)`.

```rust
// In crate-foo:
#[derive(thiserror::Error, ...)]
pub struct FooError { pub kind: String, pub reason: String }

pub fn foo_error_to_orbit(error: FooError) -> OrbitError {
    if error.kind == "foo_invalid" {
        OrbitError::InvalidInput(error.reason)
    } else {
        OrbitError::Execution(error.to_string())
    }
}

// At any caller that returns OrbitError:
foo::do_thing(...).map_err(foo_error_to_orbit)?
```

The principle: internal code propagates the rich typed error so callers can match on variants; once the error crosses the crate boundary it's collapsed to `OrbitError` so the workspace's public surface stays uniform.

## When to reach for it

- **You're adding a new crate.** Define a typed error there. Export a `*_error_to_orbit` translator. Don't `pub use OrbitError` as your crate's error type — that couples your internals to the workspace surface.
- **Your crate already has its own error and now needs to be called from a crate that returns `OrbitError`.** The boundary is `.map_err(translator)?`, never an ad-hoc `OrbitError::Execution(other_err.to_string())` at the callsite.
- **The same `OrbitError` variant should be produced from many translation sites.** Centralizing the kind→variant mapping in one function keeps the public error surface coherent.

## When NOT to

- **Within a single crate.** Use the typed error directly. Translating mid-crate discards information you might want at the next layer.
- **You don't have a typed error yet.** A thin wrapper crate producing `OrbitError` directly is fine; introduce a typed error only when you have enough variants that matching on them adds value.
- **The "translation" is `OrbitError::from(other_err.to_string())`.** Stringifying loses the kind. If that's all your translator does, you don't need one — write the one-line `.map_err` at the boundary.

## Reference: `DispatchError` → `OrbitError`

An enum error whose variants act as the discriminator, with a translator that
maps each surfaced family to a specific `OrbitError` variant. From
`crates/orbit-engine/src/activity_job/dispatcher.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DispatchError {
    JobValidation(String),
    DeterministicActionUnavailable { activity: String, action: String },
    CliInvocationFailed(String),
    // ...
}
```

The translator lives next to the error and preserves validation failures and
recoverable VCS conflicts while collapsing the remaining dispatch failures:

```rust
pub fn dispatch_error_to_orbit(error: DispatchError) -> OrbitError {
    match error {
        DispatchError::JobValidation(message) => OrbitError::JobValidation(message),
        unavailable @ DispatchError::DeterministicActionUnavailable { .. } =>
            OrbitError::JobValidation(unavailable.to_string()),
        other => OrbitError::InvalidInput(other.to_string()),
    }
}
```

The live translator additionally preserves `DispatchError::RecoverableVcsConflict`
as `OrbitError::RecoverableVcsConflict`.

Other live translators in the same shape are `selector_error_to_orbit`
(`orbit-common::fs::selector`) and `rpc_error_to_orbit`
(`orbit-search::rpc`).

Patterns to copy:

- **Translator lives in the source crate, next to the error.** Not in `orbit-common`, not in each caller. The crate that *defined* `FooError` owns the kind→variant mapping. Re-export at the crate root so callers can `use crate_foo::foo_error_to_orbit;`.
- **Discriminator field drives the mapping.** A typed `kind: String` (or an enum, equivalently) lets the translator branch without exposing internal `thiserror` variants to consumers.
- **One named match per surfaced variant; everything else passes through.** "`InvalidData` → `InvalidInput`, `Io` → `Io`, default → `Execution`" is the right granularity — name the kinds callers will actually branch on, dump the rest into the generic bucket.
- **`.map_err(translator)?`, not `.map_err(|e| translator(e))?`.** The translator's signature is `FnOnce(E) -> OrbitError`, so the bare path works as a closure. The shorter form reads better at boundary sites.

Use this shape for every new crate in the workspace per the architecture diagram in `ARCHITECTURE.md`. A new typed error should land in the same PR as its translator. `scripts/check-error-translation.sh` (ORB-10013, wired into `make ci-fast` and CI guardrails) enforces the mechanically checkable core: registered boundary errors must export their translator from the owning crate, translators may not live in caller crates, and no foreign error type may be mapped to `OrbitError` variants at a call site.
