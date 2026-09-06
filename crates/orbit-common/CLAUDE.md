# orbit-common

Project instructions for the shared mechanism crate.

## One job

Mechanisms every layer needs, sitting directly above `orbit-types` and below
everything else. It owns the workspace-wide `OrbitError` and a small set of
responsibility-named modules; it owns no feature, no runtime, and no storage
backend.

The split against its only dependency is sharp: **`orbit-types` says what a
thing is, `orbit-common` says how to do something with it.** A pure shape with
no behavior belongs in `orbit-types`. Anything that touches the filesystem, a
process, an environment variable, a SQLite connection, YAML, or a tracing
subscriber belongs here — or, if it is specific to one feature, in that
feature's crate rather than here.

## Modules are named for a responsibility, not a feature

[`error`](src/error.rs), [`fs`](src/fs), [`governance`](src/governance),
[`migration`](src/migration), [`model`](src/model),
[`observability`](src/observability), [`process`](src/process),
[`protocol`](src/protocol), [`security`](src/security),
[`storage`](src/storage).

Never add a module named after a caller (`core_support`, `cli_helpers`) or a
vertical feature. If new code does not fit an existing responsibility, that is
evidence it belongs to the crate that needs it, not evidence for a new bucket
here. Each top-level module's `mod.rs` is declarations plus
`#[cfg(test)] mod tests;` only — see [`security/mod.rs`](src/security/mod.rs).
Behavior lives in the leaf files.

Tests use the sibling `tests/` layout
([`test_layout.md`](../../docs/design-patterns/test_layout.md)).
[`tests/task_artifacts_v2.rs`](tests/task_artifacts_v2.rs) at the crate root is
a Cargo integration test against the public surface — keep unit coverage in the
sibling directories instead of growing that file.

## Single-owner invariants

Some modules here exist *because* the behavior must have exactly one
implementation. Do not reimplement these at a call site, and do not add a
surface-local variant:

- [`security::child_env`](src/security/child_env.rs) is the only builder for an
  agent-subprocess environment. It is **allowlist**-based on purpose: a
  denylist admits every credential nobody thought to name. Every subprocess
  launcher applies it to a *cleared* environment; `orbit-config` only supplies
  the operator's `[execution.env]` pass list.
- [`security::release`](src/security/release.rs) is the only Rust copy of the
  release signing key set and of checksum-manifest verification. `install.sh`
  and `npm/scripts/install-binary.js` carry their own copies because they run
  before any Rust exists; `scripts/check-installer-pubkey.sh` fails the build
  when the three drift (L-0044). Every Rust consumer — `orbit update`,
  `orbit semantic install` — reads this module.
- [`security::redaction`](src/security/redaction.rs) is the only redaction
  implementation. `scripts/check-artifact-redaction-guardrail.sh` fails the
  build if a task/friction tool surface or a CLI-runner argv path grows its own
  `fn redact_*`.
- [`fs::selector`](src/fs/selector.rs) owns `SelectorParseError` and its
  `selector_error_to_orbit` translator; per
  [`error_translation.md`](../../docs/design-patterns/error_translation.md) no
  caller crate may translate it.
- [`migration`](src/migration) is the forward-only, read-time YAML migration
  framework. It deliberately has no rollback and no write-back; one-shot
  importers belong in `orbit-store::workflow`.

## Operation registries live here, handlers do not

[`governance::operation`](src/governance/operation.rs) is the
operations-as-data kernel, and [`governance::friction`](src/governance/friction)
is the first noun declared through it. The kernel lives in this leaf crate
precisely so every consumer surface (`orbit-tools`, `orbit-cli`, `orbit-web`,
`orbit-core`) can read the same table without a new dependency edge, which
means it must stay transport- and runtime-agnostic: **no clap types, no axum
types, no `OrbitRuntime` handle.**

Handlers need a runtime, so the handler table lives in `orbit-core` and is
joined to the spec table by the noun's verb enum. Adding a verb here without
its handler arm is a compile error by construction — that is the design, not a
gap to paper over with a default arm.

Every string in a registry (tool name, parameter name, CLI flag spelling, help
text) is shipped contract. Changing one is a consumer-visible break.

## Feature flags

- `sqlite` gates [`storage::sqlite`](src/storage/sqlite.rs) so non-SQLite
  consumers (`orbit-policy`, `orbit-exec`) do not pull `rusqlite`.
- `test-util` exposes [`test_env`](src/test_env.rs) and
  [`test_fixtures`](src/test_fixtures.rs) to sibling crates, which cannot see
  this crate's `#[cfg(test)]` items. Consume it from `[dev-dependencies]` only;
  never from a production dependency.
- `clap` forwards to `orbit-types/clap` and adds nothing of its own.

Keep gated code behind the flag rather than always-compiled with an `#[allow]`.
