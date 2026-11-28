## Workspace-local Rust toolchains

`proc.spawn` refuses, before the child starts, a default-profile rustup install whose `RUSTUP_HOME` is inside the workspace: the default and complete profiles install rust-docs, whose `share/doc/rust/html/core/macro.env.html` matches denyModify `**/*.env.*` even under `.orbit/tmp/`. A validation command that needs a workspace-local toolchain uses an absolute preprovisioned minimal one, or installs it the way `scripts/codeql-rust-local.sh` does:

```bash
rustup toolchain install <version> --profile minimal --component rust-src --no-self-update
```

Letting `cargo` or a rustup proxy auto-install into a workspace `RUSTUP_HOME` uses the default profile and is refused. Existing toolchains and roots outside the workspace (`~/.rustup`) are untouched. A managed Linux run that still creates `macro.env.html`, `.env`, `.env.local` or `secrets.env` on a committable path fails the post-run guard. Under the run's `.orbit/tmp/` scratch, which commit never includes, the guard removes the match instead, logs a warning and records it as a denied modify in the run audit; a test fixture there must not depend on the file surviving the step.

## Toolchain (MSRV)

- **MSRV:** `rust-version` in `[workspace.package]` of the root `Cargo.toml`, checked with `cargo check --workspace --locked` by the `msrv` job in `.github/workflows/msrv.yml` (push to `agent-main` only; the full MSRV build still runs on push and no longer runs on pull requests) and by `make ci-fast`. Pull-request guardrails also reject `use super as <name>;` and `use super::{self as <name>, ...};`, which Rust 1.89 cannot resolve; use the module's full path. Keep the `MSRV` variables in `msrv.yml` and the Makefile equal to it.
- **Pinned toolchain:** [`rust-toolchain.toml`](../../rust-toolchain.toml); every non-MSRV CI step that installs Rust repeats its version, including the shard jobs' "Start the pinned toolchain install" step (a `rustup` command, not the toolchain action).

Bump the pinned toolchain in one PR: the toolchain file and those CI steps together, plus fixes for new compiler or Clippy diagnostics, with `make ci-fast` and `make ci-lint` passing on the new version. Leave the `msrv` job on the declared minimum. If a change needs a newer compiler or a dependency raises the MSRV, update `rust-version` and both `MSRV` variables in that same PR.

## Windows

Orbit supports macOS and Linux. On Windows the supported route is the Linux build inside [WSL2](../runbooks/windows-wsl2.md). Native Windows is not built or checked in CI.

- Code using Unix-only APIs (`libc`, `std::os::fd`, `std::os::unix`) sits behind `#[cfg(unix)]`; its `#[cfg(not(unix))]` counterpart returns an explicit unsupported error or `Unknown` and never invents Windows semantics.
- Tests that need those APIs or drive a `#!/bin/sh` fake are gated `#[cfg(unix)]` at the narrowest level (test, module, or `#![cfg(unix)]` for a POSIX-only file). Gate them; never delete them.

## Supply-chain (cargo-deny)

[`cargo-deny`](https://embarkstudios.github.io/cargo-deny/) checks two independent Cargo workspaces using the shared [`deny.toml`](../../deny.toml). The main PR job runs all checks on the root graph through `scripts/ci-guardrails.sh`: advisories, yanked versions, licenses and sources. That command does **not** cover `desktop/Cargo.lock`. The **Desktop advisories** job in [`desktop.yml`](../../.github/workflows/desktop.yml) separately runs the advisory check against `desktop/Cargo.toml` with `--locked`, including native default dependencies and all target platforms. It fails if cargo-deny is missing. Both lanes pin cargo-deny 0.19.9; the desktop lane never adds a native build to the main Check job's 400-second budget. Run both commands from the repository root before landing a dependency change:

```bash
cargo install cargo-deny --version 0.19.9 --locked   # once
make audit                                         # root graph, all checks
scripts/cargo-deny.sh --manifest-path desktop/Cargo.toml --locked check advisories
python3 scripts/test-desktop-advisories.py           # requires PyYAML
```

- **New license:** add its SPDX identifier to `[licenses].allow` only if it is permissive or public-domain-equivalent, with a comment naming the crate(s) and, for weak copyleft such as MPL-2.0, a justification. Copyleft that would bind Orbit's own sources is never added; replace the dependency.
- **Advisory exception:** only when no patched release exists (otherwise bump the dependency), as an individually reviewed `[advisories].ignore` object with `id` (`RUSTSEC-YYYY-NNNN`) and a `reason` documenting Orbit's exposure, exact dependency chain, upstream constraint, re-review owner and a `Re-review YYYY-MM-DD` date, about six months out. Re-review by that date and remove the entry when a fix lands. Ignores apply by ID across both graphs, not by package version or workspace: review any new occurrence outside the documented chain. For `advisory-not-detected`, confirm the ID no longer matches **either** workspace before retiring the entry, then rerun both commands. Desktop-only exceptions naturally produce that warning on the root graph. A reintroduced advisory is reviewed afresh; do not allow all unmaintained crates or future unknown IDs.
- **Read-only advisory database:** set `CARGO_DENY_DB_PATH` (or `ORBIT_CARGO_DENY_DB_PATH`) to a writable directory; `scripts/cargo-deny.sh` points `[advisories].db-path` there. The invoking runner cleans it up. Sandboxed agent dispatches whose profile grants scratch writes get this automatically: dispatch creates a per-run directory under the run's scratch dir and injects `CARGO_DENY_DB_PATH` into the child environment, so no manual setting is needed there. Read-only profiles get no injection and still fail closed.
- **Offline:** set `CARGO_DENY_DISABLE_FETCH=1` (or `ORBIT_CARGO_DENY_DISABLE_FETCH`, `CARGO_DENY_OFFLINE`, `ORBIT_CARGO_DENY_OFFLINE`) to check against a local clone of [`rustsec/advisory-db`](https://github.com/rustsec/advisory-db) (default directory `advisory-db-3157b0e258782691`) or the environment's fixture. Missing or stale data fails; it never passes by skipping.
- **YAML parser:** the `serde_yaml` dependency key aliases [`yaml_serde`](https://github.com/yaml/yaml-serde) 0.10.7, the YAML organization's continuation of the archived `serde-yaml`, with the same codecs, `Value`/`Mapping` API and error types, so persisted formats are unchanged. Its backend, [`libyaml-rs`](https://github.com/yaml/libyaml-rs) 0.3.0, is still a C-to-unsafe-Rust translation of libyaml; review upstream activity on upgrades. (`serde_yaml_ng` and `serde_norway` were staler; `serde-saphyr` lacks the compatible `Value` API.) After changing the parser, run `cargo test --locked -p orbit-types --lib` and `cargo test --locked -p orbit-tools --tests`.

The desktop maintenance exceptions were assessed on 2026-10-19. **Orbit desktop maintainers (dependency/CI PR reviewers)** own re-review by **2027-04-19**, including checking published parent manifests and removing each exception as soon as its chain can migrate. These advisories report unmaintained packages, not demonstrated exploitation. The three zbus findings describe one upstream consolidation into zbus 6. Each ID still has its own disposition in `deny.toml`:

| Advisory | Committed desktop chain and migration constraint |
| --- | --- |
| [RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html) | `orbit-desktop -> tauri 2.12.4 -> gtk 0.18.2 -> gtk3-macros 0.18.2 -> proc-macro-error 1.0.4`, and `gtk -> glib 0.18.5 -> glib-macros 0.18.5 -> proc-macro-error`. Build-time macro diagnostics; [Tauri 2.12.4](https://docs.rs/crate/tauri/2.12.4/source/Cargo.toml) still requires `gtk ^0.18`, so the newer GTK 0.19 line is incompatible. |
| [RUSTSEC-2026-0371](https://rustsec.org/advisories/RUSTSEC-2026-0371.html) | `orbit-desktop -> notify-rust 4.18.2 / tauri-plugin-opener 2.7.2 -> zbus 5.19.0 -> zbus_names 4.3.5` (also via `zbus_macros 5.19.0`). Names moved into zbus 6. |
| [RUSTSEC-2026-0370](https://rustsec.org/advisories/RUSTSEC-2026-0370.html) | Same parents and zbus version -> `zvariant 5.15.1` (also via `zbus_macros` and `zbus_names`). Wire format moved into zbus 6. |
| [RUSTSEC-2026-0372](https://rustsec.org/advisories/RUSTSEC-2026-0372.html) | Same parents and zbus version -> `zvariant 5.15.1 -> zvariant_derive 5.15.1`. Derives moved into zbus 6. |

The latest published [notify-rust 4.18.2](https://docs.rs/crate/notify-rust/4.18.2/source/Cargo.toml) and [tauri-plugin-opener 2.7.2](https://docs.rs/crate/tauri-plugin-opener/2.7.2/source/Cargo.toml) still require `zbus ^5` and `^5.9` respectively; substituting zbus 6 needs upstream API migrations. No patched standalone version is listed for these four advisories. Until the parents migrate, accept the documented maintenance risk with exact-ID exceptions. The regression executes the workflow's real audit command with real Cargo and cargo-deny against tiny independent workspaces and an offline fixture advisory database: the root-only command misses a desktop-only finding, the desktop command fails, an exact-ID exception passes, and another unknown ID still fails. It also checks that a stale desktop lockfile fails without being rewritten. Only the local fixture dependency's metadata source is adapted to registry form so no crate download or native build is needed.

The workspace chrono exceptions were assessed on 2026-11-18. **Orbit dependency maintainers** own re-review by **2027-02-17** and removal once the workspace migration completes. Both advisories report unmaintained crates, and cargo-deny finds no safe upgrade. The exact dependency chains covered by the exceptions in the root workspace graph are:

| Advisory | Dependency chain and constraint |
| --- | --- |
| [RUSTSEC-2026-0543](https://rustsec.org/advisories/RUSTSEC-2026-0543.html) | Root: Orbit crates use `chrono 0.4.43` directly; `rmcp` and `schemars` also depend on it as normal dependencies (`rusqlite` does not enable it, since its `chrono` feature is off); `orbit-automation -> cel 0.15.0 -> chrono` also retains it. `croner` is built without its `chrono` feature, so it adds no chrono edge. All chrono versions are affected, so there is no patched release to select. |
| [RUSTSEC-2026-0555](https://rustsec.org/advisories/RUSTSEC-2026-0555.html) | `orbit-automation -> cel 0.15.0 -> chrono-tz 0.10.4` (the `cel` `chrono` feature behind CEL temporal values) is the only chain; the scheduling test fixtures evaluate DST in host-local time instead. No safe chrono-tz upgrade is available; removal requires migrating CEL temporal evaluation. |

## Vendored dashboard JavaScript

DOMPurify and marked are vendored under [`crates/orbit-web/assets/dashboard/vendor/`](../../crates/orbit-web/assets/dashboard/vendor/); [`VENDOR.md`](../../crates/orbit-web/assets/dashboard/vendor/VENDOR.md) has the refresh procedure. Refresh with `./scripts/refresh-dashboard-vendor.sh` after changing the pins in `package.json`; never edit the blobs by hand. `make ci-fast` runs [`check-dashboard-vendor.py`](../../scripts/check-dashboard-vendor.py), which fails on a digest mismatch or drift between the manifest, `package.json` and `package-lock.json`. Dependabot and GitHub security alerts watch these packages, and the `dependabot-alert-sweep` job files remediation tasks; `cargo-deny` does not see them.

### Leaf step diagnostics

`orbit run show` exposes `lock_wait_ms` on step records and displays positive
values in its table and step detail. Timing sums advisory file-lock acquisition
(including turnstile waits and failed acquisition) and the store writer mutex
waits inside the synchronous leaf, across retries and recovery. Each leaf owns
its own scope: parallel workers and nested steps do not borrow another step's
waits. Child processes and SQLite's internal busy-handler waits are excluded.
Historical records have unknown (`null`) timing; zero means the scope observed
less than one millisecond of acquisition wait. The completion audit event
retains timing even when the leaf fails, and terminal worker summaries copy it
into stored step rows.

A stopped `git_rebase` (the delivery pipeline's `sync_base`) records
`conflicting_paths` and `conflicting_files` in its checkpoint output before
recovery, and supplies both to the recovery leaf. Each file carries
`deterministically_resolvable`: true only when unmerged index object IDs and
modes prove identical sides or a side unchanged from the base, with both ours
and theirs present. A missing ours or theirs stage is always false, including
rename/delete conflicts; a missing base stage is true only for identical
add/add sides. Distinct edits and malformed evidence are conservatively false. This is diagnostic;
recovery still validates and resolves the stopped rebase under existing rules.
On resume, an incomplete step retains only these two diagnostic fields outside
the usable pipeline until it runs successfully. A new conflict supersedes them,
including an explicit empty list, and that supersession survives final-recovery
invalidation of the step's pipeline entry. Diagnostics neither skip execution
nor expose failed outputs to templates.
