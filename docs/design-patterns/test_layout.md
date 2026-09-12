---
type: pattern
summary: "Per-Module Sibling tests/ Directory"
last_validated: 2026-09-12
---
# Per-Module Sibling tests/ Directory

Unit tests for the source files in a module live in a *sibling* `tests/` directory under that same module. Each test file mirrors its source file by name. Because the test file is a sibling of the source it covers (not a child of it), it can only reach `pub`, `pub(crate)`, and `pub(super)` items — never raw private ones. That *structurally* enforces "test through the module's exposed surface" without relying on author discipline.

```text
src/command/
  mod.rs                          # declares: mod skill; mod render; #[cfg(test)] mod tests;
  skill.rs                        # source
  render.rs                       # source
  tests/
    mod.rs                        # declares: mod skill; mod render;
    skill.rs                      # tests for sibling skill.rs
    render.rs                     # tests for sibling render.rs
```

```rust
// src/command/mod.rs        (or src/command.rs — both module-rooting styles work)
mod skill;
mod render;

#[cfg(test)]
mod tests;
```

```rust
// src/command/skill.rs
pub(crate) fn install_skill(...) -> Result<...> { ... }
fn internal_helper() { ... }                       // private — invisible to siblings
```

```rust
// src/command/tests/mod.rs
mod skill;
mod render;
```

```rust
// src/command/tests/skill.rs
use super::super::skill::install_skill;            // up two: tests/ -> command -> command::skill

#[test]
fn installs_skill_into_workspace() { ... }
```

The principle: keep production directory listings free of `#[cfg(test)]` artefacts, and let the file structure itself enforce that tests only exercise public seams. If you find you need access to a private item, that's a signal — widen the visibility deliberately (with a comment) or restructure the seam, don't reach around the convention.

## When to reach for it

- **You're adding a new module that needs tests.** Use this layout from the first file. If the module is a single source file with no nested directory yet, still put tests in a sibling `tests/<name>.rs`.
- **A module's inline test block has grown beyond a smoke test.** Move it to `<module>/tests/<source_filename>.rs` and let untestability surface as design pressure for cleaner public seams.
- **A test wants to verify behaviour at a `pub`/`pub(crate)` boundary.** That's exactly what this layout targets.

## When NOT to

- **You're tempted to "fix" a test failure by widening a private to `pub(crate)` purely for test access.** That's the smell the structural enforcement is meant to catch. Either restructure so the seam is at a deliberate public boundary, or accept that the helper is covered transitively through its caller. Visibility widening that exists *only* for tests is debt.
- **You're writing an integration test that exercises the crate's public API end-to-end.** Use crate-root `tests/<name>.rs` — Cargo compiles each as a separate binary against the crate's public surface, which is exactly what integration tests want.
- **The module has zero non-trivial logic worth a unit test.** Don't scaffold an empty `tests/` for the sake of uniformity.

## Edge case: standalone top-level files

For source files directly under `src/` with no enclosing module directory, tests go in a top-level `src/tests/` directory mirroring them:

```text
src/
  lib.rs                          # declares: mod foo; #[cfg(test)] mod tests;
  foo.rs
  tests/
    mod.rs                        # declares: mod foo;
    foo.rs                        # tests for sibling foo.rs
```

## Anti-pattern: nested per-source `tests/` (do not use)

```text
src/command/
  skill.rs                        # WRONG: declares mod tests; here
  skill/
    tests/                        # WRONG: child of skill, not sibling
      mod.rs
      <topic>.rs
```

This nests the test module as a *child* of the source module. Children can read parent privates, so the structural "test only public" enforcement is lost — the convention then depends entirely on author discipline. Use the sibling layout shown above instead.

## Reference: `orbit-mcp::adapter`

The MCP `adapter` module has children `name_map.rs`, `schema.rs`, `dispatch.rs`, and `structured.rs`. The canonical layout: `adapter/mod.rs` declares each child plus `#[cfg(test)] mod tests;`, and `adapter/tests/` contains one file per source child (`tests/name_map.rs`, `tests/schema.rs`, etc.). MCP proxy/discovery tests live beside their implementations under `orbit-mcp/src/remote/tests/`; Registry's crate-level tests live under `orbit-registry/src/tests/`, including the workspace-registry persistence tests in `tests/workspace_registry.rs`. Each test file accesses its sibling source through the narrowest deliberate visibility.

## Migration recipe

For each source file with an inline `#[cfg(test)] mod tests { ... }`:

1. Identify the *parent* module — the directory the source file lives in.
2. If `<parent>/tests/` doesn't exist yet, create it with a `mod.rs`. Add `#[cfg(test)] mod tests;` to the parent's declaring file (`<parent>/mod.rs` or `<parent>.rs`) — once, not per source file.
3. Move the test body into `<parent>/tests/<source_filename>.rs`. Replace `use super::*;` with `use super::super::<source_module_name>::<item>;` — these must go through `pub` / `pub(crate)` / `pub(super)`.
4. Append `mod <source_filename_stem>;` to `<parent>/tests/mod.rs`.
5. Delete the inline block from the source file.
6. If any test required a truly private item, widen the item's visibility deliberately (with a comment explaining why) or restructure the test against a public seam — *don't* fall back to the nested anti-pattern above.
7. `cargo test -p <crate>` must pass.

For each sibling `*_tests.rs` file:

1. Move it to `<parent>/tests/<source_filename>.rs` (drop the `_tests` suffix; the directory says "tests" already).
2. Append `mod <source_filename_stem>;` to `<parent>/tests/mod.rs` (create the dir and the parent's `#[cfg(test)] mod tests;` declaration if absent).
3. Update imports: `use super::*;` becomes `use super::super::<source_module_name>::<item>;`.
4. `cargo test -p <crate>` must pass.

For modules already migrated to the *nested* anti-pattern (`<file>/tests/<topic>.rs`):

1. For each `<file>/tests/<topic>.rs`, fold the contents into a single `<parent>/tests/<file>.rs` (one file per source, not one per concern).
2. Delete the `<file>/tests/` directory entirely and remove `#[cfg(test)] mod tests;` from `<file>.rs`.
3. Ensure `<parent>/mod.rs` declares `#[cfg(test)] mod tests;` once for the whole parent module.
4. Update imports through the sibling path (`use super::super::<file>::<item>;`).
5. `cargo test -p <crate>` must pass.
