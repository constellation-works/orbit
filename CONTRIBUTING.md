# Orbit Internal Development Standards

Orbit does not accept external pull requests. Development follows a
single-writer pipeline that executes on a trusted host; running submitted code
from an untrusted branch there would expose the maintainer's credentials.
Unsolicited pull requests will be closed.

GitHub issues remain open for bug reports and feature requests. Outside input
reaches Orbit through issues; implementation is handled by the maintainer and
Orbit's own agents.

## Principles

- Prefer simple, coherent designs over preserving accidental complexity.
- Fix root causes when practical, not just symptoms.
- Keep command, engine, executor, store, and type boundaries clean.
- Treat agent and human experience as product concerns, not just implementation details.

## Setup

```bash
cargo test --workspace
```

Use targeted tests while iterating, then run the full workspace suite before
landing an internal change.

## Safe Mutable CLI Fixtures

Test fixtures and manual reproductions that mutate Orbit task, run, workspace,
or registry state must be isolated from the process that launches them. This is
separate from authorized operator or production CLI work, which should retain
normal Orbit routing and state.

Use absolute disposable paths and spawn the CLI as a child. The shared helper
clears the managed-run routing, identity, and grant variables before the
fixture sets its own `HOME` and `USERPROFILE`:

```rust
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;

fn fixture_orbit(work: &Path, home: &Path) -> Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}
```

The pattern above is already used by
[`crates/orbit-cli/tests/tool_list.rs`](crates/orbit-cli/tests/tool_list.rs).
For a complete disposable fixture, create absolute temporary paths, initialize
the fixture workspace through that helper, then perform a read-only routing
check before adding tasks or starting runs:

```rust
let temp = tempfile::tempdir().expect("fixture tempdir");
let home = temp.path().join("home");
let work = temp.path().join("work");
fs::create_dir_all(&home).expect("fixture home");
fs::create_dir_all(&work).expect("fixture work");

let expected_work = fs::canonicalize(&work).expect("canonicalize fixture work");

fixture_orbit(&work, &home)
    .args(["workspace", "init", "--name", "fixture"])
    .assert()
    .success();

let report = fixture_orbit(&work, &home)
    .args(["workspace", "show", "--format", "json"])
    .output()
    .expect("workspace routing check");
assert!(report.status.success());
let report: serde_json::Value = serde_json::from_slice(&report.stdout).expect("routing JSON");
assert_eq!(report["registered"], true);
assert_eq!(
    report["checkout"]["repo_root"],
    expected_work.to_string_lossy().to_string()
);
assert_eq!(
    report["checkout"]["orbit_dir"],
    expected_work.join(".orbit").to_string_lossy().to_string()
);
```

`workspace show` exposes the physical checkout paths resolved by the child, so
canonicalize the fixture path before comparing it. This check catches a fixture
routed to an ambient workspace before the fixture performs its useful mutation.
A shell `export HOME=/tmp/...` is not an isolation boundary: a
managed child can inherit `ORBIT_MANAGED_RUN_CONTEXT` and the
`ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair, which carries durable authority
and takes precedence over home discovery. Never use bare mutable fixture CLI
commands against ambient authority in a managed worker. See
[`crates/orbit-cli/tests/ambient_authority_isolation.rs`](crates/orbit-cli/tests/ambient_authority_isolation.rs)
for the regression coverage.

Live `bwrap` spawn is not available from inside an agent-executor or job-run
worktree. The outer sandbox blocks nested `unshare(CLONE_NEWUSER)`, so even
`bwrap --ro-bind / / --tmpfs /tmp --dev /dev -- echo works` fails with
`No permissions to create new namespace`. Treat that denial as nested-sandbox
environment, not a missing AppArmor profile or a product defect. Replay live
spawn checks (`spawn_under_linux_bwrap`, `--run-ignored`) on the owning Linux
host. Do not disable `linux-bwrap` or try to make bwrap nest from a fixture.

## Toolchain (MSRV)

Orbit's minimum supported Rust version is declared as `rust-version` in the
workspace `Cargo.toml` (`[workspace.package]`) and enforced by the `msrv` job
in `.github/workflows/ci.yml` (`cargo check --workspace --locked` on the
pinned toolchain). If a change genuinely needs a newer compiler or a
dependency bump raises the floor, bump `rust-version` and the workflow's
`MSRV` env var together in the same PR, and call it out in the CHANGELOG.

## Testing & Coverage

CI collects workspace test coverage with
[`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) on every PR
(the `Coverage (informational)` job in `.github/workflows/ci.yml`) and
uploads an lcov report as the `coverage-lcov` workflow artifact. The job is
**informational only — it never gates a merge**.

Per-crate line-coverage **targets** — goals to steer test investment, not
gates that fail CI:

| Crate | Target | Why |
|---|---|---|
| `orbit-policy` | > 90% | Policy evaluation is the security decision surface; on Linux it is the only enforcement layer. |
| `orbit-core` | > 80% | Composition root and command handling — regressions here surface everywhere. |
| `orbit-exec` | > 70% | Process spawning/sandboxing is platform-conditional, so some paths are unreachable on any single CI runner. |

When touching those crates, check the coverage summary in the CI job log (or
run `cargo llvm-cov -p <crate> --summary-only` locally) and prefer adding
tests that close the gap toward the target.

### Tests that depend on host process visibility

A few suites derive a process-start identity token from `ps -o lstart=`
(`orbit_common::process::identity`). Some launch environments refuse to
execute `ps` at all — the macOS agent-executor sandbox denies it — so those
probes return `Unavailable` and production takes its documented fail-safe
branch: an owner it cannot verify is neither finalized nor signalled.

A test whose subject *is* the derived token must therefore ask before
asserting, rather than fail for a reason unrelated to the code under test.
Call `orbit_common::test_env::start_identity_probe_blocker()`: `None` means
the probe works and the full assertion applies; `Some(reason)` names the
constraint, and the test either returns early or asserts the fail-safe
branch, logging `reason` so the choice is attributable from the log. Never
weaken the assertion taken when the probe *is* available.

## Repository Shape

Rust workspace crates live under `crates/` (for example `crates/orbit-cli`).

- `orbit-cli`: CLI entrypoint
- `orbit-core`: composition root, command handling, runtime wiring
- `orbit-engine`: job and activity execution engine
- `orbit-tools`, `orbit-agent`, `orbit-store`, `orbit-types`, `orbit-policy`, `orbit-exec`: supporting runtime layers

## Change Expectations

- Keep changes scoped and intentional.
- Add or update tests when behavior changes.
- Prefer removing legacy paths over carrying compatibility code when the product is still pre-adoption.
- If you discover friction or recurring issues, fix them in scope or create a concrete follow-up task.

## Supply-chain (cargo-deny)

Dependencies are gated by [`cargo-deny`](https://embarkstudios.github.io/cargo-deny/)
on every PR (via `scripts/ci-guardrails.sh`) and locally with `make audit`. The
policy lives in [`deny.toml`](deny.toml): it denies crates with an open RUSTSEC
advisory or a yanked version, and restricts licenses to a reviewed allow-list.

Run it before landing an internal dependency change:

```bash
cargo install cargo-deny --locked   # one-time
make audit                          # == cargo deny check
```

**Adding a license.** If a new dependency introduces a license not in the
`[licenses].allow` list, `cargo deny check` fails. Add the SPDX identifier to
the list in `deny.toml` **only** if it is a permissive/public-domain-equivalent
license, with a one-line comment naming the crate(s) and (for weak-copyleft
licenses such as MPL-2.0) a short justification. Copyleft licenses that would
impose obligations on Orbit's own sources must not be added — replace the
dependency instead.

**Advisory exceptions.** Only when there is no safe upgrade available may an
advisory be time-boxed in `[advisories].ignore`. Each entry must be an object
carrying:

- `id` — the `RUSTSEC-YYYY-NNNN` identifier, and
- `reason` — why it is safe in Orbit's usage (why the vulnerable path is
  unreachable or the impact is bounded) **and** a `Re-review YYYY-MM-DD` date
  (default: ~6 months out).

Re-review ignored advisories on or before their date and drop the entry once an
upstream fix lands. Never ignore an advisory that has an available patched
release — bump the dependency instead.

If cargo-deny reports `advisory-not-detected` for an ignored entry, treat it as
the signal to retire that exception: verify that the advisory no longer matches
the current dependency graph or advisory policy, then remove the entry and its
stale rationale and rerun the supply-chain checks. If the dependency or
advisory is later reintroduced, review it afresh against its current use and
dependents rather than carrying the old exception forward.

**Isolated advisory database override.** In managed runners or sandboxed validation
where `~/.cargo/advisory-dbs` is read-only (such as containerized workers), configure
an explicit writable advisory database path via `CARGO_DENY_DB_PATH` (or
`ORBIT_CARGO_DENY_DB_PATH`). The canonical wrapper (`scripts/cargo-deny.sh`)
synthesizes an isolated configuration pointing `[advisories].db-path` to that
directory so cargo-deny can acquire its database lock without writing to the home
directory. Cleanup of the temporary scratch directory or fixture is owned by the
invoking runner.

**Offline validation & snapshot provenance.** To validate offline without network
fetching, set `CARGO_DENY_DISABLE_FETCH=1` (or `ORBIT_CARGO_DENY_DISABLE_FETCH=1` /
`CARGO_DENY_OFFLINE=1`). The advisory database snapshot is expected to be cloned
from upstream [`https://github.com/rustsec/advisory-db`](https://github.com/rustsec/advisory-db)
(with default target directory `advisory-db-3157b0e258782691`) or provided by the
environment fixture. Missing, stale, or unrefreshable required data yields an explicit
failure, never a success-by-skip.

## Vendored dashboard JavaScript

The embedded dashboard vendors DOMPurify and marked as checked-in files under
[`crates/orbit-web/assets/dashboard/`](crates/orbit-web/assets/dashboard/).
Pins, upstream URLs, SHA-256 digests, and the refresh command are in
[`vendor-manifest.json`](crates/orbit-web/assets/dashboard/vendor-manifest.json);
the procedure is in [`VENDOR.md`](crates/orbit-web/assets/dashboard/VENDOR.md).
`make ci-fast` runs [`scripts/check-dashboard-vendor.py`](scripts/check-dashboard-vendor.py),
which fails if a blob no longer matches its recorded digest, if those
versions drift from `package.json`, or if `package-lock.json` is missing or
drifted from the recorded pins.

GitHub Dependabot (`npm` ecosystem on that directory in
[`.github/dependabot.yml`](.github/dependabot.yml)) and GitHub security alerts
cover new releases and advisories. The workspace `dependabot-alert-sweep` job
files remediation tasks from those alerts. `cargo-deny` does not see these files.

Refresh with `./scripts/refresh-dashboard-vendor.sh` after changing the pins;
do not edit the minified blobs by hand.

## Orbit State

Orbit keeps operational state under `.orbit/`. Review those changes carefully before committing.

- Do not accidentally commit noisy runtime artifacts.
- Treat tracked asset changes as product changes.
- Treat mutable run/task state as operational data unless the change is intentional.

## Commits

- Use clear commit messages.
- Agent-authored commits should use the agent commit identity for that commit.
- Do not leave the repository configured with the agent identity afterward.
