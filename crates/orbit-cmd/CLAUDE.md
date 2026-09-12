# orbit-cmd

Command groups that need both a Core runtime and Registry state (`orbit-registry` sits above `orbit-core`, so Core can't see the workspace catalog). Consumed by `orbit-cli` and `orbit-web`.

- Flat layout: one file per command group, each closing exactly one composition seam named in its module doc. No grouping directories mirroring `--help` sections.
- Runtime behavior is exposed as per-module `*Commands` extension traits re-exported through `prelude`; add new groups the same way.
- Pure consumer of `orbit_core::OrbitRuntime`'s public API. If a seam is missing, add it in Core deliberately, then consume it here. Never re-implement a Core rule.
- No `clap`, no `axum`. Return plain result structs both surfaces can render. Logic needed by Core's own runtime internals belongs in `orbit-core::adapter::command`.
- [`assets/agent-rules.md`](assets/agent-rules.md) is the `--inject-agent-rules` block; its markers live in the asset, so edit the asset, not marker handling.
- No crate-root `tests/`; end-to-end coverage lives in the CLI's integration tests.
