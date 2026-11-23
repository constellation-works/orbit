# orbit-types

Shared data contracts: structs, enums, serde shapes, pure constructors, lifecycle predicates, narrow domain errors. Nothing else.

## Rules

- Zero Orbit dependencies; no fs, process, env, database, network, or tracing. Behavior that needs those goes to `orbit-common`; only the shape lives here.
- Every item lives under exactly one domain module; `OrbitId` is the only crate-root primitive. What each module holds: [architecture page](../../docs/architecture/orbit-types.md#domain-modules).
- Module shape: private submodules, `mod.rs` = declarations + `pub use` exports (the exports define the public surface); use per-leaf globs for complete passthroughs and explicit lists for partial exports, one `error.rs` per domain with a `thiserror` enum. `OrbitError` belongs to `orbit-common`, never here.
- Serde shapes are persisted contract (bundles, YAML, SQLite, MCP wire). A field rename or `#[serde]` change is a data migration — check `orbit-store` readers and keep `*_SCHEMA_VERSION` guards in step. Tier **stable** in [`ARCHITECTURE.md`](../../ARCHITECTURE.md).
- The `clap` feature is `ValueEnum` derives only — no `Args`/`Parser`, no help text. A default build must not link clap.
