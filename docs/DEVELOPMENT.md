---
type: context
summary: "Orbit development reference: test isolation, coverage, MSRV, supply chain, vendored JS"
last_validated: 2026-09-23
---

# Development Reference

The detailed rules behind [CONTRIBUTING.md](../CONTRIBUTING.md). Read the section that matches what you are touching.

## Validation and CI

Before implementation handoff or before-PR review, run these repository gates:

```bash
make ci-fast           # formatting and script guardrails; runs no Rust tests
make ci-test-affected  # complete test targets of changed crates and workspace dependents
make ci-lint           # clippy for production and all targets, then rustdoc
make goldens           # CLI/MCP, CI logs and sandbox profile goldens
```

`make ci-fast` runs no Rust tests. Its script fixtures and static guardrails
do not establish that the Rust test suite passes. Focused test filters help
investigate a change, but do not replace `make ci-test-affected`: an existing
test can encode behavior the candidate changes even when its new tests pass.
Hosted CI runs the full `make ci` for open PRs. Every PR-triggered workflow
first reads the live PR state through the GitHub API in a checkout-free
`Live PR state` job with only `pull-requests: read` permission. If the PR was
merged or closed while queued, all build and analysis jobs are skipped through
job-level conditions; their check names stay the same and GitHub treats skipped
jobs as successful required checks. API errors fail the gate visibly rather
than admitting an unknown PR state. A PR that closes after the gate has read
`open` can still finish its suite.

Push, scheduled and manually dispatched runs skip the state gate and keep
their existing suites and concurrency. In particular, agent-main/main push
CI and CodeQL analysis continue to cover merged changes. The offline
`scripts/test-pr-state-workflows.py` fixture executes each workflow's shell
gate against recorded API responses and checks job admission; `make ci-fast`
runs it alongside workflow YAML parsing.

The affected-test gate reads Cargo workspace metadata without compiling. It
selects each crate with changed paths, then all transitive reverse workspace
dependencies, including dev, build, renamed, optional and target-specific
dependencies. For example, changing `crates/orbit-core/` selects `orbit-core`,
`orbit-cmd`, `orbit-web` and `orbit-cli`. It runs their complete library,
binary and integration test targets with nextest (or Cargo when nextest is
unavailable), followed by Cargo doctests. Compilation and execution retain
the shared [build-budget admission](runbooks/build-budget.md).

Run Cargo-based gates (`ci-fast`, `ci-test-affected`, `goldens`, `ci-lint`, and
`cargo test` / `nextest`) one at a time when they share a target directory. To
overlap gates, assign each a separate `CARGO_TARGET_DIR`. Concurrent gates in
one target can rebuild `target/debug/orbit` while generation-bound fixtures
are running, causing spurious CLI fixture failures; F2026-10-118 records this
failure mode.

When the temporary directory is inside the checkout, the runner adds that
directory to `GIT_CEILING_DIRECTORIES` for test execution. This prevents
non-Git fixtures from discovering the managed checkout above them; existing
caller boundaries and sandbox permissions are preserved.
The runner also drops `ORBIT_WORKER_CONTEXT_REQUIRED` from test processes. A
claimed executor exports it so that Orbit commands its agent runs refuse to
start without their recorded worker binding; a test process has no binding,
so every in-process fixture that opens a runtime would fail with `managed
worker runtime binding unavailable`. The rest of the run envelope is passed
through, and tests of the refusal set the marker on their own child. A focused
`cargo test` inside a claimed executor needs the same treatment:
`env -u ORBIT_WORKER_CONTEXT_REQUIRED cargo test ...`.

To check whether a failure is pre-existing, replay the same command on the
unmodified base. Extract it into its own directory under `.orbit/tmp/` (for
example `git archive <base> | tar -x -C .orbit/tmp/base`), refresh its
timestamps, and give it its own `CARGO_TARGET_DIR`. Set
`GIT_CEILING_DIRECTORIES` to `.orbit/tmp` so its fixtures cannot discover the
managed checkout around the extract. Any target path works:
the `orbit update` fixtures that need a checkout build copy the tested binary
to a `target/debug/orbit` path of their own instead of relying on the shape of
Cargo's output directory. Replay the same nextest selection the gate ran;
fixture deadlines and the host's load are the same inputs on both arms only
when the runs are close together.

By default the comparison base is the merge base of `HEAD` with
`origin/agent-main`, or local `agent-main` when the remote ref is absent.
The diff includes committed, staged, unstaged and non-ignored untracked
paths; moves include both source and destination crates. A reviewer with a
pinned delivery base must use `CI_TEST_BASE=<base.commit> make ci-test-affected`.
The same variable accepts an exact revision for manual runs. An unavailable
base fails the gate rather than silently selecting nothing.

Cargo metadata cannot see files a test reads from outside its own crate. Those
edges are declared in `FILE_READERS` in `scripts/ci-test-affected.py` as a path
prefix and the crates whose tests read it; a changed path under the prefix
selects those crates and their reverse dependents. Today this covers
`crates/orbit-core/assets/jobs/` (read by `orbit-engine` and `orbit-cli`
tests), `crates/orbit-core/assets/activities/` (read by `orbit-engine`
tests, though `orbit-core` depends on `orbit-engine`, not the reverse),
`plugin/hooks/` and the root `server.json` (read by `orbit-cli` tests). When a
test starts reading another crate's or a repository-root file, add its prefix
there; the entry is not detected automatically. A declared crate absent from
current metadata fails the gate.

Changes to shared build inputs (`Cargo.toml`, `Cargo.lock`, `.cargo/`,
`.config/` or the Rust toolchain files), or a removed crate absent from
current metadata, select every workspace crate. A docs-only diff selects
nothing and passes without compiling or running Rust tests. Inspect the
selection with `python3 scripts/ci-test-affected.py --list` (and optionally
`--base <commit>`).

The owner workspace must include `make ci-test-affected` in
[`workflow.required_validation_commands`](CONFIG.md). This list
lives in ignored, per-user `.orbit/config.toml`, not in the source diff.
Once the owner and execution checkouts have the target, append the command
to the existing list, preserving every other requirement. For a workspace
whose only existing command is `make ci-fast`, the resulting setting is:

```toml
[workflow]
required_validation_commands = ["make ci-fast", "make ci-test-affected"]
```

The deterministic candidate-validation step and before-PR review then
require a passing affected-test record. Distributed review contracts freeze
the owner's list at admission; existing claims need a fresh admission after
a policy change.

## Filesystem writes

Replace durable files through `orbit_common::fs::io::atomic_write_text` or
`atomic_write_bytes`. These stage a private sibling file, sync its contents,
rename it into place, and sync the parent directory. Existing file permissions
are preserved. Use `StagedTextFile` when validation must run between staging
and commit; dropping an uncommitted stage removes it.

The production Clippy pass disallows `std::fs::write`, including `fs::write`
aliases. Keep any exception scoped to the intentional write and name its
reason in `#[allow(clippy::disallowed_methods, reason = "...")]`. Current
exceptions preserve in-place edits of user TOML configs linked from dotfiles,
artifact exports to user-selected paths (including symlinks and devices),
and generated scaffold sources with their existing `--force` behavior. These
writes do not promise crash-safe replacement. The explicitly non-atomic
`write_text_with_parent` helper is for deliberate new-file or disposable writes; use
`write_new_private_text` for exclusive creation of new private files. Test
fixtures may use direct writes under the separate all-targets lint pass.

## Browser checks on a prepared host

The website and dashboard browser checks below need Playwright, a Chromium
build, its shared libraries and a font configuration. A host can prepare
these once instead of having each run download a browser into `.orbit/tmp/`.
The prepared host provides `~/.local/chromium-deps/env.sh`. When that file
exists, source it before a browser check:

```bash
. ~/.local/chromium-deps/env.sh
node crates/orbit-web/src/tests/dashboard_operations_browser.mjs \
  "$PLAYWRIGHT_MODULE" .orbit/tmp/dashboard-browser
```

The file exports `LD_LIBRARY_PATH`, `FONTCONFIG_FILE`,
`PLAYWRIGHT_BROWSERS_PATH` and `PLAYWRIGHT_MODULE`. Pass `$PLAYWRIGHT_MODULE`
wherever a command below asks for `/absolute/path/to/playwright/index.mjs`.
Without the file, set up the dependencies under `.orbit/tmp/` as the sections
below describe. Skia FontConfig crashes and missing `lib*.so` errors come from
the host environment, not the change under test. Record them as host
evidence; don't count them as a failed check.

## Website changelog validation

After changing the changelog renderer, run `npm ci`, `npm run build`, and
`npm run check` in `website/`. Serve `website/dist` locally, then use an
installed Playwright module and Chromium to check the rendered page:

```bash
node website/scripts/check-changelog.mjs \
  /absolute/path/to/playwright/index.mjs http://localhost:4187 .orbit/tmp/changelog-browser
```

The check covers release dates, merged-PR links, preserved version and
subsection anchors, keyboard and JavaScript-free disclosure controls, the
version badge, and future dated headings. It records page heights and
screenshots at 1440px and 375px in both themes. Browser dependencies may live
under `.orbit/tmp/`; use `PLAYWRIGHT_BROWSERS_PATH` for a local browser cache
and `TMPDIR` for scratch profiles when required by the executor. Minimal Linux
runners may also need `FONTCONFIG_FILE` pointing to a configuration with local
fonts and a cache under `.orbit/tmp/`. The check selects the site's saved theme
explicitly and requires its web fonts to load before measuring page height.

## MCP Apps compatibility prototype

See [the isolated reproduction and native desktop probe](qa/mcp-apps-probe.md)
for the versioned Control Center, operator controls, isolated rendered preview,
routing boundaries and evidence template.
Automated protocol/bridge results and native desktop results are recorded separately.

## Dashboard HTTP integration fixtures

`cargo test -p orbit-web --test http_api` drives the public dashboard server
over loopback HTTP. All cases share one integration binary; helpers and cases
live under `crates/orbit-web/tests/http_api/`. Each mutable fixture re-executes
its exact test with inherited authority cleared, then launches the public
`serve_from_env` entry point against a disposable registry and workspace.
Child tests run under the shared child-test hang guard (see
[Fixtures on a loaded host](#fixtures-on-a-loaded-host)), server readiness has
a 10-second deadline, and HTTP requests (including SSE reads) have a 5-second
timeout.
Process guards kill and reap servers even after an assertion fails.

HTTP fixture servers bind port zero and the launcher reads the actual address
from that child's listening announcement before probing health. Do not reserve
and release a port in the launcher: another concurrent fixture can bind it
before the intended child, and its successful health response can route the
first API request into the wrong disposable workspace. The friction projection
case keeps an empty dashboard alive alongside the seeded dashboard to check
first-request isolation as well as month/limit memo reuse. This is a fixture
startup race; no production audit visibility defect has been demonstrated.

Submission fixtures normally substitute a harmless worker stub. The replay
fixture instead re-executes the exact ignored `replay_worker_child` entry point
against its disposable registry and calls the real pipeline worker. A sleep
longer than the HTTP timeout proves that replay returns before execution finishes;
the fixture waits for terminal run state before dropping its temporary roots.

The security table discovers literal paths from API router registrations and
uses HTTP `Allow` responses to enumerate their mutating methods. It checks
origin/Host protection on every mutation and operator admission on governed
actions. Existing ordinary writes have explicit method/path exceptions;
new mutating routes default to operator-only. The auto launch probe requests
completion authority, which is governed separately from a normal launch.
Resume probes use saved source runs with review and done completion policies,
so both ordinary admission and inherited completion authority are exercised.
This is source-assisted discovery followed by behavioral HTTP assertions,
not a source-text snapshot. No private router API is exposed for testing.

The remaining `orbit-web` unit tests cover admitted security safeguards and
deterministic shutdown/cache-publication races; routine API coverage lives in
the HTTP integration suite. Dashboard JavaScript behavior belongs in a JS
runner, rather than Rust tests that launch Node. The standalone loading and
distributed browser scenarios remain part of the QA sweep, and the Operations
browser scenario remains available with its shared fixtures under
`crates/orbit-web/src/tests/`.

The loading browser fixture also renders run detail at 1440px: held-state
color and hold reason, one-based step numbering and singular counts, empty
log feedback, omitted empty knowledge metrics, disabled active replay,
timeline legend and column labels, and formatted stream-JSON stdout. It
checks that long log lines wrap inside the panel, malformed lines stay
readable as text, and each stream's wrap toggle restores horizontal scrolling
without widening the page. It also drives the full app's cancel/replay actions:
settlement counts and errors remain visible after refreshes and scheduled
polls (including a failed detail read and recovery), dismissal and subsequent
actions clear feedback, and changing run or
workspace retires it. It saves screenshots, `run-detail-result.json`, and
`run-actions-result.json`:

```bash
node crates/orbit-web/src/tests/dashboard_loading_browser.mjs \
  /absolute/path/to/playwright/index.mjs .orbit/tmp/loading-browser --run-detail
```

The loading browser fixture also opens fresh single- and multi-workspace
dashboards. It checks that the sole Audit workspace chip has no remove control,
other Audit filters remain removable, and Operations, Drain start/stop and
distributed approve/revoke/recover succeed with explicit workspace scope.
With multiple workspaces, removing the chip selects the aggregate view and
the picker restores a concrete scope. These scenarios run with both the full
suite and `--run-detail`.

Omit `--run-detail` to include the full loading and responsive-layout suite,
which also checks row keyboard access: no task, run or friction row nests a
control inside a button, each row list is one Tab stop with Up/Down between
rows, Enter and Space toggle a task row, its disclosure keeps focus across a
refresh that rebuilds the row, and `/` focuses the task search.
It also dispatches Ship from the detail and row controls, suppresses duplicate
clicks while the request is pending, preserves failure feedback and retry after
a concurrent refresh, surfaces the server's conflict for a live run, and enables
both controls after the task returns to backlog without a reload.

The duration browser fixture checks Metrics at 1440px and 1024px with 55
actor rows (165 duration cells), using text ranges to detect line wrapping.
It also verifies that Automation uses the shared duration format and that
running run and step durations show elapsed time with `↻`, advance on
refresh, and switch to recorded durations on completion. Sub-hour durations
retain milliseconds, decimal seconds and unpadded seconds; longer durations
roll into hours/minutes or days/hours. It saves screenshots and
`measurements.json`:

```bash
node crates/orbit-web/tests/http_api/dashboard_durations_browser.mjs \
  /absolute/path/to/playwright/index.mjs .orbit/tmp/durations-browser
```

The Operations browser fixture also checks the responsive dashboard shell. At
375×812, the brand, workspace picker, Drain indicator and Refresh icon share
the header row; destinations and the active section's views share a scrolling
navigation row, and the three host chips sit in one row with every reading
in view. The fixture checks panel position on Tasks, Runs, run detail and
Incidents, keyboard and pointer access with visible focus, count spacing, and
page width across routes.
The Drain fixture distinguishes host throttling, stopped admissions, workspace
leaf saturation and task conflicts. It checks window, workspace and pool count
labels and verifies that both blocker task IDs remain visible at 1024px while
the lock path truncates. It also checks that the **Proposed tasks**
(approve-proposed) control, its disabled reasons and the live window's approvals
line fit the card at 336px and 375px, and captures `drain-approve-*` card
screenshots. The dashboard is dark-only, so there is no light-theme pass.
Run it with an installed Playwright module and put evidence in `.orbit/tmp/`:

```bash
node crates/orbit-web/src/tests/dashboard_operations_browser.mjs \
  /absolute/path/to/playwright/index.mjs .orbit/tmp/dashboard-browser
```

The Audit browser fixture exercises the shipped event and summary tables at
1440, 1024, 1920 and mobile widths, keyboard expansion, duplicate targets and
the Policy count explanation with filtered and independent-window views:

```bash
node crates/orbit-web/tests/http_api/dashboard_audit_browser.mjs \
  /absolute/path/to/playwright/index.mjs .orbit/tmp/audit-browser
```

The dashboard polish browser fixture checks every Settings, Health and
Automation deep link on load and reload at 375px, plus hash changes. Long view
lists scroll within the space beside their destination so both selections stay
visible. It also checks Crews header geometry, column alignment and consistent
empty values at 1280, 1440 and 1920px, and full-name Audit tooltips for tools
with shared prefixes. It saves screenshots and `measurements.json`:

```bash
node crates/orbit-web/tests/http_api/dashboard_polish_browser.mjs \
  /absolute/path/to/playwright/index.mjs .orbit/tmp/polish-browser
```

The refresh browser fixture loads the complete dashboard, aborts `/api/*` with
Playwright after a successful load, and checks retained rail counts and the
Drain pill at 1440px and 768px. Failed refreshes dim these surfaces;
their titles show the last clean refresh as `as of HH:MM`. The connection dot
is red for a network failure, amber for a panel failure, and green after a
clean refresh. The rail names failed panels, and recovery clears stale styling
and titles. Host resource chips retain their independent sample-age handling.
The fixture also checks repeated failures and failed or absent run streams:

```bash
node crates/orbit-web/src/tests/dashboard_refresh_browser.mjs \
  /absolute/path/to/playwright/index.mjs .orbit/tmp/refresh-browser
```

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
[`crates/orbit-cli/tests/tool/tool_list.rs`](../crates/orbit-cli/tests/tool/tool_list.rs).
For a complete disposable fixture, create absolute temporary paths, make the
checkout a real Git repository, initialize the fixture workspace through that
helper, then perform a read-only routing check before adding tasks or starting
runs:

```rust
let temp = tempfile::tempdir().expect("fixture tempdir");
let home = temp.path().join("home");
let work = temp.path().join("work");
fs::create_dir_all(&home).expect("fixture home");
git_repo::init(&work); // crates/orbit-cli/tests/support/git_repo.rs

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

Root discovery asks Git where the checkout ends, and Git walks past a plain
directory or an empty `.git` directory. With TMPDIR inside another checkout,
such as a managed worktree's `.orbit/tmp`, a fixture without its own repository
resolves the enclosing checkout's Orbit root: `workspace init` then refuses
with that checkout's identity, or, inside a managed run, fails on its read-only
`.orbit` mount. Never answer that with `--force`. Give every fixture checkout,
including a negative-control cwd, its own repository. Ordinary (non-bootstrap)
lookup and the plugin's SessionStart hook still accept an initialized `.orbit`
at any ancestor, so a control that needs "no workspace above this directory"
must check that precondition and report a skip rather than pass under an
enclosing workspace.

A shell `export HOME=/tmp/...` is not an isolation boundary: a
managed child can inherit `ORBIT_MANAGED_RUN_CONTEXT` and the
`ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair, which carries durable authority
and takes precedence over home discovery. Never use bare mutable fixture CLI
commands against ambient authority in a managed worker. See
[`crates/orbit-cli/tests/workspace/ambient_authority_isolation.rs`](../crates/orbit-cli/tests/workspace/ambient_authority_isolation.rs)
for the regression coverage.

Apply the same shared scrub to fixture Git setup commands. `GIT_DIR`,
`GIT_WORK_TREE`, and `GIT_COMMON_DIR` override cwd independently of Orbit's
environment. Leaving them inherited can make `git init` initialize the parent
repository and route a workflow's Git fetch lock into the parent checkout.

Workflow-dispatch fixtures must also observe their detached workers reaching a
terminal state in the fixture's run history before dropping its temporary home
and checkout. A successful submission only proves admission, not where the
worker opened its store. Check that the fixture records the expected runs and
that a disposable parent registry presented through the managed environment
gains no runs. The output-goldens fixture exercises this boundary with a
task-pilot dispatch that stops before any agent work.

Live `bwrap` spawn is not available from inside an agent-executor or job-run
worktree. The outer sandbox blocks nested `unshare(CLONE_NEWUSER)`, so even
`bwrap --ro-bind / / --tmpfs /tmp --dev /dev -- echo works` fails with
`No permissions to create new namespace`. Treat that denial as nested-sandbox
environment, not a missing AppArmor profile or a product defect. Replay live
spawn checks (`spawn_under_linux_bwrap`, `--run-ignored`) on the owning Linux
host. Do not disable `linux-bwrap` or try to make bwrap nest from a fixture.

The Bubblewrap environment-forwarding and credential-mask fixtures re-execute
their exact test in a separate process with a cleared environment and a private
temporary `HOME` outside `/tmp`. The launcher compiles credential masks from its
own `HOME` and `CARGO_HOME`, so changing those variables in the parallel harness
would let other tests observe a fixture home while it is created or deleted.
Inject fixture variables through the child command instead of process-wide env
guards; the parent keeps the home alive until the isolated test exits.

The `plugin_secrets` module of the CLI `tool` integration binary also runs
`secret_sinks::seeded_secrets_stay_out_of_persistence_logs_and_children`:

```bash
cargo test -p orbit-cli --test tool plugin_secrets::secret_sinks::seeded_secrets_stay_out
```

One isolated child seeds fake provider, SCM, cloud and database credentials
before redactor initialization. It drives task writes and provider output through
the CLI, then checks persisted task bundles, command/run audit rows, audit blobs,
run logs, an attached diagnostic artifact, dashboard HTTP log views and the
provider's actual environment. Every leak reports its sink. A hostile legacy
process-log record also exercises dashboard rendering independently of the
producer's redaction. Raw task attachments retain their byte-preserving contract;
the attached diagnostic comes from the run-log producer. The fixture has a
120-second child deadline, 30-second CLI deadlines, 10-second provider and
dashboard readiness deadlines, and 5-second HTTP timeouts. Its disposable
executor explicitly uses `sandbox: off`; it tests redaction and environment
composition without nesting an OS sandbox.

## Testing & Coverage

### Reclaim workspace scratch

`orbit gc tmp` reports each top-level entry under the selected workspace
checkout's `.orbit/tmp`, including nested run and step-recovery scratch.
The default and `--dry-run` leave files intact; `--confirm` (or `--yes`)
removes all contents and keeps the empty directory. Use `--workspace <selector>`
to select a registered workspace and `--json` for a structured report:

```sh
orbit gc tmp --workspace my-workspace --dry-run --json
orbit gc tmp --workspace my-workspace --confirm
```

Confirmed collection requires operator or runner authority, as worktree
collection does. It refuses while any job run in that workspace is `pending`
or `running`, naming the run IDs; it never reconciles stale owners as part of
collection. Finish, cancel, or explicitly reconcile those runs first. Other
workspace checkouts and job worktrees' scratch are outside this command's scope.

Reports include `entries_removed` (top-level entries), per-entry and total
`bytes_reclaimable`, and `bytes_reclaimed` (zero during preview). Byte counts
sum regular-file lengths and symlink lengths; they exclude directory metadata
and do not estimate physical blocks or account for shared hard links. Symlinks
inside scratch are unlinked without traversing their targets; a symlinked
`.orbit` or `tmp` directory refuses collection. Linux and macOS use pinned
directory descriptors for traversal and removal; other platforms refuse the
command. Reports display non-UTF-8 names with replacement characters while
removal uses their original byte names. Collection is an explicit operator
action, with no routine scheduling.

### Authorization coverage

Authorization coverage is generated from the live governed-operation and
builtin tool registries by `authorization_matrix_matches_live_registry` in the
CLI `output` integration binary (module `output_goldens`). `make goldens UPDATE=1` regenerates
its operation/capability/caller verdict table; review every capability diff.
The `public_tool_surface` module of the `orbit-tools` `tools` integration
binary dispatches invalid arguments to
every registered `orbit.task.*` tool, including inactive tools, and checks each
declared parameter. Task dispatch refuses unknown fields and incompatible
types before domain execution. Transport wrappers remain supported, as do
existing optional nulls, numeric strings, string booleans and string/list
forms. Handler-specific required fields and guarded modes are still validated
by their handlers. `agent_task_deletion_is_denied_through_every_dispatch_path`
in the CLI `mcp` binary's `mcp_roundtrip` module exercises runtime, CLI/MCP dispatch and local/remote MCP
sessions in a disposable child process with a 120-second deadline.

CI collects workspace test coverage with
[`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) on every open PR
(the `Coverage (informational)` job in `.github/workflows/ci.yml`) and
uploads an lcov report as the `coverage-lcov` workflow artifact. The job is
**informational only — it never gates a merge**. It runs the tests through
`cargo llvm-cov nextest`, so each test gets its own process and the
`.config/nextest.toml` test groups apply, as in the `Check / Clippy / Test` job.
Both Linux jobs install ripgrep before running tests: provider inspection
fixtures execute real `git` and `rg` commands inside a pinned checkout, so
coverage needs the same host tools as the regular test job.

File-size fault injection must preserve the hard resource limit and restore
the soft limit before the isolated child exits, including when assertions
unwind. LLVM writes coverage profiles at process exit; leaving `RLIMIT_FSIZE`
lowered can truncate a profile even when the test passes and make the later
`cargo llvm-cov report` fail. The routine staging-write fixture restores its
limit with a drop guard and verifies a write larger than the injected limit.

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

### Fixtures on a loaded host

`make ci-test-affected` runs at nextest's default concurrency, often beside a
busy drain. On a CPU-saturated host a fixture's wall-clock time stretches by
an order of magnitude, so a deadline sized from an idle run fails a passing
fixture. Keep fixture outcomes independent of host load:

- Re-execute a child test through `orbit_common::test_env::run_child_test`,
  then verify it with `assert_child_test_passed`. Its `CHILD_TEST_DEADLINE`
  (300 seconds) is a hang guard below nextest's ten-minute termination. An
  overrun kills the child's process group and fails with the host's load
  averages and everything the child printed, instead of a bare `exceeded`.
- A wait for an observable event ends when the event arrives. Size its ceiling
  for a saturated host, not the idle duration, and report what it saw and
  `orbit_common::test_env::host_load()` when it expires.
- Admission reads the host resource monitor. A fixture whose subject is not
  throttling installs a pinned calm sample with
  `OrbitRuntime::with_host_resource_probe` on every runtime it opens, including
  one reopened over the same roots; otherwise a loaded test host throttles
  the admissions it counts. A throttling test injects and drives its own probe.
- Bound work, not elapsed time: measure a cost guard in CPU time where the
  platform allows, so scheduling delay cannot fail it.
- A fallback that production takes under load (an interactive login probe
  past its timeout, for example) may satisfy a fixture only when the result
  reports that fallback and its reason; a silent fallback still fails.

A refusal fixture asserts the exact authorization cause it guards: the
refusal kind and the denied tool, grant or ceiling it names. Asserting only a
non-zero exit or a generic error lets a bootstrap or sandbox failure (a global
root that is not a workspace, `Permission denied` from the fixture's own
path) pass as the policy refusal. For example, a global-scope plugin
callback leaves the tool context's workspace root unset, so a fixture that
reuses the workspace CLI callback path fails before any policy runs.

### Tests that depend on host process visibility

A few suites derive a process-start identity token with
`orbit_common::process::identity`. Linux reads `/proc/<pid>/stat` and the
kernel boot time without needing `ps` on `PATH`. macOS tries libproc first;
other Unix hosts use `ps -o lstart=`. All stable tokens retain the UTC /
C-locale `ps` rendering, so persisted tokens remain compatible. Legacy tokens
written in the caller's local environment still have a `ps` fallback.
Unreadable kernel data or an unavailable fallback can make a probe
`Unavailable`; production then takes its documented fail-safe branch:
an owner it cannot verify is neither finalized nor signalled.

A test whose subject *is* the derived token must therefore ask before
asserting, rather than fail for a reason unrelated to the code under test.
Call `orbit_common::test_env::start_identity_probe_blocker()`: `None` means
the probe works and the full assertion applies; `Some(reason)` names the
constraint, and the test either returns early or asserts the fail-safe
branch, logging `reason` so the choice is attributable from the log. Never
weaken the assertion taken when the probe *is* available.

### Long-lived CLI fixture cleanup

Every dashboard and persistent MCP child in `crates/orbit-cli/tests` must be
owned immediately after spawn by `tests/support/child_guard.rs`, before pipe
extraction or readiness assertions. The shared `ChildGuard` sends SIGTERM on
Unix, allows two seconds for exit, then force-kills and reaps the child. It
also reaps on assertion unwinds and retains the original child PID across
executable handover. Explicit shutdown tests can still signal and wait on the
child; dropping an already-reaped guard does not signal that PID again.
The process integration suite forces assertion failures against listening
and handed-over dashboards, then verifies PID exit and port closure.

### Process fixture readiness

Process fixtures wait for observable readiness with a generous startup ceiling;
they measure signal shutdown separately. The detached-worker registry-lock
fixture observes the real worker's persisted run with a read-only SQLite
connection and keeps the registry writer held through claim and completion.
This avoids conflating total CLI startup time or a competing CLI participant's
generation admission with registry-lock independence.

Detached resume lifecycle fixtures wait for the recorded worker to exit before
checking the task's final status and history. Run terminalization is persisted
before coupled-task cleanup, so observing a terminal run alone can still see a
readmitted task in progress. Verify the recorded PID and start identity while
waiting, and retain the bounded deadline and final task assertions.
The cleanup ordering regression holds the task commit boundary while another
thread finalizes the run, then verifies the task block after that thread exits.

The parent-signal fixture selects the proc tool from MCP `tools/list`, using the
CLI when that tool is not advertised. Its child writes a marker under the
fixture's own directory with a shell builtin and a quoted positional argument,
including when the temporary path contains spaces. Readiness waits detect early
supervisor exit and retain its output; process guards reap children on failure.
The SIGINT/SIGTERM shutdown ceiling remains ten seconds in the host and box
lanes. Do not add launch retries or relax shutdown assertions to hide startup
failures.

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
  or canonicalizes the path it compares against. On unrestricted hosts, keep
  the platform's default `TMPDIR` so the suite exercises symlinked ancestors.
  Managed executors must instead put scratch and validation logs beneath their
  injected `ORBIT_SCRATCH_DIR` (the checkout's `.orbit/tmp`): create a run
  subdirectory there and set `TMPDIR` to it. Tests that guarantee symlink
  handling must construct that case explicitly, rather than depend on the
  platform's default temporary path. The [cross-revision helper](runbooks/compiler-cache.md#cross-revision-beforeafter-validation)
  requires this explicit scratch root to allow workdirs beneath `.orbit`,
  while retaining source-checkout and live-state refusals.
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

## Workspace-local Rust toolchains

`proc.spawn` refuses a default-profile rustup toolchain install whose resolved
`RUSTUP_HOME` is inside the workspace. The default and complete profiles write
rust-docs, including `share/doc/rust/html/core/macro.env.html`. denyModify
`**/*.env.*` matches that name even under `.orbit/tmp/`: the earlier
`!.orbit/tmp/**` exception does not win. The refusal is returned before the
child starts, and the error names the install and that deny rule.

A validation command that needs a toolchain inside the workspace must use an
absolute preprovisioned minimal toolchain, or install with
`rustup toolchain install <version> --profile minimal --component rust-src --no-self-update`,
as `scripts/codeql-rust-local.sh` does. `--profile minimal` does not install
rust-docs. Setting `RUSTUP_HOME` inside the workspace and letting `cargo` or
another rustup proxy auto-install a missing toolchain uses the default profile
and is refused. A toolchain already installed under that root is left alone, as
is an install whose root is outside the workspace (the usual `~/.rustup`).

The secret globs are unchanged. A managed Linux run that still creates
`macro.env.html`, `.env`, `.env.local`, or `secrets.env` fails the post-run
guard.

## Toolchain (MSRV)

Orbit's minimum supported Rust version is declared as `rust-version` in the
workspace `Cargo.toml` (`[workspace.package]`) and enforced by the `msrv` job
in `.github/workflows/ci.yml` (`cargo check --workspace --locked` on the
pinned toolchain). If a change genuinely needs a newer compiler or a
dependency bump raises the floor, bump `rust-version` and the workflow's
`MSRV` env var together in the same PR, and call it out in the CHANGELOG.

## Windows compile check

Windows is supported through WSL2. Native Windows is compile-checked only:
`.github/workflows/ci-windows.yml` runs `cargo check --workspace --locked
--all-targets --target x86_64-pc-windows-msvc` on `windows-latest`, so test
targets must compile there too; tests never run on Windows. It is advisory, not
a required status check. Code that needs Unix-only APIs (`libc`, `std::os::fd`,
`std::os::unix`) must sit behind `#[cfg(unix)]`. Its `#[cfg(not(unix))]`
counterpart must return an explicit unsupported error or `Unknown`; it must not
invent Windows semantics. A test that needs those APIs, or drives a `#!/bin/sh`
fake, is gated `#[cfg(unix)]` at the narrowest sensible level: the test, its
module, or `#![cfg(unix)]` for a wholly POSIX file. Gate it; never delete it.

## Supply-chain (cargo-deny)

Dependencies are gated by [`cargo-deny`](https://embarkstudios.github.io/cargo-deny/)
on every open PR (via `scripts/ci-guardrails.sh`) and locally with `make audit`. The
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
