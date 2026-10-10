---
type: context
summary: "Orbit development reference: test isolation, coverage, MSRV, supply chain, vendored JS"
last_validated: 2026-10-10
---

# Development Reference

The detailed rules behind [CONTRIBUTING.md](../CONTRIBUTING.md). Read the section that matches what you are touching.

| You are… | Read |
|---|---|
| Handing off or reviewing a change | [Validation and CI](#validation-and-ci) |
| Writing files or spawning processes in production code | [Filesystem writes](#filesystem-writes), [Child processes](#child-processes) |
| Writing a test that mutates state, spawns processes or submits runs | [Safe Mutable CLI Fixtures](#safe-mutable-cli-fixtures), [Testing & Coverage](#testing--coverage) |
| Changing the dashboard or website | [Dashboard fixtures](#dashboard-http-integration-fixtures), [Browser checks](#browser-checks-on-a-prepared-host) |
| Changing toolchains, platforms or dependencies | [MSRV](#toolchain-msrv), [Windows](#windows-compile-check), [Supply-chain](#supply-chain-cargo-deny), [Vendored JS](#vendored-dashboard-javascript) |

## Validation and CI

Run these gates before implementation handoff or before-PR review:

```bash
make ci-fast           # MSRV compile check, formatting, script guardrails; no Rust tests
make ci-test-affected  # complete test targets of changed crates and their workspace dependents
make ci-lint           # clippy for production and all targets, then rustdoc
make goldens           # CLI/MCP, CI log and sandbox profile goldens; UPDATE=1 regenerates
```

- `make ci-fast` passing says nothing about the Rust tests. Focused test filters help you investigate but do not replace `make ci-test-affected`: an existing test can encode behavior your change breaks even when your new tests pass.
- Run Cargo-based gates (`ci-fast`, `ci-test-affected`, `goldens`, `ci-lint`, `cargo test` / `nextest`) one at a time when they share a target directory, or give each its own `CARGO_TARGET_DIR`. Concurrent gates in one target can rebuild `target/debug/orbit` under running generation-bound fixtures and fail them.
- Guardrails need Python 3.11+ for `tomllib`; stock macOS `/usr/bin/python3` is 3.9. [require-python.sh](../scripts/require-python.sh) runs first in `make ci-fast`, `make ci-lint` and `ci-guardrails.sh` and stops with the version it found. It installs nothing: put a newer `python3` first on `PATH`.
- Skill docs are canonical under `crates/orbit-core/assets/skills/`. Edit them there and run `scripts/sync-plugin-skills.sh` to regenerate `plugin/skills/`; `make ci-fast` fails on a stale mirror.
- [check-doc-links.py](../scripts/check-doc-links.py) fails on a missing relative link target, heading anchor or backticked repository path in tracked Markdown. Website links resolve through Starlight routes and code blocks are ignored; its exclusions (historical decisions, RCAs, templates, changelog, fixture prose) are listed in the script. Run `python3 scripts/check-doc-links.py`; its fixtures are in `python3 scripts/test-ci-fast-guards.py`.
- [check-unused-dependencies.py](../scripts/check-unused-dependencies.py) fails on a manifest dependency its sources never use, a normal dependency only test targets use, or a `[workspace.dependencies]` entry no member declares. Keep a feature-only dependency in its `FEATURE_ONLY` allowlist with a reason. Self-test: `python3 scripts/test-check-unused-dependencies.py`.

### Affected-test selection

`make ci-test-affected` (`scripts/ci-test-affected.py`) reads Cargo metadata without compiling. It selects every crate with changed paths plus all transitive reverse workspace dependents (dev, build, renamed, optional and target-specific edges), then runs their library, binary and integration test targets with nextest (Cargo if nextest is missing) and Cargo doctests, under the shared [build-budget admission](runbooks/build-budget.md). A change under `crates/orbit-core/` selects `orbit-core`, `orbit-cmd`, `orbit-web` and `orbit-cli`.

- **Diff:** committed, staged, unstaged and non-ignored untracked paths; a move selects both crates.
- **Base:** the merge base of `HEAD` with `origin/agent-main` (local `agent-main` if the remote ref is absent). Override with `CI_TEST_BASE=<rev>`; a reviewer with a pinned delivery base must run `CI_TEST_BASE=<base.commit> make ci-test-affected`. An unavailable base fails the gate.
- **Everything or nothing:** a change to `Cargo.toml`, `Cargo.lock`, `.cargo/`, `.config/` or the toolchain files, or a removed crate, selects every crate. A docs-only diff selects nothing and passes without compiling.
- **Cross-crate file reads:** Cargo cannot see a test reading files outside its crate. Declare the path prefix and reading crates in `FILE_READERS` in `scripts/ci-test-affected.py`; nothing detects them for you, and a declared crate missing from metadata fails the gate.
- **Inspect:** `python3 scripts/ci-test-affected.py --list [--base <commit>]`.
- **Test environment:** nextest runs with `--success-output immediate`, so the host verifying a required check sees, and refuses, a pass carrying a `DEFERRED:` notice (a test that skipped its sandboxed path). A temporary directory inside the checkout is added to `GIT_CEILING_DIRECTORIES`. `ORBIT_WORKER_CONTEXT_REQUIRED` is dropped ([why](#test-process-environment)); the rest of the run envelope passes through.

To check whether a failure is pre-existing, replay the same command on the unmodified base: extract it under `.orbit/tmp/` (for example `git archive <base> | tar -x -C .orbit/tmp/base`), refresh its timestamps, give it its own `CARGO_TARGET_DIR` (any path works; the `orbit update` fixtures copy the tested binary to their own `target/debug/orbit`), and set `GIT_CEILING_DIRECTORIES` to `.orbit/tmp` so its fixtures cannot find the surrounding checkout. Replay the same nextest selection soon after the candidate run, since fixture deadlines depend on host load.

### Required validation commands

The owner workspace lists `make ci-test-affected` in [`workflow.required_validation_commands`](CONFIG.md), in ignored per-user `.orbit/config.toml`. Append it and keep the existing entries:

```toml
[workflow]
required_validation_commands = ["make ci-fast", "make ci-test-affected"]
```

Candidate validation and before-PR review then require a passing affected-test record. Distributed review contracts freeze the owner's list at admission, so existing claims need a fresh admission after a change.

### Validation summary and base reruns

When a required command fails, Orbit reruns it on the base to tell a red base from a candidate failure ([decision rules](CONFIG.md#workflowvalidation_env--the-toolchain-required-validation-runs-with)). A command that selects tests from the diff would test nothing on the base, so the runner passes two variables, both implemented by `scripts/ci-test-affected.py`:

- `ORBIT_VALIDATION_SUMMARY`: a file outside the checkout where the script writes `{"schema_version": 1, "selection": {"packages", "target_flags", "doctest_packages"}, "tests_run": N}`. `tests_run` comes from nextest's JUnit report (enabled with `--tool-config-file`); it is `null` after a build failure or under the `cargo test` fallback, and excludes doctests.
- `ORBIT_VALIDATION_SELECTION`: the candidate's `selection`. The script tests exactly those packages; a package the checkout lacks fails the run without a summary.

A base run counts only if it reports the same selection, and a base pass only if it ran at least one test. Otherwise it is not comparable: it never makes a failure `baseline_red`, never refutes a reviewer's red-base claim and never lifts a hold that recorded the selection. A command that writes no summary is compared by exit status, so only `make ci-test-affected` is guarded against a zero-test base pass.

### Hosted CI

Hosted CI runs the full `make ci` on open PRs. Each PR-triggered workflow starts with a checkout-free `Live PR state` job (`pull-requests: read` only); if the PR merged or closed while queued, the build and analysis jobs are skipped under unchanged check names, which GitHub treats as passing. An API error fails the gate rather than admitting an unknown state; a PR that closes after the gate read `open` can still finish. Push, scheduled and manual runs skip the gate, so agent-main/main CI and CodeQL still cover merged changes. `make ci-fast` runs `scripts/test-pr-state-workflows.py`, which replays each gate against recorded API responses.

## Filesystem writes

The production Clippy pass in `make ci-lint` disallows `std::fs::create_dir_all`, `std::fs::create_dir` and `std::fs::write`, including aliases. Use the `orbit_common::fs::io` helpers:

| Need | Helper |
|---|---|
| Orbit-owned directory | `create_private_dir_all`, or `create_private_dir` for exclusive creation. Owner-only on Unix regardless of umask; existing modes are left alone. |
| Replace a durable file | `atomic_write_text` / `atomic_write_bytes`: stage a private sibling, sync, rename, sync the parent. Permissions are preserved. |
| Validate between staging and commit | `StagedTextFile`; dropping an uncommitted stage removes it. |
| New private file, exclusively | `write_new_private_text` |
| Deliberate new-file or disposable write (not atomic) | `write_text_with_parent` |

State and task-artifact writers must use the private helpers; `orbit doctor --fix-state-directory-permissions` repairs legacy writable state. An exception is scoped to the one intentional call and names its reason in `#[allow(clippy::disallowed_methods, reason = "...")]`. Current exceptions: external source, installation, service-manager and Cargo output directories; in-place edits of dotfile-linked user TOML configs; artifact exports to user-selected paths (including symlinks and devices); scaffold sources with their `--force` behavior. None promises crash-safe replacement. Test fixtures may write directly.

## Child processes

Production code never waits on a child without a deadline. Spawn through `orbit_common::process::run_bounded` or `run_bounded_capped`, which put the child in its own process group, kill the group at the deadline and cap captured output. `run_bounded_capped_typed` returns a failed spawn as its `io::Error` for callers that branch on the kind (the `ETXTBSY` retry in `orbit update`, the sandbox probes).

The production Clippy pass disallows `std::process::Command::output` and `Command::status`; an exception needs `#[allow(clippy::disallowed_methods, reason = "...")]`. The current exceptions have no natural deadline: the SSH stdio relay in `orbit-mcp` and the interactive or operator-watched `sudo` steps of `orbit init` on Linux. Fixtures may use both.

## Safe Mutable CLI Fixtures

Fixtures and manual reproductions that mutate Orbit task, run, workspace or registry state run isolated from the process that launches them; authorized operator work keeps normal routing. Spawn the CLI as a child with absolute disposable paths, clearing inherited authority before setting `HOME` and `USERPROFILE`:

```rust
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

`clear_inherited_authority` removes the managed-run routing, identity and grant variables, including `ORBIT_WORKER_CONTEXT_REQUIRED`. [`tool_list.rs`](../crates/orbit-cli/tests/tool/tool_list.rs) uses this pattern; [`isolated_cli_fixture.rs`](../crates/orbit-cli/tests/support/isolated_cli_fixture.rs) is a shared disposable home, registry and workspace. Orbit core's libtest fixtures use `application::tests::run_isolated_test`, which re-executes one exact test this way and verifies it passed in the child.

Before adding tasks or starting runs, make the checkout a Git repository (`git_repo::init(&work)`), run `workspace init`, then check routing read-only: `workspace show --format json` must report `registered: true`, and `checkout.repo_root` and `checkout.orbit_dir` must equal the canonicalized fixture paths (the child reports physical paths). This catches a fixture routed to an ambient workspace before it mutates anything.

- **Every fixture checkout gets its own Git repository,** negative controls included. Root discovery walks past a plain directory or empty `.git`, so with `TMPDIR` inside another checkout (such as a managed `.orbit/tmp`) the fixture resolves the enclosing Orbit root and `workspace init` refuses, or fails on a managed run's read-only `.orbit`. Never answer that with `--force`. Ordinary lookup and the plugin's SessionStart hook still accept an `.orbit` at any ancestor, so a control that needs "no workspace above" must check that precondition and report a skip.
- **`export HOME=/tmp/...` is not isolation.** A managed child can inherit `ORBIT_MANAGED_RUN_CONTEXT` and `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE`, which carry durable authority and override home discovery. Never run bare mutable CLI commands in a managed worker ([regression test](../crates/orbit-cli/tests/workspace/ambient_authority_isolation.rs)).
- **Build every fixture Git command, reads included, with `git_repo::command()`** (`crates/orbit-cli/tests/support/git_repo.rs`), which scrubs authority first; set another initial branch with a scrubbed `symbolic-ref` before the first commit. Inherited `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, index or object-directory variables can amend the parent repository or force-push to its origin.
- **Engine fixtures that publish Git branches** create repositories from scratch, verify the root before writing, and set `GIT_ALLOW_PROTOCOL=file` plus a file-only protocol policy in the child's disposable `HOME/.gitconfig` (the engine's VCS adapter clears Git variables but keeps `HOME`). [`git_fixture.rs`](../crates/orbit-engine/tests/engine/git_fixture.rs) refuses any effective push URL, after rewrites, that is not an absolute local path; fetch failures use local upload-pack programs. Keep this policy when a test replaces its global Git config.
- **Change process-wide state only in a re-executed child,** never through a guard in the parallel libtest parent. This covers forge-outage Git wrappers on `PATH` (a sibling could resolve the wrapper after its directory is dropped), tracing's callsite-interest cache in the log-capture fixture, and the Bubblewrap environment and credential-mask fixtures, whose launcher reads `HOME` and `CARGO_HOME`: they run with a cleared environment and a private `HOME` outside `/tmp`, and receive fixture variables through the child command.
- **Workflow-dispatch fixtures wait for their detached workers** to reach a terminal state in the fixture's run history before dropping the temporary home; a successful submission proves only admission. Check that the expected runs are recorded and that a disposable parent registry exposed through the managed environment gains none, as the output-goldens task-pilot fixture does.

`cargo test -p orbit-cli --test tool plugin_secrets::secret_sinks::seeded_secrets_stay_out` seeds fake provider, SCM, cloud and database credentials before redactor initialization, drives task writes and provider output through the CLI, and reports each leak by sink: task bundles, audit rows and blobs, run logs, attached diagnostics, dashboard log views and the provider's environment. Raw task attachments keep their byte-preserving contract. Its executor runs with `sandbox: off`.

## Testing & Coverage

Test design (boundary first, unit tests by exception, binary layout) is in [test_strategy.md](design-patterns/test_strategy.md). This section covers the mechanics.

### Test process environment

- **Worker binding.** A claimed executor exports `ORBIT_WORKER_CONTEXT_REQUIRED`, so Orbit commands its agent runs refuse to start without a worker binding. A test has none, so any in-process runtime would fail with `managed worker runtime binding unavailable`. `make ci-test-affected` drops the marker; tests of the refusal set it on their own child.
- **Focused `cargo test` in a managed run.** Every test binary of `orbit-common` and `orbit-core` calls `orbit_common::isolate_test_process!()`, which clears `INHERITED_AUTHORITY_ENV` before `main`; re-executed children keep what their parent set. Add the macro to any other crate whose tests build a runtime: under `#[cfg(test)]` in the lib and at the top of each integration target.
- **Seatbelt on macOS.** Tests that launch Seatbelt-confined children call `orbit_exec::macos_sandbox_test_guard(test_name)`. Only a probe exit 71 with the `sandbox_apply` marker (a nested macOS executor) yields a named `SKIP:`; a missing binary, spawn error, timeout or other failure fails the test. macOS host coverage sets `ORBIT_REQUIRE_SANDBOX_EXEC=1`, which makes an apply refusal fail too; a skipped run never satisfies `host_sandbox_test` evidence. Binary-absence tests use `sandbox_exec_available()`.
- **Bubblewrap on Linux.** Inside an agent-executor or job-run worktree, live `bwrap` fails with `No permissions to create new namespace` because the outer sandbox blocks nested user namespaces. That is the environment, not a defect: replay the live spawn checks (`spawn_under_linux_bwrap`, `--run-ignored`) on the owning Linux host, and never disable `linux-bwrap` or try to make it nest ([runbook](runbooks/linux-sandbox.md#verify-the-bubblewrap-boundary-natively)).

### Fixtures on a loaded host

Gates often run beside a busy drain, where a fixture's wall-clock time stretches tenfold. Keep outcomes independent of host load:

- Re-execute child tests through `orbit_common::test_env::run_child_test` and verify them with `assert_child_test_passed`. `CHILD_TEST_DEADLINE` (300 s, below nextest's ten-minute kill) is a hang guard; an overrun reports load averages and the child's output. The child leads its own process group; a watchdog process kills that group if the test process dies first (nextest interrupt or timeout), so no fixture child outlives its parent.
- End a wait when its event arrives, size its ceiling for a saturated host, and on expiry report what it saw plus `orbit_common::test_env::host_load()`. `test_env::wait_until` does this for a readiness condition under `FIXTURE_STEP_DEADLINE` (120 s). Bound a single CLI command or HTTP request by the same guard, and call `test_env::assert_step_finished` before asserting on a command's outcome, so a command killed at the guard does not read as a refusal.
- Admission reads the host resource monitor, so a fixture not testing throttling pins a calm sample with `OrbitRuntime::with_host_resource_probe` on every runtime it opens, including reopens; a CLI fixture sets `workflow.resource_throttle.enabled` to false with `orbit config set` in its disposable config. Throttling tests drive their own probe, stamping each sample's age when the probe is read rather than at setup, so slow setup cannot make it stale.
- Bound work, not elapsed time: measure cost guards in CPU time where possible.
- Seeding pays an fsync per task. Root a fixture that seeds thousands at `tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())`, which prefers tmpfs on Linux, and report its phases with `orbit_common::test_env::FixtureProgress` so an overrun names the phase.
- A production fallback taken under load (such as a timed-out login probe) may satisfy a fixture only if the result reports the fallback and its reason.

### Tests that submit pipeline runs

Submitting a run (ship, resume, auto, job) spawns a detached worker by re-executing the current binary. From a test, that binary is the libtest harness, which would rerun the spawning test without bound ([RCA](rca/2026-09-23-cross-crate-test-worker-oom.md)), so only the CLI `main` may act as a worker and an unsubstituted submission fails with `OrbitError::Execution`.

Install a substitute with `orbit_core::test_support::install_substitute_pipeline_worker` (`crate::test_support` inside `orbit-core`), enabling the orbit-core `test-support` feature from `[dev-dependencies]` only. It is process-wide and the last install wins, so it covers other threads (such as a dashboard's blocking pool); install one argv per test binary, as `crates/orbit-web/tests/http_api/support.rs` does. `{run_id}` in the argv becomes the run id. CLI tests that run the real `orbit` binary use its production entry point instead.

### Tests that depend on host process visibility

`orbit_common::process::identity` derives process-start tokens from `/proc` on Linux, libproc on macOS and `ps -o lstart=` elsewhere, keeping the UTC/C-locale `ps` format so persisted tokens stay compatible. When the probe is `Unavailable`, production takes its fail-safe branch: an owner it cannot verify is neither finalized nor signalled.

A test whose subject is the token first calls `orbit_common::test_env::start_identity_probe_blocker()`. `None`: assert fully. `Some(reason)`: return early or assert the fail-safe branch, and log `reason`. Never weaken the full assertion. For a live process outside the test, use `orbit_common::test_env::spawn_unrelated_process()`, not pid 1, whose start time macOS hides from unprivileged callers.

### Process fixture cleanup and readiness

- Hand every dashboard and persistent MCP child in `crates/orbit-cli/tests` to `ChildGuard` ([`child_guard.rs`](../crates/orbit-cli/tests/support/child_guard.rs)) right after spawn, before pipe extraction or readiness checks. It sends SIGTERM, waits two seconds, then kills and reaps, also on assertion unwinds and across executable handover, and never re-signals a reaped PID. Explicit shutdown tests may still signal and wait on the child.
- Wait for observable readiness under a generous ceiling and measure shutdown separately (SIGINT/SIGTERM ceiling: ten seconds). Readiness waits detect early supervisor exit and keep its output. Never add launch retries or relax shutdown assertions to hide startup failures.
- Observe persisted state rather than CLI timing: the registry-lock fixture reads the worker's run through a read-only SQLite connection while holding the registry writer.
- Before asserting a task's final state after a detached resume, wait for the recorded worker (PID and start identity) to exit, within a deadline: the run turns terminal before coupled-task cleanup finishes.

### Tests and macOS temp paths

macOS `TMPDIR` is under `/var/folders`, and `/var` is a symlink to `/private/var`. Orbit resolves the roots it is given, so fixtures there have a symlinked ancestor Linux CI never sees.

- Production code treats a symlinked ancestor above the Orbit or workspace root as ordinary: compare resolved with resolved, never resolved with unresolved, and police symlinks only inside the tree Orbit controls.
- A fixture that compares Orbit's reported paths with paths it spelled creates its temp dir with `tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir())`, or canonicalizes its side. A test that must hold everywhere builds its own symlink with `std::os::unix::fs::symlink`.
- On unrestricted hosts keep the default `TMPDIR`. Managed executors put scratch and logs in a run subdirectory of the injected `ORBIT_SCRATCH_DIR` (`.orbit/tmp`) and point `TMPDIR` there; the [cross-revision helper](runbooks/compiler-cache.md#cross-revision-beforeafter-validation) requires this explicit root.
- Root Unix socket fixtures (the plugin broker) under `/tmp`: macOS `sun_path` holds 104 bytes.

### Authorization coverage

- `authorization_matrix_matches_live_registry` (CLI `output` binary, `output_goldens` module) generates the operation/capability/caller verdict table from the live registries. `make goldens UPDATE=1` regenerates it; review every capability diff.
- The `public_tool_surface` module of the `orbit-tools` `tools` binary sends invalid arguments to every registered `orbit.task.*` tool, inactive ones included. Dispatch refuses unknown fields and incompatible types before domain code; transport wrappers, optional nulls, numeric strings, string booleans and string/list forms stay accepted, and handlers still validate their own required fields and guarded modes.
- `agent_task_deletion_is_denied_through_every_dispatch_path` (CLI `mcp` binary, `mcp_roundtrip` module) covers runtime, CLI/MCP and local and remote MCP dispatch.
- A refusal fixture asserts the exact cause: the refusal kind and the tool, grant or ceiling named. A bare non-zero exit lets a bootstrap or sandbox failure pass as the policy refusal; for example, a global-scope plugin callback has no workspace root, so reusing the workspace callback path fails before policy runs.

### Coverage

[`.github/workflows/coverage.yml`](../.github/workflows/coverage.yml) runs `cargo llvm-cov nextest` ([cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov)) on `main` every six hours, once promoted there, and uploads the `coverage-lcov` artifact; dispatch it with `gh workflow run Coverage --ref main`. It never runs on pushes or PRs and **never gates a merge**; the host CI failure sweep files a red run like a red push. Like the `Check / Clippy / Test` job it uses nextest test groups and installs ripgrep for the provider inspection fixtures.

File-size fault injection keeps the hard limit and restores the soft `RLIMIT_FSIZE` before the child exits, even on unwind: LLVM writes profiles at exit, and a lowered limit truncates them and fails `cargo llvm-cov report`.

Line-coverage **targets** steer test investment; they do not fail CI. Check the CI log or run `cargo llvm-cov nextest -p <crate> --summary-only`.

| Crate | Target | Why |
|---|---|---|
| `orbit-policy` | > 90% | Policy evaluation is the security decision surface; on Linux it is the only enforcement layer. |
| `orbit-core` | > 80% | Composition root and command handling — regressions here surface everywhere. |
| `orbit-exec` | > 70% | Process spawning/sandboxing is platform-conditional, so some paths are unreachable on any single CI runner. |

### Reclaim workspace scratch

```sh
orbit gc tmp --workspace my-workspace --dry-run --json   # report .orbit/tmp entries (the default)
orbit gc tmp --workspace my-workspace --confirm          # empty it, keep the directory
```

`--confirm` (alias `--yes`) needs operator or runner authority and refuses while any job run in the workspace is `pending`, `running` or `retrying`; finish, cancel or reconcile those runs yourself. The report gives `entries_removed`, per-entry and total `bytes_reclaimable` (file and symlink lengths, not disk blocks) and `bytes_reclaimed`. Symlinks are unlinked, never followed; a symlinked `.orbit` or `tmp` refuses. Linux and macOS only. It covers only that checkout's scratch, and nothing schedules it.

## Dashboard HTTP integration fixtures

`cargo test -p orbit-web --test http_api` drives the public dashboard server over loopback HTTP; helpers and cases are in `crates/orbit-web/tests/http_api/`. Put routine API coverage there. `orbit-web` unit tests are for admitted security safeguards and deterministic shutdown and cache-publication races; dashboard JavaScript behavior belongs in a JS runner, not Rust tests that launch Node.

- Each mutable fixture re-executes its exact test with inherited authority cleared and launches `serve_from_env` against a disposable registry and workspace, under the [shared hang guard](#fixtures-on-a-loaded-host). Readiness and each HTTP request are bounded by `FIXTURE_STEP_DEADLINE` (120 s) with host load reported on expiry, and process guards reap servers after a failed assertion.
- Servers bind port zero and the launcher reads the address from the child's listening announcement. Never reserve and release a port: a concurrent fixture can take it and receive your first request.
- Submission fixtures use a harmless worker stub. The replay fixture runs the real worker through the ignored `replay_worker_child` entry point and waits for terminal run state before dropping its roots.
- The security table discovers literal routes from the API router and their mutating methods from HTTP `Allow` responses, then checks origin/Host protection on every mutation and operator admission on governed actions. A new mutating route defaults to operator-only; existing ordinary writes have explicit exceptions. The table asserts HTTP behavior, not source text; never expose a private router API for testing.

## Browser checks on a prepared host

Browser checks need Playwright, Chromium, its shared libraries and a font configuration. On a prepared host, `~/.local/chromium-deps/env.sh` exports `LD_LIBRARY_PATH`, `FONTCONFIG_FILE`, `PLAYWRIGHT_BROWSERS_PATH` and `PLAYWRIGHT_MODULE` (Playwright's `index.mjs`); source it first. Elsewhere, set `PLAYWRIGHT_MODULE` yourself and stage dependencies under `.orbit/tmp/` ([runbook](runbooks/website-validation.md#stage-playwright-and-chromium-without-root)). Skia FontConfig crashes and missing `lib*.so` errors are host evidence, not a failed check.

```bash
. ~/.local/chromium-deps/env.sh
node crates/orbit-web/src/tests/dashboard_operations_browser.mjs \
  "$PLAYWRIGHT_MODULE" .orbit/tmp/dashboard-browser
```

### Dashboard browser fixtures

Each runs as `node <script> "$PLAYWRIGHT_MODULE" .orbit/tmp/<evidence-dir>` and saves screenshots and any JSON named below. The dashboard is dark-only, so there is no light-theme pass. The loading and distributed scenarios also run in the QA sweep.

| Script | Evidence dir | Covers |
|---|---|---|
| `crates/orbit-web/src/tests/dashboard_loading_browser.mjs` | `loading-browser` | Task labels and navigation, workspace scope, responsive layout, row keyboard access, Ship dispatch. Task titles wrap to two lines with consistent row height; compact status/crew chips reveal native editors on focus or click. Checks title capacity beside the default dock at 1280×800 and 1440×900 and keyboard edits (`task-titles-result.json`). Review and in-progress rows and the detail link a task's pull request only when its `github-pr` ref has an http(s) page (`pull-request-links-result.json`). `--run-detail` narrows it to run detail, cancel/replay and child outcome tallies, state colors, durations and navigation at desktop/mobile widths (`run-detail-result.json`, `run-actions-result.json`, `run-child-outcomes-result.json`). |
| `crates/orbit-web/src/tests/dashboard_operations_browser.mjs` | `dashboard-browser` | Responsive shell at 375×812, panel layout and focus, Drain blockers and the **Proposed tasks** control, auto-task status tokens. |
| `crates/orbit-web/tests/http_api/dashboard_durations_browser.mjs` | `durations-browser` | Duration formats and wrapping in Metrics and Automation, running `↻` durations (`measurements.json`). |
| `crates/orbit-web/tests/http_api/dashboard_audit_browser.mjs` | `audit-browser` | Event and summary tables across widths, keyboard expansion, Events paging (Load older, shown-of-window header, refresh keeping loaded pages, status and hide-unconfirmed filters on every page), the Policy count explanation. |
| `crates/orbit-web/tests/http_api/dashboard_polish_browser.mjs` | `polish-browser` | Settings, Health and Automation deep links, crew labels and usage references, Crews geometry (`measurements.json`). |
| `crates/orbit-web/src/tests/dashboard_refresh_browser.mjs` | `refresh-browser` | Failed and recovered refreshes: retained counts with `as of HH:MM`, the connection dot, failed run streams. |
| `crates/orbit-web/src/tests/dashboard_host_switch_browser.mjs` | `host-switch` | Host switcher: requests routed through `/api/on/<host>/…`, host precedence, failure and recovery (`host-switch-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_doctor_browser.mjs` | `doctor-browser` | Health › Doctor rows, remediation, cached report age, remote and too-old hosts (`doctor-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_provider_limits_browser.mjs` | `provider-limits-browser` | Crews Limit column, Drain provider-limit waits, contrast under both emulated colour schemes (`provider-limits-assertions.json`). |

### Website changelog validation

After changing the changelog renderer, run `npm ci`, `npm run build` and `npm run check` in `website/`, serve `website/dist`, then:

```bash
node website/scripts/check-changelog.mjs \
  "$PLAYWRIGHT_MODULE" http://localhost:4187 .orbit/tmp/changelog-browser
```

It checks release dates, merged-PR links, version and subsection anchors, keyboard and no-JavaScript disclosure controls, the version badge and future-dated headings, and records heights and screenshots at 1440px and 375px in both themes. In a restricted executor, point `PLAYWRIGHT_BROWSERS_PATH` and `TMPDIR` under `.orbit/tmp/`; a minimal Linux runner may also need `FONTCONFIG_FILE` naming a config with local fonts.

## MCP Apps compatibility prototype

[The isolated reproduction and native desktop probe](qa/mcp-apps-probe.md) covers the Control Center, operator controls, rendered preview, routing boundaries and evidence template. Record automated protocol/bridge results and native desktop results separately.

## Workspace-local Rust toolchains

`proc.spawn` refuses, before the child starts, a default-profile rustup install whose `RUSTUP_HOME` is inside the workspace: the default and complete profiles install rust-docs, whose `share/doc/rust/html/core/macro.env.html` matches denyModify `**/*.env.*` even under `.orbit/tmp/`. A validation command that needs a workspace-local toolchain uses an absolute preprovisioned minimal one, or installs it the way `scripts/codeql-rust-local.sh` does:

```bash
rustup toolchain install <version> --profile minimal --component rust-src --no-self-update
```

Letting `cargo` or a rustup proxy auto-install into a workspace `RUSTUP_HOME` uses the default profile and is refused. Existing toolchains and roots outside the workspace (`~/.rustup`) are untouched. A managed Linux run that still creates `macro.env.html`, `.env`, `.env.local` or `secrets.env` on a committable path fails the post-run guard. Under the run's `.orbit/tmp/` scratch, which commit never includes, the guard removes the match instead, logs a warning and records it as a denied modify in the run audit; a test fixture there must not depend on the file surviving the step.

## Toolchain (MSRV)

- **MSRV:** `rust-version` in `[workspace.package]` of the root `Cargo.toml`, checked with `cargo check --workspace --locked` by the `msrv` job in `.github/workflows/ci.yml` and by `make ci-fast`. Keep the Makefile's `MSRV` and the workflow's `MSRV` variable equal to it.
- **Pinned toolchain:** [`rust-toolchain.toml`](../rust-toolchain.toml); every non-MSRV CI step that installs Rust repeats its version.

Bump the pinned toolchain in one PR: the toolchain file and those CI steps together, plus fixes for new compiler or Clippy diagnostics, with `make ci-fast` and `make ci-lint` passing on the new version. Leave the `msrv` job on the declared minimum. If a change needs a newer compiler or a dependency raises the MSRV, update `rust-version` and both `MSRV` variables in that same PR.

## Windows compile check

Windows is supported through [WSL2](runbooks/windows-wsl2.md). Native Windows is compile-checked only: `.github/workflows/ci-windows.yml` runs `cargo check --workspace --locked --all-targets --target x86_64-pc-windows-msvc`, so test targets must compile there, but no test runs. The check is advisory, not required.

- Code using Unix-only APIs (`libc`, `std::os::fd`, `std::os::unix`) sits behind `#[cfg(unix)]`; its `#[cfg(not(unix))]` counterpart returns an explicit unsupported error or `Unknown` and never invents Windows semantics.
- Tests that need those APIs or drive a `#!/bin/sh` fake are gated `#[cfg(unix)]` at the narrowest level (test, module, or `#![cfg(unix)]` for a POSIX-only file). Gate them; never delete them.

## Supply-chain (cargo-deny)

[`cargo-deny`](https://embarkstudios.github.io/cargo-deny/) gates dependencies on every open PR (through `scripts/ci-guardrails.sh`). [`deny.toml`](../deny.toml) denies crates with an open RUSTSEC advisory or a yanked version and limits licenses to a reviewed allow-list. Run it before landing a dependency change:

```bash
cargo install cargo-deny --locked   # once
make audit                          # scripts/cargo-deny.sh check
```

- **New license:** add its SPDX identifier to `[licenses].allow` only if it is permissive or public-domain-equivalent, with a comment naming the crate(s) and, for weak copyleft such as MPL-2.0, a justification. Copyleft that would bind Orbit's own sources is never added; replace the dependency.
- **Advisory exception:** only when no patched release exists (otherwise bump the dependency), as an `[advisories].ignore` object with `id` (`RUSTSEC-YYYY-NNNN`) and a `reason` saying why Orbit's use is safe plus a `Re-review YYYY-MM-DD` date, about six months out. Re-review by that date and remove the entry when a fix lands. `advisory-not-detected` on an entry means retire it: confirm it no longer matches, remove it, rerun. A reintroduced advisory is reviewed afresh.
- **Read-only advisory database:** set `CARGO_DENY_DB_PATH` (or `ORBIT_CARGO_DENY_DB_PATH`) to a writable directory; `scripts/cargo-deny.sh` points `[advisories].db-path` there. The invoking runner cleans it up.
- **Offline:** set `CARGO_DENY_DISABLE_FETCH=1` (or `ORBIT_CARGO_DENY_DISABLE_FETCH`, `CARGO_DENY_OFFLINE`, `ORBIT_CARGO_DENY_OFFLINE`) to check against a local clone of [`rustsec/advisory-db`](https://github.com/rustsec/advisory-db) (default directory `advisory-db-3157b0e258782691`) or the environment's fixture. Missing or stale data fails; it never passes by skipping.
- **YAML parser:** the `serde_yaml` dependency key aliases [`yaml_serde`](https://github.com/yaml/yaml-serde) 0.10.7, the YAML organization's continuation of the archived `serde-yaml`, with the same codecs, `Value`/`Mapping` API and error types, so persisted formats are unchanged. Its backend, [`libyaml-rs`](https://github.com/yaml/libyaml-rs) 0.3.0, is still a C-to-unsafe-Rust translation of libyaml; review upstream activity on upgrades. (`serde_yaml_ng` and `serde_norway` were staler; `serde-saphyr` lacks the compatible `Value` API.) After changing the parser, run `cargo test --locked -p orbit-types --lib` and `cargo test --locked -p orbit-tools --tests`.

## Vendored dashboard JavaScript

DOMPurify and marked are vendored under [`crates/orbit-web/assets/dashboard/vendor/`](../crates/orbit-web/assets/dashboard/vendor/); [`VENDOR.md`](../crates/orbit-web/assets/dashboard/vendor/VENDOR.md) has the refresh procedure. Refresh with `./scripts/refresh-dashboard-vendor.sh` after changing the pins in `package.json`; never edit the blobs by hand. `make ci-fast` runs [`check-dashboard-vendor.py`](../scripts/check-dashboard-vendor.py), which fails on a digest mismatch or drift between the manifest, `package.json` and `package-lock.json`. Dependabot and GitHub security alerts watch these packages, and the `dependabot-alert-sweep` job files remediation tasks; `cargo-deny` does not see them.
