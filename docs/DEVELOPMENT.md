---
type: context
summary: "Orbit development reference: test isolation, coverage, MSRV, supply chain, vendored JS"
last_validated: 2026-09-23
---

# Development Reference

The detailed rules behind [CONTRIBUTING.md](../CONTRIBUTING.md). Read the section that matches what you are touching.

## MCP Apps compatibility prototype

See [the isolated reproduction and native desktop probe](mcp-apps-probe.md)
for the versioned Control Center, operator controls, isolated rendered preview,
routing boundaries and evidence template.
Automated protocol/bridge results and native desktop results are recorded separately.

## Dashboard HTTP integration fixtures

`cargo test -p orbit-web --test http_api` drives the public dashboard server
over loopback HTTP. All cases share one integration binary; helpers and cases
live under `crates/orbit-web/tests/http_api/`. Each mutable fixture re-executes
its exact test with inherited authority cleared, then launches the public
`serve_from_env` entry point against a disposable registry and workspace.
Child tests have a 60-second deadline, server readiness has a 10-second
deadline, and HTTP requests (including SSE reads) have a 5-second timeout.
Process guards kill and reap servers even after an assertion fails.

The security table discovers literal paths from API router registrations and
uses HTTP `Allow` responses to enumerate their mutating methods. It checks
origin/Host protection on every mutation and operator admission on governed
actions. Existing ordinary writes have explicit method/path exceptions;
new mutating routes default to operator-only. The auto launch probe requests
completion authority, which is governed separately from a normal launch.
This is source-assisted discovery followed by behavioral HTTP assertions,
not a source-text snapshot. No private router API is exposed for testing.

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

The same authority list also clears `ORBIT_WORKER_CONTEXT_REQUIRED`, which
would otherwise demand a worker binding from an empty fixture registry. Orbit
core's mutable libtest fixtures use the shared
`application::tests::run_isolated_test` launcher to re-execute an exact test
with this isolation and verify one test passed in the child.

The pattern above is already used by
[`crates/orbit-cli/tests/tool_list.rs`](../crates/orbit-cli/tests/tool_list.rs).
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
[`crates/orbit-cli/tests/ambient_authority_isolation.rs`](../crates/orbit-cli/tests/ambient_authority_isolation.rs)
for the regression coverage.

Live `bwrap` spawn is not available from inside an agent-executor or job-run
worktree. The outer sandbox blocks nested `unshare(CLONE_NEWUSER)`, so even
`bwrap --ro-bind / / --tmpfs /tmp --dev /dev -- echo works` fails with
`No permissions to create new namespace`. Treat that denial as nested-sandbox
environment, not a missing AppArmor profile or a product defect. Replay live
spawn checks (`spawn_under_linux_bwrap`, `--run-ignored`) on the owning Linux
host. Do not disable `linux-bwrap` or try to make bwrap nest from a fixture.

## Testing & Coverage

Authorization coverage is generated from the live governed-operation and
builtin tool registries by `authorization_matrix_matches_live_registry` in the
CLI `output_goldens` integration binary. `make goldens UPDATE=1` regenerates
its operation/capability/caller verdict table; review every capability diff.
The `public_tool_surface` integration binary dispatches invalid arguments to
every registered `orbit.task.*` tool, including inactive tools, and checks each
declared parameter. Task dispatch refuses unknown fields and incompatible
types before domain execution. Transport wrappers remain supported, as do
existing optional nulls, numeric strings, string booleans and string/list
forms. Handler-specific required fields and guarded modes are still validated
by their handlers. `agent_task_deletion_is_denied_through_every_dispatch_path`
in `mcp_roundtrip` exercises runtime, CLI/MCP dispatch and local/remote MCP
sessions in a disposable child process with a 120-second deadline.

CI collects workspace test coverage with
[`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) on every PR
(the `Coverage (informational)` job in `.github/workflows/ci.yml`) and
uploads an lcov report as the `coverage-lcov` workflow artifact. The job is
**informational only — it never gates a merge**. It runs the tests through
`cargo llvm-cov nextest`, so each test gets its own process and the
`.config/nextest.toml` test groups apply, as in the `Check / Clippy / Test` job.

Per-crate line-coverage **targets** — goals to steer test investment, not
gates that fail CI:

| Crate | Target | Why |
|---|---|---|
| `orbit-policy` | > 90% | Policy evaluation is the security decision surface; on Linux it is the only enforcement layer. |
| `orbit-core` | > 80% | Composition root and command handling — regressions here surface everywhere. |
| `orbit-exec` | > 70% | Process spawning/sandboxing is platform-conditional, so some paths are unreachable on any single CI runner. |

When touching those crates, check the coverage summary in the CI job log (or
run `cargo llvm-cov nextest -p <crate> --summary-only` locally) and prefer adding
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

### Tests and macOS temp paths

On macOS `TMPDIR` is `/var/folders/...`, and `/var` is a symlink to
`/private/var`. Orbit resolves symlinks in the roots it is given (discovery
lists a resolved directory and reports resolved paths, registries record
resolved checkouts), so a fixture under the default temp directory exercises a
symlinked ancestor that Linux CI never sees. In practice:

- Production code must treat a symlinked ancestor above the Orbit or workspace
  root as ordinary. Compare a resolved path with a resolved path (or an
  unresolved one with an unresolved one), never one of each, and police
  symlinks only inside the tree Orbit controls. A test that has to hold on every
  platform builds its own symlink (`std::os::unix::fs::symlink` from a real
  directory) rather than relying on the platform's temp directory.
- A fixture that compares paths Orbit reports with paths it spelled roots
  itself at `tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir())`,
  or canonicalizes the path it compares against. Do not override `TMPDIR` for
  the suite: that hides the symlinked-ancestor cases the default exercises.
- Unix socket fixtures (the plugin broker) also need a short root, because
  `sun_path` is 104 bytes on macOS; root them under `/tmp` as the broker's own
  fixtures do.

A test that needs a live process that is no part of the test process uses
`orbit_common::test_env::spawn_unrelated_process()` instead of pid 1, whose
start time an unprivileged caller cannot read on macOS.

### Tests that submit pipeline runs

Submitting a run (ship, resume, auto, job) spawns a detached worker that
re-executes the current binary. From a test, that binary is the libtest
harness, which reads the worker argv as test filters and can re-run the
spawning test without bound. The 2026-09-23 outage
([RCA](rca/2026-09-23-cross-crate-test-worker-oom.md)) started this way.
The CLI `main` explicitly marks its process as allowed to re-execute as a
worker. A libtest harness never runs that entry point, so an unsubstituted
submission fails with `OrbitError::Execution` before spawning, whatever path
the harness has. The old Cargo `deps/<crate>-<hash>` path check was removed;
worker permission no longer depends on Cargo's filename layout.

- Inside `orbit-core`, install a per-thread substitute with
  `worker_command_override::set` and clear it on drop.
- In a downstream crate, enable the orbit-core `test-support` feature from
  `[dev-dependencies]` only. Then call
  `orbit_core::test_support::install_substitute_pipeline_worker` before the
  submission. It is process-wide, so it also covers submissions that run on
  another thread, such as a dashboard handler's blocking pool. Install one
  argv per test binary, as `substitute_pipeline_worker` in the `orbit-web`
  test module does. CLI integration tests that execute the real `orbit`
  binary instead exercise its production entry point.

## Toolchain (MSRV)

Orbit's minimum supported Rust version is declared as `rust-version` in the
workspace `Cargo.toml` (`[workspace.package]`) and enforced by the `msrv` job
in `.github/workflows/ci.yml` (`cargo check --workspace --locked` on the
pinned toolchain). If a change genuinely needs a newer compiler or a
dependency bump raises the floor, bump `rust-version` and the workflow's
`MSRV` env var together in the same PR, and call it out in the CHANGELOG.

## Supply-chain (cargo-deny)

Dependencies are gated by [`cargo-deny`](https://embarkstudios.github.io/cargo-deny/)
on every PR (via `scripts/ci-guardrails.sh`) and locally with `make audit`. The
policy lives in [`deny.toml`](../deny.toml): it denies crates with an open RUSTSEC
advisory or a yanked version, and restricts licenses to a reviewed allow-list.

Run it before landing an internal dependency change:

```bash
cargo install cargo-deny --locked   # one-time
make audit                          # == cargo deny check
```

**YAML parser provenance and compatibility.**

The workspace's `serde_yaml` dependency key aliases
[`yaml_serde`](https://github.com/yaml/yaml-serde), the YAML organization's
continuation of David Tolnay's archived `serde-yaml`. The shared alias follows
the upstream migration instructions and preserves callers' typed codecs,
`Value`/`Mapping` operations and error types without adding crate dependencies.
It covers persisted tasks/frontmatter, workflow catalogs, routines, auto-tasks,
policies, workspace identities, model prices, plugin manifests, schemas and
conformance files.

Selection checked on 2026-09-30: the
[`0.10.7` registry release](https://crates.io/crates/yaml_serde/0.10.7) was
published on 2026-08-18, is not yanked, and declares Rust 1.82 (below Orbit's
MSRV). The [upstream history](https://github.com/yaml/yaml-serde/commits/main/)
retains the original development history and includes August 2026 fixes for
I/O error sources and compiler compatibility. Its
[CI](https://github.com/yaml/yaml-serde/blob/main/.github/workflows/ci.yml)
includes stable/MSRV tests, Miri and fuzz-target compilation. Registry package
metadata, source revision and the Cargo.lock checksum identify the selected
release; the fork is MIT OR Apache-2.0 licensed.

The parser backend is now
[`libyaml-rs` 0.3.0](https://github.com/yaml/libyaml-rs), maintained by the same
organization as a fork of `unsafe-libyaml`. It remains a C-to-unsafe-Rust
translation of libyaml: this migration addresses maintenance, and is not a
claim of memory safety or exhaustive malformed-input coverage. Continue to
run `make audit` and review upstream activity on upgrades. The similarly named
`serde_yaml_ng` and `serde_norway` had older registry releases at selection;
`serde-saphyr` is actively developed but lacks the compatible YAML `Value` API
used here and would require a broader migration.

No persisted-format migration is required. Behavioral coverage in
`orbit-common`'s protocol YAML tests and `orbit-tools`' plugin loader tests
checks round trips, anchors, explicit scalar tags, block/quoted scalars,
timestamps, invalid documents and plugin field/location diagnostics. Existing
type, storage, plugin and golden tests guard the surrounding contracts. Run the
focused codecs and plugin fixtures when changing the parser:

```bash
cargo test --locked -p orbit-common --lib protocol::tests::yaml
cargo test --locked -p orbit-types --lib
cargo test --locked -p orbit-tools --tests
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
[`crates/orbit-web/assets/dashboard/vendor/`](../crates/orbit-web/assets/dashboard/vendor/).
Pins, upstream URLs, SHA-256 digests, and the refresh command are in
[`vendor-manifest.json`](../crates/orbit-web/assets/dashboard/vendor/vendor-manifest.json);
the procedure is in [`VENDOR.md`](../crates/orbit-web/assets/dashboard/vendor/VENDOR.md).
The npm `package.json` and `package-lock.json` sit at the dashboard root.
`make ci-fast` runs [`scripts/check-dashboard-vendor.py`](../scripts/check-dashboard-vendor.py),
which fails if a blob no longer matches its recorded digest, if those
versions drift from `package.json`, or if `package-lock.json` is missing or
drifted from the recorded pins.

GitHub Dependabot (`npm` ecosystem on that directory in
[`.github/dependabot.yml`](../.github/dependabot.yml)) and GitHub security alerts
cover new releases and advisories. The workspace `dependabot-alert-sweep` job
files remediation tasks from those alerts. `cargo-deny` does not see these files.

Refresh with `./scripts/refresh-dashboard-vendor.sh` after changing the pins;
do not edit the minified blobs by hand.
