---
type: runbook
summary: Opt in, measure, and remove the host Rust compiler cache shared across Orbit worker worktrees.
tags: [operations, rust, cache, worktrees, sandbox, linux]
paths: ["Makefile", "scripts/rustc-compiler-cache.sh", "scripts/compiler-cache.sh", ".cargo/config.toml"]
related_features: [policy-sandbox, executors]
related_artifacts: [ORB-11259]
last_validated: 2026-09-05
---

# Share Rust dependency compilation across worker worktrees

Use this runbook when managed worktrees recompile the same crates (`tokio`,
`regex_automata`, and friends) into a private `target/` on every task, or when
enabling, checking, or removing the opt-in host compiler cache.

The cache is **opt-in at the host**: the repository ships a rustc wrapper that
falls back to ordinary `rustc` whenever sccache is missing or the cache
directory is not writable. It does **not** share `CARGO_TARGET_DIR`. Per-worktree
test binaries and dirty-source fingerprints stay private.

## Safety

- Do not point workers at one shared mutable Cargo target directory. That
  serializes `cargo` on `target/.cargo-lock`.
- Do not install system packages unless the host has no other way to obtain
  `sccache`. `scripts/compiler-cache.sh setup --install` downloads a pinned
  user-level binary into `$HOME/.orbit/cache/bin`.
- Do not delete `$HOME/.orbit/cache/compiler` while workers are compiling.
- Do not change live worker processes; new cargo invocations pick up the
  committed wrapper after the branch is present in their worktree.

## Ownership and location

| Object | Owner | Path |
| --- | --- | --- |
| Host cache seam | Orbit global registry (`~/.orbit`), language-neutral | `$HOME/.orbit/cache/` |
| Rust compiler cache | Operator opt-in via sccache | `$HOME/.orbit/cache/compiler/` |
| rustc wrapper | Repository | `scripts/rustc-compiler-cache.sh` |
| Cargo hook | Repository | `.cargo/config.toml` (`build.rustc-wrapper`) |
| Per-worktree build output | Each managed worktree | `$CARGO_TARGET_DIR` or `<worktree>/target/` |

Linux implementer sandboxes grant `$HOME/.orbit/cache` as a narrow extra write
root. Managed worktrees also bind the checkout at `/tmp/orbit-workspace` and
`<worktree>/target` at `/tmp/orbit-build` inside the private mount namespace so
sccache keys do not include the `jrun-*` path. Workspace `.orbit/**`
protected-path denies are unchanged. Reviewer and other read-only profiles do
not receive the cache grant. macOS already allows `$HOME/Library/Caches` and
also grants `$HOME/.orbit/cache/**` so the same default directory works. The
stable `/tmp` mounts are Linux Bubblewrap-only.

Unavailable cache (missing binary, unwritable directory, `ORBIT_COMPILER_CACHE=0`)
execs `rustc` with the original argv. Compilation stays correct; it is just
uncached.

## Inspect

```bash
make compiler-cache-status
# equivalent: scripts/compiler-cache.sh status
```

Treat the cache as effective only when `effective: enabled` and a subsequent
`cargo` build reports sccache hits:

```bash
# after any cargo check/build/clippy in a worktree
sccache --show-stats
# or, if using the setup-installed binary:
$HOME/.orbit/cache/bin/sccache --show-stats
```

A warm second worktree against unchanged source should show compile-request
hits and a wall-clock drop versus the first worktree's cold fill. Hits with no
wall-clock improvement are not evidence of a win.

## Setup

```bash
scripts/compiler-cache.sh setup --install
```

This creates `$HOME/.orbit/cache/compiler` and, with `--install`, fetches pinned
sccache `v0.17.0` into `$HOME/.orbit/cache/bin`. No apt packages. The committed
wrapper looks there before `PATH`. The binary lives beside the cache data, not
inside `SCCACHE_DIR`.

Optional host overrides (passed through `[execution.env].pass` only if you set
them on the Orbit process):

| Variable | Default |
| --- | --- |
| `SCCACHE_DIR` | `$HOME/.orbit/cache/compiler` |
| `SCCACHE_CACHE_SIZE` | `5G` (sccache LRU) |
| `ORBIT_COMPILER_CACHE_BIN` | `$HOME/.orbit/cache/bin/sccache`, else `sccache` on `PATH` |
| `ORBIT_COMPILER_CACHE=0` | Force ordinary rustc |
| `SCCACHE_BASEDIRS` | For the pinned sccache `v0.17.0`, inert for rustc; Rust path normalization comes from the wrapper's `STABLE_SRC`/`STABLE_TGT` argv and environment rewrite when Linux Bubblewrap stable mounts alias the checkout and target |
| `CARGO_INCREMENTAL=0` | Recommended for workers; sccache cannot cache rustc incremental invocations |

After upgrading the Orbit binary that contains the Linux cache grant, new
implementer runs can write the directory. Until then the wrapper sees an
unwritable path and falls back.

## Measure

Same toolchain, unchanged `HEAD`, private target directories, equivalent
command (`cargo check -p orbit-types --offline` by default):

```bash
scripts/compiler-cache.sh setup --install   # once per host
make compiler-cache-bench
```

The harness records sequential no-cache cold builds, a cache cold fill, a cache
warm second worktree, and a concurrent pair. It never reuses one `CARGO_TARGET_DIR`.

## Invalidation

sccache keys on compiler identity, flags, and source inputs. Expected misses:

- source edits in the crate being compiled
- toolchain upgrades (`rustc --version` changes)
- feature / flag / profile changes (`--release`, extra `--features`, `RUSTFLAGS`)

The second worktree's private `target/` still rebuilds crates whose fingerprints
changed; only identical rustc invocations hit.

## Removal

```bash
scripts/compiler-cache.sh remove --yes
```

Deletes `$HOME/.orbit/cache/compiler` only. The wrapper remains in the
repository and keeps falling back to `rustc`. To ignore a still-present cache
without deleting it: `ORBIT_COMPILER_CACHE=0`.

## Sandbox

Protected-path rules stay enforced: workspace `.orbit/**` (except the existing
auto_tasks/routines/config/resources exceptions), `**/.env`, and dotenv globs.
The cache grant is the host global `cache/` directory, not a workspace
`.orbit/state` path and not a shared target dir.

Verify a Linux implementer profile can write the cache without widening
reviewer:

```bash
orbit policy check implementer "$HOME/.orbit/cache/compiler"
```

`orbit policy check` is workspace-relative and will not name the host global
path; the grant is applied by the Linux runtime write-root appender at spawn.
Confirm empirically with `make compiler-cache-status` inside a managed
implementer run, or with a Bubblewrap smoke that bind-mounts the cache
directory writable and leaves `/tmp` private.

## Related references

- [Prepare a Linux Host for Sandboxed Dispatch](./linux-sandbox.md)
- [Policy & Sandboxing — Design](../design/policy-sandbox/2_design.md)
- [Configuration](../CONFIG.md) (`[execution.env].pass`)
