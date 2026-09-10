---
type: runbook
summary: Opt in, measure, and remove the host Rust compiler cache shared across Orbit worker worktrees, and validate before/after builds with it off.
tags: [operations, rust, cache, worktrees, sandbox, linux]
paths: ["scripts/rustc-compiler-cache.sh", "scripts/compiler-cache.sh", "scripts/test-compiler-cache.sh", "scripts/test-compiler-cache-namespaces.sh", "scripts/bench-compiler-cache.sh", "scripts/cross-revision-check.sh", "scripts/test-cross-revision-check.sh", ".cargo/config.toml"]
related_features: [policy-sandbox, executors]
related_artifacts: [ORB-11259, ORB-11755, ORB-11981]
last_validated: 2026-09-10
---

# Share Rust dependency compilation across worker worktrees

Use this runbook when managed worktrees recompile the same crates (`tokio`,
`regex_automata`, and friends) into a private `target/` on every task, or when
enabling, checking, or removing the opt-in host compiler cache.

The cache is **opt-in at the host**: the repository ships a rustc wrapper that
falls back to ordinary `rustc` whenever sccache is missing or the cache
directory is not writable. It does **not** share `CARGO_TARGET_DIR`. Per-worktree
test binaries and dirty-source fingerprints stay private. Do not enable a
global host cache until `scripts/test-compiler-cache-namespaces.sh` passes on
a host that can create Linux mount namespaces.

## Safety

- Do not point workers at one shared mutable Cargo target directory. That
  serializes `cargo` on `target/.cargo-lock`.
- Do not install system packages unless the host has no other way to obtain
  `sccache`. `scripts/compiler-cache.sh setup --install` downloads a pinned
  user-level binary into `$HOME/.orbit/cache/bin`.
- Do not delete `$HOME/.orbit/cache/compiler` while workers are compiling.
- Do not change live worker processes; new cargo invocations pick up the
  committed wrapper after the branch is present in their worktree.
- Do not share one TCP sccache daemon across Linux mount namespaces that bind
  different sources at `/tmp/orbit-workspace`. That daemon compiles through
  the first namespace's mounts and the second fails with a missing rlib.
  The wrapper defaults prevent this (see [Daemon ownership](#daemon-ownership)).

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

The provider **agent cwd stays on the real worktree**. Bubblewrap `--chdir` is
not mapped onto `/tmp/orbit-workspace`. The rustc wrapper, and only the rustc
wrapper, remaps *compiler* cwd onto the stable source mount so sccache hashes a
stable cwd (sccache includes rustc cwd in the rustc cache key and embeds it in
rlibs). A nested suffix is preserved: rustc invoked from
`<worktree>/crates/orbit-types` lands in `/tmp/orbit-workspace/crates/orbit-types`,
so relative `src/lib.rs` keeps the same referent. A cwd outside the checkout is
not moved onto the repo root. Relative compiler args stay relative; they are
not expanded back to the unique worktree prefix.

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
hits **and** a wall-clock drop versus the first worktree's cold fill. Hits with
no wall-clock improvement are not evidence of a win. Cache cold fill is often
*slower* than an uncached baseline because of wrapper/daemon overhead; do not
advertise a blanket speedup.

## Setup

Host-side, outside a managed implementer sandbox (those workers must not write
the live `~/.orbit/cache` from a task):

```bash
scripts/compiler-cache.sh setup --install
```

This creates `$HOME/.orbit/cache/compiler` and, with `--install`, fetches pinned
sccache `v0.17.0` into `$HOME/.orbit/cache/bin`. No apt packages. The committed
wrapper looks there before `PATH`. The binary lives beside the cache data, not
inside `SCCACHE_DIR`.

Confirm the Linux implementer profile can write the cache directory, then run
the namespace regression on a host that can create user namespaces (not nested
inside an existing Bubblewrap sandbox):

```bash
scripts/test-compiler-cache.sh
ORBIT_COMPILER_CACHE_REQUIRE_NS=1 scripts/test-compiler-cache-namespaces.sh
```

Only after that regression passes should the orchestrator leave the host cache
directory writable for workers.

Optional host overrides (passed through `[execution.env].pass` only if you set
them on the Orbit process):

| Variable | Default |
| --- | --- |
| `SCCACHE_DIR` | `$HOME/.orbit/cache/compiler` |
| `SCCACHE_CACHE_SIZE` | `5G` (sccache LRU) |
| `ORBIT_COMPILER_CACHE_BIN` | `$HOME/.orbit/cache/bin/sccache`, else `sccache` on `PATH` |
| `ORBIT_COMPILER_CACHE=0` | Force ordinary rustc (practical opt-out) |
| `SCCACHE_CLIENT_SIDE` | Wrapper default `1`: compile in the rustc client; daemon is cache storage only |
| `SCCACHE_SERVER_UDS` | Wrapper default `/tmp/orbit-sccache.sock` (private per Linux sandbox `/tmp`) |
| `SCCACHE_SERVER_PORT` | Unset. Setting it disables the default UDS and uses TCP on the shared net |
| `SCCACHE_BASEDIRS` | For the pinned sccache `v0.17.0`, inert for rustc; Rust path normalization comes from the wrapper's `STABLE_SRC`/`STABLE_TGT` argv/env rewrite and rustc cwd chdir when Linux Bubblewrap stable mounts alias the checkout |
| `CARGO_INCREMENTAL=0` | Required for cacheable rustc; sccache `cannot_cache` incremental invocations |
| `CARGO_BUILD_JOBS` | Bound concurrency; primary mitigation for Clippy, which sccache does not cache |

## Daemon ownership

sccache 0.17.0 defaults to a background daemon on `127.0.0.1:4226`. Managed
Linux sandboxes use `--share-net`, so two worktrees would otherwise share that
daemon. The daemon then runs rustc against *its* `/tmp/orbit-workspace` bind.
A second namespace with different source contents fails with
`extern location /tmp/orbit-workspace/target/debug/deps/lib…rlib does not exist`.

The wrapper's default ownership model:

1. `SCCACHE_CLIENT_SIDE=1` — rustc runs in the client process (the calling
   namespace). The daemon is only a disk-cache gateway.
2. `SCCACHE_SERVER_UDS=/tmp/orbit-sccache.sock` — each sandbox's private `/tmp`
   tmpfs gets its own daemon. Disk cache (`SCCACHE_DIR`) stays shared.

Honor explicit operator overrides, including `SCCACHE_CLIENT_SIDE=0` plus a
shared `SCCACHE_SERVER_PORT`, which is how
`scripts/test-compiler-cache-namespaces.sh` regresses the missing-artifact
failure. Do not use that combination on workers.

## Custom `CARGO_TARGET_DIR`

Jobs may set a private target directory that is **not** `<worktree>/target`, so
it is not bind-mounted at `/tmp/orbit-build`. The wrapper used to require
*both* source and target aliases before rewriting anything; that silently
left unique worktree prefixes in argv, `CARGO_*` env, and cwd, so sccache
missed.

The wrapper now rewrites source paths and rustc cwd whenever
`/tmp/orbit-workspace/Cargo.toml` aliases the checkout, even if the custom
target is not aliased. `--out-dir` is excluded from sccache's rustc key, so
reuse still works; outputs stay in each private target. Relative compiler
inputs are left relative.

If you need `--out-dir` itself to use the stable build mount (debuginfo paths),
bind that custom target at `/tmp/orbit-build` inside the sandbox. The
namespace test and bench do this for their private targets. The Linux
Bubblewrap planner still binds `<cwd>/target` by default and does **not**
remap the provider agent cwd.

## When sccache is ineffective or slower

| Workload | What sccache 0.17.0 does | What to do |
| --- | --- | --- |
| `cargo clippy` analysis (`clippy-driver`) | Tiny-fixture smoke: 0 cacheable compile requests; reasons include `missing output_dir` and `multiple input files`. That is the clippy-driver invocation, not every rustc the phase may spawn. | Do not advertise `cargo check` hits as caching clippy-driver itself. rustc dependency compilations on a representative graph may still reuse the disk cache during a Clippy run. Keep bounded `CARGO_BUILD_JOBS`. Opt out with `ORBIT_COMPILER_CACHE=0` if wrapper overhead is noise. |
| Incremental (`CARGO_INCREMENTAL=1` / `-C incremental`) | Pinned sccache `0.17.0` **rejects** the invocation (`incremental compilation is prohibited: Unset CARGO_INCREMENTAL to continue`) instead of compiling uncached. | Keep `CARGO_INCREMENTAL=0` on workers. To run incremental rustc, set `ORBIT_COMPILER_CACHE=0`. |
| Cache cold fill | Extra wrapper/daemon/hash work; measured slower than uncached `ORBIT_COMPILER_CACHE=0` on the same crate | Expected. Judge effectiveness on a *warm* second worktree vs uncached, not vs cold fill. |
| Tiny crates | Daemon startup can dominate wall time | Hits without a wall-clock drop are not a win. |
| Crate types other than rlib/staticlib (bins, dylibs, proc-macros) | Non-cacheable `crate-type` | Dependency rlibs can still hit; the final bin still compiles. |

`/usr/bin/time` user/sys on the cargo process tree excludes a detached cache
daemon's CPU. With the wrapper default `SCCACHE_CLIENT_SIDE=1`, compiler CPU
stays in the client tree. Do not advertise command-tree CPU as total compiler
CPU when using server-side mode.

Practical opt-out, no uninstall: `ORBIT_COMPILER_CACHE=0`.

## Cross-revision before/after validation

Comparing two revisions is the one workflow where the cache must be **off**, and
where reused build state is the main source of false evidence. Recorded
observations, all of them ordinary consequences of reusing state across source
trees rather than defects in Cargo or sccache:

- An archived baseline compiled under the configured rustc wrapper produced a
  test binary listing the *current* worktree's newly added tests.
- Two extracts sharing one `CARGO_TARGET_DIR` ran the first extract's embedded
  fixture paths, so the second arm never exercised the revision it named.
- `git archive` and `tar` write the commit's timestamps, so an extract placed
  beside an existing build can look up to date and skip the rebuild.
- A build piped into `tail` reported `tail`'s exit status, turning a compile
  error into a green result.

Use the maintained helper instead of hand-rolling `git archive | tar -x`:

```bash
scripts/cross-revision-check.sh \
  --baseline <pre-fix-sha> --candidate <post-fix-sha> \
  --expect-baseline fail --expect-candidate pass \
  --baseline-marker 'test result: FAILED' \
  --candidate-marker 'test result: ok' \
  -- cargo test -p orbit-core --lib
```

| Guarantee | False result it prevents |
| --- | --- |
| A scratch extract and a private `CARGO_TARGET_DIR` per arm | An arm reusing the sibling revision's build output or embedded fixture paths |
| Reused `--workdir` arm trees and targets are reset at the start of each invocation | Files or build artifacts from an earlier revision surviving into a rerun |
| Every extracted mtime reset to now | A build skipped because archive timestamps predate an existing target dir |
| `ORBIT_COMPILER_CACHE=0`, empty `RUSTC_WRAPPER` / `CARGO_BUILD_RUSTC_WRAPPER`, `CARGO_INCREMENTAL=0` | A baseline compiled through the host cache picking up the other tree's artifacts |
| Producer status captured from a redirect, then `--tail` reads the log file | A filter's success replacing the build's failure |
| `--expect-baseline` / `--expect-candidate` | An unexpected failure reported as an ordinary arm result |
| Per-arm marker required, sibling marker rejected | A stale or foreign test set passing as this revision's |
| Read-only Git access (`rev-parse`, `archive`, `GIT_OPTIONAL_LOCKS=0`) | Writes to a managed read-only `.git`, or a mutated source checkout |
| `--workdir` refused inside the checkout or under `.orbit/` | Scratch trees landing in the state being validated |

Limits worth stating in a validation summary:

- Marker verification is a literal substring match on each arm's log. A marker
  both revisions can print proves nothing; pick text only one arm can emit.
- The helper compares whatever the two revisions contain. It does not establish
  that they differ only in the change under test.
- Uncached arms are slower than a warm worktree build. That is the cost of an
  independent baseline, not a regression; do not re-enable the cache for the
  baseline arm to speed it up. `--keep-compiler-cache` exists for deliberate
  cache experiments, not for before/after evidence.
- Arm wall time is not a cache measurement. Use
  [`scripts/bench-compiler-cache.sh`](#measure) for that.
- The helper cannot recover a status you discard inside your own pipeline. Pass
  the producer directly and let `--tail` do the bounding.

Regression fixtures for the helper — arm isolation, mtime normalization, cache
opt-out, producer exit code through bounded output, read-only `.git`, and
workdir containment — run in CI and locally:

```bash
make cross-revision-check-test
# equivalent: scripts/test-cross-revision-check.sh
```

## Measure

Same toolchain, unchanged `HEAD`, private target directories, equivalent
command (`cargo check -p orbit-types --offline --locked` by default):

```bash
# Isolated bench (downloads pinned sccache into the bench workdir if needed;
# does not write ~/.orbit/cache unless SCCACHE_DIR points there):
scripts/bench-compiler-cache.sh

# On a host that can create user namespaces, also require the concurrent test:
ORBIT_COMPILER_CACHE_REQUIRE_NS=1 scripts/bench-compiler-cache.sh
```

The harness records sequential no-cache cold builds, a cache cold fill, a cache
warm second worktree, Clippy and incremental probes, a real-mount custom-target
pair when `/tmp/orbit-workspace` aliases the checkout, and concurrent
different-source namespaces when Bubblewrap can create them. It never reuses
one `CARGO_TARGET_DIR`. Nested implementer sandboxes cannot create a second
user namespace; that skip is not a pass. Re-run the namespace script on the
host.

## Invalidation

sccache keys on compiler identity, flags, hashed sources, `CARGO_*` env
(with a few exclusions), and **rustc cwd**. Expected misses:

- source edits in the crate being compiled
- toolchain upgrades (`rustc --version` changes)
- feature / flag / profile changes (`--release`, extra `--features`, `RUSTFLAGS`)
- original worktree cwd without the wrapper's stable-source chdir
- incremental rustc (`-C incremental`)
- Clippy-driver invocations

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
