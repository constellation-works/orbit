# orbit-cmd

Command groups that need to compose Core runtime operations with Registry state live here. `orbit-registry` sits below `orbit-core`; Core uses it for workspace catalog reads and publication-success bookkeeping writes. Consumed by `orbit-cli` and `orbit-web`.

## Rules

- Flat layout: one file per command group, each closing exactly one composition seam named in its module doc. No grouping directories mirroring `--help` sections.
- Return a `Core`, never the runtime: no item names `OrbitRuntime`, which `orbit-core` does not export. Compose a workspace through `orbit_core::app::workspace::Opening`. Kernel items this crate implements or resolves (the workspace catalog, root resolution) come from `orbit-runtime` directly. If a seam is missing, add it in Core deliberately, then consume it here. Never re-implement a Core rule.
- No `clap`, no `axum`. Return plain result structs both surfaces can render. Logic needed by Core's own runtime internals belongs in `orbit-core::adapter::command`.
- [`assets/agent-rules.md`](assets/agent-rules.md) is the `--inject-agent-rules` block; its markers live in the asset, so edit the asset, not marker handling.
- No crate-root `tests/`; end-to-end coverage, and tests of this crate's exported entry points, live in the CLI's integration tests.
