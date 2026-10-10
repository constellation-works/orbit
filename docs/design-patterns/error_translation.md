---
type: pattern
summary: "Crate-Boundary Error Translation"
last_validated: 2026-10-08
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

## Classify at translation, never from text

When callers must branch on *why* something failed, decide the class where the
native error is still in hand and carry it in the `OrbitError` variant. A
caller never re-derives it with `error.to_string().contains(...)`: any message
can quote such text (a child's stderr, a remote reply), and rewording a message
would silently change behaviour.

When a class must not change the wire contract, give it a variant that displays
and codes exactly like the variant it refines:

- `StorageAccessDenied { layer, message }`: a read-only or access-denied
  failure from `io::Error` kinds (`From<io::Error>`, `OrbitError::storage_io`)
  or SQLite codes (`storage::sqlite::sqlite_error`). It reads and codes as the
  `Io`, `Store` or `Migration` error named by `layer`. Query it with
  `is_readonly_or_access_failure()` and `storage_layer()`.
- `ClaimRefused { kind, message }`: an owner claim or handoff refusal. It reads
  and codes as `InvalidInput`. Query it with `claim_refusal()`.
- `ExecutionTimeout { timeout_ms, message }`: an execution failure caused by a
  deadline. It reads and codes as `Execution`. Query it with `is_timeout()`,
  which also covers `ProcessTimeout`.

Each wildcard match in another crate must name the new variant wherever it
names the twin, because `OrbitError` is `#[non_exhaustive]` and an unnamed
variant falls into the generic arm.

## Reference: `DispatchError` → `OrbitError`

An enum error whose variants act as the discriminator, with a translator that
maps each surfaced family to a specific `OrbitError` variant. From
`crates/orbit-engine/src/activity_job/dispatcher.rs`:

```rust
#[derive(Debug, Error, Clone)]
pub enum DispatchError {
    JobValidation(String),
    DeterministicActionUnavailable { activity: String, action: String },
    CliInvocationFailed(String),
    // ...
}
```

The translator lives next to the error. It preserves validation failures as
`JobValidation`, recoverable VCS conflicts, and live-completion refusals; other
dispatch errors collapse to `InvalidInput`.

```rust
pub fn dispatch_error_to_orbit(error: DispatchError) -> OrbitError {
    match error {
        DispatchError::JobValidation(message) => OrbitError::JobValidation(message),
        unavailable @ DispatchError::DeterministicActionUnavailable { .. } => {
            OrbitError::JobValidation(unavailable.to_string())
        }
        DispatchError::RecoverableVcsConflict {
            operation,
            original_base_sha,
            target_base_sha,
            conflicting_paths,
            diagnostic,
        } => OrbitError::RecoverableVcsConflict(Box::new(RecoverableVcsConflict {
            operation,
            original_base_sha,
            target_base_sha,
            conflicting_paths,
            diagnostic,
        })),
        DispatchError::TaskCompletionLiveRun { task_id, run_id } => {
            OrbitError::TaskCompletionLiveRun { task_id, run_id }
        }
        other => OrbitError::InvalidInput(format!("{other}")),
    }
}
```

For the current set of boundary translators and their owning crates, see the
registry in [`scripts/check-error-translation.sh`](../../scripts/check-error-translation.sh).

## Exception: `orbit-types` errors translate in `orbit-common`

`orbit-types` is below `orbit-common` and cannot name `OrbitError`, so a
translator cannot live beside its errors. Each `orbit-types` error that crosses
into `OrbitError` instead has one `impl From<Type> for OrbitError` in
`crates/orbit-common/src/error.rs`. The orphan rule allows it there because
`orbit-common` owns the target type. Callers convert with `?` or
`.map_err(OrbitError::from)`.

The variants still drive the mapping. `ReviewHistoryError` maps an unreadable
stored history to `Store` and a refused report revision to `InvalidInput`.
`WorkerBindingError` maps a defective binding to `InvalidInput` and a relation
outside the claimed task to `PolicyDenied`.

A caller that needs another `OrbitError` variant than the translator gives
keeps an explicit `map_err`. So does a caller that prefixes its own context
(a file path, a crew name). Neither may name the error type on the line that
builds the `OrbitError`. The script's `from_registry` lists these types.

Patterns to copy:

- **Translator lives in the source crate, next to the error.** Not in `orbit-common`, not in each caller. The crate that *defined* `FooError` owns the kind→variant mapping. Re-export at the crate root so callers can `use crate_foo::foo_error_to_orbit;`. The one exception is `orbit-types`, whose errors translate through `From` impls beside `OrbitError` (see above).
- **Discriminator field drives the mapping.** A typed `kind: String` (or an enum, equivalently) lets the translator branch without exposing internal `thiserror` variants to consumers.
- **One named match per surfaced variant; everything else passes through.** Preserve the distinctions callers need, then keep unmapped variants in the generic bucket chosen by the public error contract.
- **`.map_err(translator)?`, not `.map_err(|e| translator(e))?`.** The translator's signature is `FnOnce(E) -> OrbitError`, so the bare path works as a closure. The shorter form reads better at boundary sites.

Use this shape for every new crate in the workspace per the architecture diagram in `ARCHITECTURE.md`. A new typed error should land in the same PR as its translator. `scripts/check-error-translation.sh` (ORB-10013, wired into `make ci-fast` and CI guardrails) enforces the mechanically checkable core: registered boundary errors must export their translator from the owning crate, or for `orbit-types` errors have a `From` impl in `orbit-common`; translators may not live in caller crates; and no foreign error type may be mapped to `OrbitError` variants at a call site.
