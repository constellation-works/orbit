---
type: context
summary: "Orbit development reference: validation, tests, docs index, MSRV, supply chain, vendored JS"
last_validated: 2026-10-24
---

# Development Reference

The detailed rules behind [CONTRIBUTING.md](../CONTRIBUTING.md). Read the section that matches what you are touching.

| You are… | Read |
|---|---|
| Handing off or reviewing a change | [Validation and CI](#validation-and-ci) |
| Writing files or spawning processes in production code | [Filesystem writes](#filesystem-writes), [Child processes](#child-processes) |
| Writing a test that mutates state, spawns processes or submits runs | [Safe Mutable CLI Fixtures](#safe-mutable-cli-fixtures), [Testing & Coverage](#testing--coverage) |
| Running a mutating `orbit` command by hand for QA, from a managed lane | [Manual QA from a managed lane](#manual-qa-from-a-managed-lane) |
| Changing the dashboard or website | [Dashboard fixtures](#dashboard-http-integration-fixtures), [Browser checks](#browser-checks-on-a-prepared-host) |
| Changing the desktop app in `desktop/` | [Desktop app](#desktop-app) |
| Changing toolchains, platforms or dependencies | [MSRV](#toolchain-msrv), [Windows](#windows), [Supply-chain](#supply-chain-cargo-deny), [Vendored JS](#vendored-dashboard-javascript) |

## Validation and CI

For tasks that deliver a diff, there is no required local gate. The required PR checks (Check / Clippy / Test and Test shard 1/3, 2/3 and 3/3) gate formatting, guardrails, lint, rustdoc, goldens and the whole test suite, so the targets below are focused tools, not blanket pre-handoff gates. Run the ones the change needs:

```bash
make ci-guards         # static repository guards; no build or network
make ci-fast           # MSRV compile check, formatting, script guardrails; no Rust tests
make ci-test-affected  # complete test targets of changed crates and their workspace dependents
make ci-lint           # clippy for production and all targets, then rustdoc
make goldens           # CLI/MCP, CI log, sandbox and SQLite ledger schema goldens; UPDATE=1 regenerates
```

Run `make goldens` when the change touches a golden surface (CLI help, MCP text, sandbox profiles, ledger schemas), and the tests of the behavior you changed. A red required check on the PR is repaired through the PR (auto-fix or a follow-up commit). `workflow.required_validation_commands` in the workspace config sets what the pipeline runs.

When an edit changes any generated row in `docs/INDEX.md` (for example, indexed Markdown frontmatter, its first heading, or summary), run `make docs-index`, review the generated index, and run `scripts/generate-doc-indexes.sh --check`. Include any resulting `docs/INDEX.md` change with the source edits, even when `make ci-fast` is pipeline-owned final validation.

When renaming or consolidating a test selected by macOS CI, update the filter
in `.github/workflows/ci-macos.yml` and run `make check-ci-macos`. This lists
actual compiled tests, including the workspace feature set, so a filter cannot
silently become a zero-test pass.

The macOS and CodeQL workflows do not run on pull requests: GitHub's
hosted-runner concurrency was the merge bottleneck, and none of their checks is
required. They run on `agent-main` and `main` pushes (CodeQL also weekly), and a
failure there becomes a repair task, as a CodeQL alert does. Website checks run
on pull requests only when they change `website/**`, `CHANGELOG.md` or the
website workflow, and on `main` pushes with the same paths. Their check names stay stable:
`macOS Clippy (informational)`,
`macOS Sandbox` and `Check and build`. The historical `informational` label
does not weaken Clippy: the job runs `make ci-lint` and fails on errors.
`scripts/check-ci-macos.sh` requires the workflow to run on `agent-main` pushes and retains exact
target/filter listings and native execution evidence validation. Push runs of
`ci-macos.yml` and `codeql.yml` share one group per branch with cancellation off: one run
executes and one waits, and a newer push replaces the waiting run. An
intermediate commit can therefore get no macOS run, while the newest commit
always does. Scheduled and dispatch runs keep a group per run. Required-check
settings are managed separately in the repository's provider rules.

The task and MCP CLI integration binaries use named contract tables, with
four and eight nextest slots respectively. Each row keeps its assertions and disposable state; rows that
share a workspace address independent task IDs. Exact-child fixtures isolate
the complete table once, rather than re-executing removed test entry points.
The [MCP and task test budget report](dev/reviews/cli-mcp-task-test-budget.md)
records the coverage mapping and candidate-host before/after measurements.

Tasks tagged `no-diff-expected` run no workspace gates or required validation
commands and attach no `no-diff.json` or workspace validation logs, in owner or
claimed mode. Run only commands the task's own brief requires; a QA sweep may
build and run the product, and a review may reproduce a finding when its brief
asks for it. Keep the brief's evidence artifacts and execution summary.

- `make ci-guards` (or `scripts/ci-guardrails.sh --static`) runs the file and
  metadata-only subset of the fast repository guards before candidate handoff.
  It includes unit-test admissions, QA surface inventory, public artifact IDs,
  test shell waits, dependency direction, orphan modules, doc links and indexes,
  and the skill mirror check. The QA inventory uses `--check --static` to omit
  its fixture self-tests and explicitly request offline metadata. Cargo is used
  only for metadata, with `CARGO_NET_OFFLINE=true`; no compilation, formatting,
  Rust tests or network requests run. It needs Python 3.11+, PyYAML, ripgrep and
  an installed Cargo toolchain. The networked workflow action-pin check, Node UI
  behavior tests, scratch plugin installation, and all fixture self-tests remain
  in `make ci-fast` and full CI. The command uses the same bounded pool, names
  failing guards, returns a nonzero status on failure, and prints per-guard and
  total wall times. This early check does not replace any required delivery gate.
  For a diff, run it and fix its findings even when `make ci-fast` is reserved for
  pipeline final validation. Run `cargo fmt --all -- --check` in Rust workspaces,
  including test targets. Include changed test files when applying rustfmt fixes,
  and check them explicitly with `rustfmt --check --edition <edition> <path> ...`
  using their crate's edition; do not limit formatting to production files.
  For Rust changes, also mirror both `ci-lint` Clippy
  passes and warnings-denied rustdoc for each changed crate, sequentially:

  ```bash
  scripts/build-budget.py -- cargo clippy -p <crate> --lib --bins -- -D warnings -D clippy::disallowed_methods
  scripts/build-budget.py -- cargo clippy -p <crate> --all-targets -- -D warnings -A clippy::disallowed_methods
  RUSTDOCFLAGS="-D warnings" scripts/build-budget.py -- cargo doc --no-deps -p <crate>
  ```

  Replace `<crate>` with the package name. For a crate with no library
  target (`orbit-cli`), use `--bins` in place of `--lib --bins` in the
  production Clippy command. Keep the whole final-validation
  commands with the pipeline. These scoped checks cover production channel
  restrictions, test targets, and private/broken documentation links.
- `make ci-fast` runs independent repository script checks through a background pool. Its width is the build budget's Cargo job count (`scripts/build-budget.py --cargo-jobs`: `ORBIT_CARGO_JOBS`, then `CARGO_BUILD_JOBS`, then the `cargo-jobs` file, default 4), so guard fan-out never exceeds the same budget Cargo receives. The pool is a sliding window over launch order: a new guard starts only when the oldest launched guard finishes, even if later guards finished earlier, so a slow early guard keeps the slots of those finished guards occupied until it ends. Each script's output is captured separately and printed in launch order, per-script wall times are printed, and every failing script is named before the first failure's exit code is returned. Fixture self-tests run only when their inputs change: `scripts/`, `.github/`, `Makefile`, `.cargo/`, the root Cargo manifest/lockfile, `install.sh`, `RELEASING.md`, `plugin/`, `npm/`, `.agents/` or `.claude-plugin/`. Self-tests that assert wall-clock time or timeouts, measure build-budget admission, or drive a real Cargo target run one at a time, at normal priority, in a separate exclusive lane: `test-build-budget.sh`, `test-ci-fast-guards.py`, `test-ci-test-affected.py`, `test-codeql-rust-local.py`, `test-compiler-cache.sh` and `test-compiler-cache-namespaces.sh`. The lane starts after formatting, beside the pool. Each pool guard and, in full mode, the workflow test build starts in Linux's idle scheduling class, with a direct `nice -n 19` launch where idle scheduling is unavailable. Priority changes apply to the launching command itself, so macOS workers need no permission to renice a parent process. Nice scheduling lowers competition for CPU; Linux's idle class uses only CPU time the exclusive suites leave idle. `test-ci-download-verified.py` spends nearly all of its wall time in fixed slow-child and hang delays, so it starts beside the pool, outside its window, and is reaped after the pool drains. Its helper deadlines run on an injected clock, and a fixture step fails only after 120 s without output. Pooled workflow and fixture self-tests that used a short subprocess wall-clock cap share one hang ceiling in `scripts/self_test_hang.py` (`HANG_CEILING_SECONDS`, 120 s, the fixture-step deadline). A step that exits is not failed for wall time. A step still running at the ceiling is killed, and the failure names the step or command. The removed 10 s and 20 s caps failed correct release and PR-state steps when idle scheduling stretched wall time. Timeouts that are the subject of a test, including the exclusive-lane suites and the QA sweep's supervision cases, stay as they are. Every other guard and self-test shares the pool. In full mode the pool and the exclusive lane overlap the workflow test build. Both join before the workspace tests start, the pool first, so no guard shares the host with their elapsed-time and timeout assertions and a pool failure stays the job's exit status. Nothing in the pool depends on that build or takes the Cargo build lock. Selection uses the same `CI_TEST_BASE` override and merge-base fallback as affected tests and includes committed, staged, unstaged, deleted and untracked paths. If Git cannot resolve the base or diff, every self-test runs. The QA sweep inventory check still runs for crates-only changes because it reads live CLI/MCP/web surfaces. Full `make ci` always runs all self-tests. The MSRV compile check is timed on its own, separate from those scripts: make prints `ci-fast: MSRV check <seconds> s` for `cargo +<MSRV> check --workspace --locked`, and each guardrail keeps its own `ci-guardrails: <name> <seconds> s` line.
- `make ci-fast` passing says nothing about the Rust tests. Focused test filters help you investigate but do not replace `make ci-test-affected`: an existing test can encode behavior your change breaks even when your new tests pass.
- Run Cargo-based gates (`ci-fast`, `ci-test-affected`, `goldens`, `ci-lint`, `cargo test` / `nextest`) one at a time when they share a target directory, or give each its own `CARGO_TARGET_DIR`. Concurrent gates in one target can rebuild `target/debug/orbit` under running generation-bound fixtures and fail them.
- Guardrails need Python 3.11+ for `tomllib`; stock macOS `/usr/bin/python3` is 3.9. [require-python.sh](../scripts/require-python.sh) runs first in `make ci-fast`, `make ci-lint` and `ci-guardrails.sh` and stops with the version and interpreter it found. The selected fixture self-tests also require PyYAML (`import yaml`), including the verified-download fixture. After resolving self-test selection, `ci-guardrails.sh` checks that import with the same `python3` before Cargo or any repository guard runs. Full CI always requires it; fast mode requires it whenever self-tests are selected, including unresolved selection. Missing prerequisites fail; nothing installs automatically. Put a supported `python3` first on `PATH`, then set up its environment as below. The standalone local workflow YAML linter may still warn and skip without PyYAML; that optional boundary never substitutes for the mandatory selected fixtures.
- Linux CI installs nextest, cargo-deny, uv and ripgrep through [ci-download-verified.sh](../scripts/ci-download-verified.sh): `scripts/ci-download-verified.sh HTTPS_URL SHA256 NEW_FILE`. Each invocation allows at most three GET attempts under one 60-second monotonic budget, including backoff and digest verification. Only curl DNS, connection and timeout exits (6, 7, 28), and HTTP 429/500/502/503/504 with curl exit 22, can retry. TLS, authentication, partial transfers, unknown outcomes and digest mismatches fail closed. HTTPS-only redirects and normal certificate checks remain enabled; curl configuration files and internal retries are disabled. Each attempt uses fresh bytes, and only a matching digest publishes the new destination; existing destinations are refused. Installer traps remove scratch archives on success or failure, and archive/version checks never retry. Attempt diagnostics and total elapsed time remain in the Check job log: all setup attempts, backoff, failures and post actions count toward the complete job's 600-second budget. Offline public helper and workflow installer fixtures run in the existing guardrails via [test-ci-download-verified.py](../scripts/test-ci-download-verified.py).
  The short-deadline fixtures publish completed receipts by atomic replacement and signal readiness through an inherited pipe. That self-test points `ORBIT_CI_DOWNLOAD_FAKE_CLOCK` at a file holding a decimal second count. The helper sleeps by advancing that clock exactly, and a stalled transport waits until the helper arms the attempt deadline, then crosses it so the same watchdog kills and reaps the child. Budgets stay 0.3s and 1.4s; host scheduling cannot move the outcome. The fixtures assert exact receipt counts, remaining second-attempt budget, unsuccessful exit, destination absence, scratch cleanup and child reaping. Cancellation barriers reproduce interrupted in-place receipts and transport startup without synthetic host load. Backoff in the transient-table, checksum and attempt-limit cases also runs on that virtual clock: those cases record their delays (one second; one then two seconds) instead of sleeping them. The real-TLS boundary case and the remaining download and installer cases leave the variable unset and use `time.monotonic` with the helper's real `subprocess` timeout, so the real-TLS recovery case keeps its one-second backoff as a real sleep. The self-test does not put a short wall-clock cap on those commands: a child that exits, or keeps writing, is not failed for host speed, and a child that produces no progress is killed at a hang ceiling no correct local run approaches, with a diagnostic that names the hung step.
- Skill docs are canonical under `crates/orbit-core/assets/skills/`. Edit them there and run `scripts/sync-plugin-skills.sh` to regenerate `plugin/skills/`; `make ci-fast` fails on a stale mirror.
- [check-doc-links.py](../scripts/check-doc-links.py) checks tracked and non-ignored untracked Markdown, and fails on a missing relative link target, heading anchor or backticked repository path that is not a tracked or non-ignored untracked file present in the worktree. Website links resolve through Starlight routes and code blocks are ignored; its exclusions (historical decisions, RCAs, templates, changelog, fixture prose) are listed in the script. Run `python3 scripts/check-doc-links.py`; its fixtures are in `python3 scripts/test-ci-fast-guards.py`.
- Repository source guards reuse work only within one invocation. The orphan-module guard enumerates the same production and sibling-test trees and masks each source once for lexical module declarations. Its scan includes hidden, ignored and symlinked source files, matching the physical tree checked for orphans so ignore rules cannot hide module owners. Symlinked `lib.rs` and `main.rs` entrypoints are discovered directly, including roots that share a physical source but own different logical directories. It resolves ordinary declarations and direct `#[path = "..."]` literals, accepting whitespace and Rust comments between attribute tokens, including between `#` and `[` (for example, `# /* comment */ [path = "other.rs"]`), as well as trailing line and nested block comments, ordinary Rust escapes (quote, backslash, ASCII `\xNN`, Unicode `\u{...}`, control characters and line continuations), and raw `r"..."` and hash-delimited `r#"..."#` strings (with matching hash counts), using [Rust's enclosing module directory rules](https://doc.rust-lang.org/reference/items/modules.html#the-path-attribute): explicit paths outside inline modules start at the source directory; ordinary declarations and inline modules add their directory components and, for ordinarily loaded file modules, the source stem. A file loaded through an explicit path instead owns its containing directory for ordinary children and inline descendants; the guard follows declarations to carry that ownership into each child. Symlinked source files retain their logical owner directory and filename for child resolution, while canonical file identity is used for registration and source caching. Indexing distinguishes each source-directory/child-directory context for a physical owner; canonical source directories make growing directory-symlink spellings converge on already indexed contexts, without rejecting finite nested reuse of the same physical file. Directory discovery does not follow symlinks; declared file destinations may resolve through them. An inline module's own path attribute replaces its default directory. An external module's explicit path replaces its ordinary destination: `#[path="other.rs"] mod lost;` registers only `other.rs`, leaving a sibling `lost.rs` orphaned. The scan masks comments and literals to find declarations, then masks only comments within each attribute to preserve its actual path literal. It tracks nested braces and compares actual destination files, including `.` and `..` components; an unrelated file with the same basename cannot register a production module or sibling test. It accepts declarations regardless of cfg, rather than evaluating build configurations, and does not expand macros. The disposable command fixtures in `python3 scripts/test-ci-fast-guards.py OrphanModuleTests` compile miniature crates with `rustc --test` and enumerate their tests to verify ordinary, raw-string, redirected, inline and nested path registration while keeping same-name and same-basename orphan files rejected. Escaped/spaced attribute (including separated hash/bracket openings), trailing-comment attribute, symlink-owner and symlinked crate-root regressions also execute the registered tests, retain genuine orphan controls, and check that a growing symlink cycle terminates. The CLI import guard reads each source once with the same comment/string masking and per-crate diagnostics. The unused-dependency guard collects reference names once per file and combines them only within the original normal, dev or build scope. Each later invocation reads the current files again; no persisted source cache, reduced guard selection or wider concurrency is involved.
- The guard fixtures retain full runner success/failure checks for shell-wait routing, then exercise the remaining syntax and exception rows directly through the checker executable. Action-pin fixtures record interleaved curl attempts and exact sleep requests instead of waiting for simulated network retries; the production guard still performs its normal backoff.
- [check-unused-dependencies.py](../scripts/check-unused-dependencies.py) fails on a manifest dependency its sources never use, a normal dependency only test targets use, or a `[workspace.dependencies]` entry no member declares. Keep a feature-only dependency in its `FEATURE_ONLY` allowlist with a reason. Self-test: `python3 scripts/test-check-unused-dependencies.py`.
- [check-dashboard-browser-fixtures.py](../scripts/check-dashboard-browser-fixtures.py) runs in the static guard list beside `check-windows-portability.py` (`make ci-guards`) and fails when a Playwright `*_browser.mjs` fixture under `crates/orbit-web/` queries a literal element id, CSS class, `data-*` attribute or `/api/...` path with no occurrence in `crates/orbit-web/assets/dashboard/` or the web API routes; each failure names the fixture, line and missing hook. An id or class the assets build from a template satisfies only a hook of that kind the whole template can produce. An element-id probe is exempt only in a supported explicit absence assertion, such as `assert.equal(document.getElementById('removed'), null)` or `assert.ok(document.getElementById('removed') === null)`. A comparison alone, including an `if (... === null) throw ...` required-element guard, stays a query. For absence booleans returned by browser fixtures, use `await page.locator('#removed').count() === 0`. It catches removed or renamed hooks, not computed geometry such as a hard-coded track width. Self-test: `python3 scripts/test-dashboard-browser-fixtures.py`.
- [check-linux-gated-test-items.py](../scripts/check-linux-gated-test-items.py) runs in the static guard list beside `check-windows-portability.py` (`make ci-guards`) and fails when an import, test helper `fn`/`const`/`static` or fixture `mod` in a Rust test file or `#[cfg(test)]` module is left ungated while every use site is compiled out on macOS (typically `#[cfg(target_os = "linux")]`), which turns native macOS Clippy red under `-D warnings` with `unused import` or `never used`. It reuses the Windows guard's cfg evaluation and module edges for a macOS target, scans `crates/` and `desktop/`, and names the file, line and item of each failure. It is lexical: it matches use sites by name within the item's block and the modules declared or `include!`d in it, skips `pub` items, UpperCamel imports (a trait import is also used implicitly through method calls) and items under `allow(dead_code)`/`allow(unused_imports)` (a `cfg_attr` allowance counts only where its condition may hold on macOS), and does not reach dead code behind macros. Self-test: `python3 scripts/test-linux-gated-test-items.py`.
- [check-windows-portability.py](../scripts/check-windows-portability.py) runs in `make ci-fast` and rejects recognizable Unix std APIs reachable on Windows, including test helpers. It checks Rust files under `crates/`, follows external and inline module declarations (including `#[path]`), and accepts enclosing item, module or file gates that exclude Windows. It evaluates `unix`, `windows`, `target_os`, `target_family`, `all`/`any`/`not`, and conditional `cfg_attr`; unknown features and test flags remain potentially enabled. `cfg!(unix)` does not remove code from compilation. The static approach runs on every worker without installing a Windows target or linker, and needs no path allowlist. It scans Unix std paths/grouped imports, distinctive extension calls such as `from_mode`, `pre_exec` and `ino`, qualified permission/metadata trait calls and `permissions().mode()`. It ignores comments and literals. It does not expand macros or infer receiver types, so ambiguous application methods such as `mode`, `set_mode` and `blocks` are checked only in recognizable std forms; hosted Windows compilation remains the complete check. Self-test: `python3 scripts/test-windows-portability.py`.

For contributor setup, create a virtual environment with your supported Python,
activate it before validation, and install the mandatory module there:

```bash
python3 -m venv .venv
source .venv/bin/activate
python3 -m pip install PyYAML
python3 -c 'import sys, tomllib, yaml; print(sys.executable, yaml.__version__)'
```

For an existing writable Python environment, use that exact interpreter's
`-m pip install PyYAML` and confirm its import as above. A package installed for
another Python version or an inactive environment does not satisfy the check.
For an externally managed interpreter (such as Homebrew Python), use the virtual
environment; do not override its package-management protections. The preflight
diagnostic names the interpreter selected from `PATH` and gives the setup steps.

[check-codeql-production-paths.py](../scripts/check-codeql-production-paths.py) in `make ci-fast` rejects Rust source
names matched by `.github/codeql/codeql-config.yml`'s `paths-ignore`, reporting
the file and pattern. It checks tracked and non-ignored untracked sources,
excluding `tests/`, `examples/` and `benches/`. Other matches need a direct
`#[cfg(test)]` or `#[cfg(feature = "test-support")]` on every declaring module;
the latter is the existing feature for helpers enabled from dev-dependencies.
This naming guard fails closed for indirect or composite cfg gates and
unrecognized module layouts: use a filename that CodeQL includes in those
cases. Its behavioral fixtures run in [test-codeql-production-paths.py](../scripts/test-codeql-production-paths.py).

### Affected-test selection

Delivery remote-source and provider-attribution fixtures freeze monotonic source-budget time through
`orbit_core::test_support::SOURCE_ELAPSED_ENV`, inherited by their isolated
children and the built CLI. Zero elapsed milliseconds removes the assumption
that real Git/provider commands finish within the two-second command budget,
thirty-second source deadline, or twenty-second lookup window on a busy host.
The runner still waits for command completion and enforces output limits;
the isolated child-test guard bounds hangs and reaps the process group.
The existing scoped `with_delivery_source_clock` hook uses the same budget
clock for in-process boundary fixtures.
An expired injected clock exercises deadline refusal without sleeping or host
load. The same clock covers instruction-tree preparation. This hook exists
only with the dev-dependency `test-support` feature; production builds keep
their monotonic wall-clock budgets and recognize no override.

The engine's integration scenario families retain named assertion bodies under
bounded isolated child tests. Pristine Git fixture templates save repeated
initialization; each row receives independent repository files and objects.
The isolated Git config sets `maintenance.autoDetach` and `gc.autoDetach` to
false: automatic maintenance still runs with its normal locks and durability,
but completes before preparation returns and the seed is copied. This prevents
Git 2.55's detached temporary-lock owner from changing a supposedly pristine
seed during the copy. Missing source files remain fixture failures with source
and destination observations.
Their [scenario mapping](../crates/orbit-engine/tests/engine/SCENARIOS.md) lists
retired nextest names and their surviving rows. PR landing fixtures use the
engine's dev-only `with_vcs_retry_wait` hook for fixed delays: poll counts, logical
wait budgets and remote mutations still run, while subprocess deadlines and
cleanup remain real. Completion's waited-seconds elapsed term reads the same
prerequisite clock as the completion deadline. A fixture that advances that
clock by each logical wait does not pick up wall-clock seconds between samples.
Build-slot fixtures use the existing CLI supervision clock
only after a real wrapper heartbeat proves the queue interval. This removes
host-speed assumptions without extending timeout or cleanup budgets.

A debug `orbit` or dashboard child does not see those thread-local hooks.
Three deadlines in that child read an environment variable only when the
binary is built with debug assertions, and a release build ignores it:
`ORBIT_TEST_HOST_FORWARD_ATTACH_TIMEOUT_MS` (host-forward attach budget),
`ORBIT_TEST_MCP_LIFECYCLE_INTERVAL_MS` (stdio lifecycle tick), and
`ORBIT_TEST_PIPELINE_WAIT_VIRTUAL=1` (a pipeline wait adds each poll to its
elapsed time instead of sleeping, and does not wake on a reaped worker).
The quiet-deadline child fixture injects its bound through
`run_child_test_within`. The real child and the real reap stay in place.

`make ci-test-affected` (`scripts/ci-test-affected.py`) reads Cargo metadata without compiling. It selects every crate with changed paths plus all transitive reverse workspace dependents (dev, build, renamed, optional and target-specific edges), then runs their library, binary and integration test targets with nextest (Cargo if nextest is missing) and Cargo doctests, under the shared [build-budget admission](runbooks/build-budget.md). A change under `crates/orbit-core/` selects `orbit-core`, `orbit-cmd`, `orbit-web` and `orbit-cli`.

- **Diff:** committed, staged, unstaged and non-ignored untracked paths; a move selects both crates.
- **Base:** the merge base of `HEAD` with `origin/agent-main` (local `agent-main` if the remote ref is absent). Override with `CI_TEST_BASE=<rev>`; a reviewer with a pinned delivery base must run `CI_TEST_BASE=<base.commit> make ci-test-affected`. An unavailable base fails the gate.
- **Everything or nothing:** a change to `Cargo.toml`, `Cargo.lock`, `.cargo/`, `.config/` or the toolchain files, or a removed crate, selects every crate. A docs-only diff selects nothing and passes without compiling.
- **Cross-crate file reads:** Cargo cannot see a test reading files outside its crate. Declare the path prefix and reading crates in `FILE_READERS` in `scripts/ci-test-affected.py`; nothing detects them for you, and a declared crate missing from metadata fails the gate.
- **macOS workflow names:** when the diff adds, deletes, renames or edits a path under a `tests/` directory of a crate (`crates/**/tests/**`, `crates/**/src/**/tests/**`) or edits `.github/workflows/ci-macos.yml`, the gate first runs `scripts/check-ci-macos.sh` under build admission. It lists the tests the macOS workflow names and runs none, and exits before any test runs if one of its paths or filters no longer matches. The failure names the stale entry in `.github/workflows/ci-macos.yml`, which is the file to fix. A diff touching neither skips it. A replayed `ORBIT_VALIDATION_SELECTION` carries no diff, so it skips the check too: the candidate run that named the selection already ran it.
- **Inspect:** `python3 scripts/ci-test-affected.py --list [--base <commit>]`.
- **Hosted CI:** `--pull-request-scope FILE` and `--test-build-packages CRATES` are the pull-request CI's entry points ([PR test selection](#pr-test-selection)). They read Cargo metadata (and `cargo tree`) but no diff and run no test.
- **Test environment:** nextest runs with `--success-output immediate`. The gate archives its complete merged stdout/stderr in a unique `ci-test-affected-nextest-*.log` under `.orbit/tmp/` and streams progress, summaries, diagnostics and failing-test blocks. Passing-test blocks stream only host-evidence notices (`DEFERRED:`, `SKIP:`, `skipping:` or `skipping `, and sandbox-unavailable markers), so a full selection stays within the executor's default capture limit while the host judge still sees every refusal. The no-nextest `cargo test` fallback is unchanged and does not expose passing-test captured output. A temporary directory inside the checkout is added to `GIT_CEILING_DIRECTORIES`. `ORBIT_WORKER_CONTEXT_REQUIRED` is dropped ([why](#test-process-environment)); the rest of the run envelope passes through.
- **Observed streams:** Cargo fallback and doctest commands drain stdout and stderr independently and capture whole lines. A relay I/O failure stops the command's process group before waiting for the readers, then propagates the original error. These commands own a separate session; the gate forwards graceful SIGTERM shutdown through cleanup, with SIGKILL escalation after a short grace to stop descendants holding the pipes. Killing the gate directly with SIGKILL cannot run this cleanup.

To check whether a failure is pre-existing, replay the same command on the unmodified base: extract it under `.orbit/tmp/` (for example `git archive <base> | tar -x -C .orbit/tmp/base`), refresh its timestamps, give it its own `CARGO_TARGET_DIR` (any path works; the `orbit update` fixtures copy the tested binary to their own `target/debug/orbit`), and set `GIT_CEILING_DIRECTORIES` to `.orbit/tmp` so its fixtures cannot find the surrounding checkout. Replay only the failing tests, not the whole selection: hand the candidate's summary `selection` back with `failing_tests` added (below), or run nextest with `-E 'binary_id(=<binary-id>) & test(=<test>)'` for each failure it printed. Replay soon after the candidate run, since fixture deadlines depend on host load.

### Required validation commands

This repository currently has no blanket local required-validation commands. The required PR checks gate merges on `agent-main`: `Check / Clippy / Test` and `Test shard 1/3`, `Test shard 2/3`, and `Test shard 3/3`. Use focused local checks for the changed behavior as described in [AGENTS.md](../AGENTS.md#gates); the PR checks report their results on GitHub.

```toml
[workflow]
required_validation_commands = []
unlocked_path_validation_commands = ["make ci-fast"]

[workflow.generated_paths]
"docs/INDEX.md" = "make docs-index"

[workflow.validation_env]
build_output_env = ["CARGO_TARGET_DIR"]
```

A repair that changed only Markdown since the run's last full pass reruns only `make ci-fast` ([revalidating an unlocked-path repair](CONFIG.md#revalidating-an-unlocked-path-repair)), and a conflict on `docs/INDEX.md` is resolved by regenerating it ([generated paths](CONFIG.md#generated-paths-are-regenerated-not-merged)).

The reviewable [workspace configuration example](../config/orbit-workspace.toml) shows
the current repository policy. Merge applicable settings into the owner configuration
during operator rollout. Cargo output selection is this repository's policy; the generic
runtime has no build-tool default ([build output contract](CONFIG.md#workspace-build-output)).

When an owner configures required validation commands, candidate validation runs them in order on the final candidate after base synchronization, and before-PR review requires a passing record of each. Tasks tagged `no-diff-expected` do not run this list. A managed implementer finds the list in its envelope's `task.final_validation_commands` and leaves those commands to that validation ([required validation](CONFIG.md#required-validation)); it still runs focused checks and any gate the list omits.

Managed PR conflict recovery at `sync_base` uses that same captured list:
it runs focused resolution checks and leaves those full commands to the
host after the rebase continuation and any base chase. If the base absorbs
the candidate, `git_rebase` runs the commands on that final clean head and
attaches their logs before no-diff settlement, which bypasses the ordinary
validation step. Standalone recovery, later completion recovery and claims
without captured owner gates keep their advisory checks. A later rewrite
still requires validation of the new head; earlier evidence cannot cover it.

Roll a list change out from the operator's shell, never from an agent: deploy the Orbit build that reads it, edit the owner's `.orbit/config.toml` (copy it to each follower replica too, which `orbit doctor`'s `pull-gates` row compares), then start a new drain. A claim already admitted keeps the list its admission captured, which is never rewritten; a fresh admission picks up the new list.

### Validation failure diagnostics

Owner validation logs record advisory `timing` separately from the pass/fail
evidence. Each attempt retains UTC start/end, environment/setup and supervised
process milliseconds, its exit/timeout result, and the build wrapper's bounded
monotonic admission snapshots. `queued_wall_ms` is the union of wait intervals;
summed waits can overlap. No timeout credit is granted by this telemetry.
Network reruns retain every attempt and measured backoff rather than replacing
their cost with the last attempt. Gate-reported compile/test/package durations
are retained in `phase_timings`: compilation-unit and testcase sums can overlap
and must not be presented as wall time. Missing measurements remain null.
Baseline logs retain original execution timing even when reused from cache;
`cached: true` means that execution cost was not incurred again.

Candidate logs also record `baseline_comparison_ms` and `candidate_rerun_ms` for
the current invocation (null when absent). The comparison includes cache-lock
waiting, checkout preparation and the baseline command; the cached command's
historical elapsed time must not be added to it.

A passing pinned-base execution means the candidate failure was not reproduced
in that recorded execution; it does not establish that source changes caused
the failure. Diagnostics distinguish an exact replay of the candidate's failing
tests from a command run with its recorded selection. Exact replay covers only
those failing tests, without establishing full-suite or load equivalence.
When neither run declares a selection, matching command text does not establish
that the same guards ran. Noncomparable or inconclusive base results leave
source causation unconfirmed. These limits do not change the candidate's
validation failure, comparison rules, known-red admission or replay policy;
the candidate and base logs retain the original evidence.

Canonical owner log paths still name the latest validation result. Before a
canonical log is replaced, its previous bytes are saved as an immutable copy
under `validation/<run>/attempts/<sha256>.json`, preserving earlier candidate,
baseline and failing-test replay evidence across recovery. First-time logs
need only their canonical record. These diagnostic
copies do not replace the canonical artifacts used to admit delivery. Use
durable activity start/finish events to separate provider recovery and each
validation invocation from an end-to-end step span; subtracting reported test
time alone cannot establish engine overhead.

Required commands may report diagnostic files through the generic
`ORBIT_COMMAND_EVIDENCE_DIR` and `ORBIT_COMMAND_EVIDENCE_INVOCATION` channel.
The executor creates a fresh directory outside the checkout for each attempt,
anchors its descriptor before execution, and reads only flat, no-follow regular
files. A command writes `manifest.json` with `schema_version: 1`, its supplied
`invocation`, and up to eight `files`, each containing `name`, a relative
`source` label, `bytes`, `sha256`, and `complete`. The source label is
command-reported provenance, never a path the executor opens. Unknown fields,
foreign invocations, traversal, symlinks, nonregular files, mismatched sizes or
digests, and invalid UTF-8 are refused. The manifest read is capped at 16 KiB;
all source and retained bytes together are capped at the existing 1 MiB inline
artifact ceiling **per attempt**. These limits do not extend command deadlines.

The affected-test script reports its merged nextest log on a failed runner
exit, copying at most 1 MiB and marking a larger source's prefix incomplete.
Standalone execution still leaves the log under `.orbit/tmp/`.
Managed validation captures reported bytes before the host temporary directory
ends, applies secret redaction to the entire text before splitting it, and
attaches UTF-8 chunks of at most 16 KiB under
`validation/<run>/evidence/<digest>.json`. Each chunk has its byte offset,
length and content digest; a task-bound `<digest>.manifest.json` records the
source and retained digests/lengths, redaction, completeness, chunk artifact
digests/paths, and exact task/run/command/log/role/head/attempt/invocation
provenance and the source checkout path. The command text is redacted; its
unredacted digest retains exact command identity. Candidate, cached base, network
attempts and failing-test replay logs retain their own qualifications. Cached
base bytes still describe the original invocation, even when attached to a later run's task. Chunk attachment
precedes its manifest; failed or unknown attachment responses are never retried
and are recorded as `attachment_unknown`, `complete: false`. Disposable scratch
may then disappear without erasing conclusively attached evidence.

`not_reported` and `refused` also mean incomplete evidence. Oversized command
reports are refused rather than silently truncated or mislabeled complete; the workspace
script reports its bounded prefix with `complete: false`. The host withholds the
entire payload of a partial source before in-memory or baseline-cache retention:
a cut can hide the credential length, closing delimiter, multiline value, or
UTF-8 scalar needed for safe redaction. Its verified reported source size/digest
remain unchanged; retained size is zero with the empty-content digest,
`partial_source_withheld: true`, and both completeness flags false. `redacted`
is true when withholding removed bytes. This also withholds benign partial
payloads; bounded command output continues to retain failure diagnosis. Complete
files still undergo whole-text redaction before chunking. Older baseline-cache
results without this boundary protection are not reused. `complete` describes all
reported bytes with redaction applied; `raw_complete` is false if the producer reported a
partial log or redaction changed it. Binary logs are unsupported. A command
interrupted before reporting cannot establish complete raw evidence. Evidence
is diagnostic only: no report, missing chunk or completeness claim can grant
coverage, change exit status, or waive base/replay/host refusals. Claimed
known-red failures attach diagnostics before their existing continuation.
`evidence_capture_ms` and `evidence_retention_ms` report advisory capture and
attachment overhead separately; fixture attachment timing does not establish
owner-store/network cost. Existing trusted owner/run and claim routing govern
all writes; command manifests cannot choose an owner or an artifact path.

Required-command failure messages are limited to 16 KiB and name the command,
exit code, failing tests, base verdict and candidate/base log artifact paths.
Captured output stays in the task's `validation/<run>/<index>.json` and
`<index>.baseline.json` artifacts (claimed runs use their claim namespace).
`orbit run show --json` and Incidents retain these references in the leaf's
message; follow a parent's recorded `root_cause` to that leaf. Read the logs
with `orbit.task.artifact.get` using the owning task and artifact path. Recovery
uses those same artifacts before choosing a repair.

Parent gates echo only the leaf run, task, step and code. Incident codes use
recorded error codes, falling back to the typed terminal run state; command
stdout and diagnostic prose never supply a code. A red-base message retains
its exact persisted hold for admission. If that metadata itself exceeds the
message budget, validation refuses it rather than truncating a command or
selection needed for a safe replay; the artifacts retain the evidence.

### Validation summary and base reruns

When a required command fails, Orbit reruns it on the base to tell a red base from a candidate failure ([decision rules](CONFIG.md#workflowvalidation_env--the-toolchain-required-validation-runs-with)). A command that selects tests from the diff would test nothing on the base, so the runner passes two variables, both implemented by `scripts/ci-test-affected.py`:

- `ORBIT_VALIDATION_SUMMARY`: a file outside the checkout where the script writes `{"schema_version": 1, "selection": {"packages", "target_flags", "doctest_packages"}, "tests_run": N, "executed_tests": [...], "command", "base", "exit_code", "notices", "compile_ms", "test_ms", "package_timings"}`. `compile_ms` and `test_ms` are millisecond totals. `package_timings` is a list of `{package, compile_ms, test_ms}` sorted by package name. Each total equals the sum of the per-package integers for that phase, except under the `cargo test` fallback described below. Compile time is the sum of Cargo unit durations from the `--timings` report of the build the gate already runs (those durations overlap when compilation is parallel, so the total can exceed the build's wall time). Test time is the sum of nextest JUnit testcase durations plus per-package doctest durations (testcase durations overlap when tests run in parallel). Cargo labels doctest output with library target names; the gate resolves those names through Cargo metadata to the selected package identities, including explicit library names and hyphen normalization. A target shared by multiple selected packages cannot be attributed from that label alone, so its doctest duration is omitted. A phase that was not measured is `null` on the total and on every package. The `cargo test` fallback has no per-package test split: libtest's `finished in` time has no package, so those `test_ms` values stay `null`, and the total is that libtest time plus the per-package doctest durations. Under the fallback the total is therefore not the sum of the `test_ms` values. Nothing selected writes `compile_ms` 0, `test_ms` 0 and an empty `package_timings`. The record written before the run starts leaves all three `null`. `tests_run` comes from nextest's JUnit report (enabled with `--tool-config-file`); it is `null` after a build failure or under the `cargo test` fallback, and excludes doctests. `executed_tests` contains sorted, deduplicated nextest `<binary-id> <test>` identities from non-skipped JUnit testcases; a missing or malformed report leaves it `null`. The selection is written before the tests start (with `tests_run: null`), so a run stopped part-way still reports it. The other fields are what an [implementer's deferral](#test-process-environment) is derived from: `command` is `ORBIT_VALIDATION_GATE`, which the make target sets to `make ci-test-affected` (`null` when the script runs directly); `base` is the comparison base (`null` when given a selection); `exit_code` is the run's final status, doctests included (`null` until it finishes); and `notices` lists every line of the nextest stream that leads with `SKIP:`, `DEFERRED:`, `skipping:` or `skipping ` (empty under the `cargo test` fallback).
- `ORBIT_VALIDATION_SELECTION`: the candidate's `selection`. The script tests exactly those packages; a package the checkout lacks fails the run without a summary. A selection that also names `failing_tests` (each a nextest `<binary-id> <test>` or a bare test name, as the failed run printed them) runs only those tests: it builds only the packages their binary ids name, filters with a nextest filterset (`cargo test -- --exact <names>` in the fallback), skips doctests and reports the selection it was handed. A malformed list, or a binary id outside the packages, fails the run without a summary.

A base run counts only if it reports the same selection, and a base pass only if it ran at least one test; a replay of `failing_tests` must prove every requested identity ran, whether it passed or failed. Qualified requests match both binary and test; bare requests match the exact test path in any binary. Missing, malformed, empty or partial `executed_tests` evidence refuses comparison. A run without the required selection or execution evidence is not comparable: it never makes a failure `baseline_red`, never refutes a reviewer's red-base claim and never lifts a hold that recorded the selection.

Older narrow cache entries without this proof are declined and replayed under the existing cache lock; compatible full-selection caches remain reusable. Owner and claimed candidate validation first reuse a conclusive comparable full cache, otherwise make one exact base replay for fully identified non-timeout test failures. Compiler, lint, unknown-identity, missing-selection, timeout and host-refused outcomes keep full comparison. The full required candidate command is unchanged.

Required-command artifacts share a 256 KiB source-output budget across stdout and stderr; a short stream remains complete. Oversized streams retain their head and tail plus bounded nextest failure blocks, including captured scenario and assertion output between test statuses. Failure blocks share at most a quarter of each stream's budget (up to 64 KiB, 16 KiB per block and 128 blocks); oversized blocks keep their beginning and end so an early flood cannot crowd out later failures. Explicit byte-omission markers disclose cuts, and a block-count notice discloses when more failures were printed than selected. Output is valid UTF-8. Failure identities and host-coverage classifications still come from the complete supervised streams, independently of the bounded reader evidence.

A command that writes no summary has no selection check, so only `make ci-test-affected` is guarded against a zero-test base pass. An inherited failure requires comparable completed runs, nonempty candidate failure identities, and every candidate identity present in the base failure set. Full comparisons also require equal exit status; exact test replays can use a different status because they run only part of the selection. Missing identities, added or disjoint failures, and timeouts cannot establish reproduction; both logs retain the parsed sets even when captured output is bounded. A run stopped by the executor's output capture limit or by a signal is an infrastructure failure, never a test verdict. When a capture limit closes a pipe and its producer exits with SIGPIPE, the primary diagnostic names the observed capture limit; the supervisor retains both causes and the original child status. SIGPIPE without a capture stop and other termination signals retain their signal diagnostics. Candidate stops report the validation-environment marker without a base replay; stopped base runs are inconclusive, never cached as conclusive results and never establish `baseline_red` or lift a hold. Owner known-red claim settlement retains inconclusive attempted runs separately so retries reuse its refusal; ordinary validation and reviewer base comparisons ignore those records. Candidate validation reports the infrastructure cause when its base comparison was stopped.

When a comparable base passes but the candidate names at most three failed tests, delivery validation makes one isolated candidate replay using the `failing_tests` selection. The affected-test runner executes that selection serially (`--test-threads 1`) with nextest retries disabled; it skips doctests and unrelated tests. Acceptance requires a passing replay with the exact requested selection, at least as many executed tests as named failures, and no host coverage refusals. Missing summaries, unknown failures, timeouts, or larger failure sets do not qualify. A repeated failure keeps the ordinary candidate verdict. A passing replay records `flaky_validation` in the run and task history, preserves the initial failure and passing base/replay outputs, and names the tests with captured rerun evidence in the PR. The owner files each command/test signature once as a friction, including for accepted claimed handoffs.


### Hosted CI

Hosted CI runs the work of the full `make ci` on open PRs, split across jobs as described below. Each PR-triggered workflow starts with a checkout-free `Live PR state` job (`pull-requests: read` only); if the PR merged or closed while queued, the build and analysis jobs are skipped under unchanged check names, which GitHub treats as passing. An API error fails the gate rather than admitting an unknown state; a PR that closes after the gate read `open` can still finish. Push, scheduled and manual runs skip the gate, so agent-main/main CI and CodeQL still cover merged changes, narrowed in `ci.yml` as described next. `make ci-fast` selects `scripts/test-pr-state-workflows.py` when self-test inputs change; full `make ci` always runs it. The script replays each gate against recorded API responses.

PR jobs use two revisions: the primary checkout, build and tests use
`github.event.pull_request.head.sha`, while workflow orchestration helpers use
`github.sha`, the revision supplying the workflow (the PR merge ref). In
`ci.yml`, a separate sparse checkout under `.orbit/tmp/workflow-ci` supplies
`ci-await-jobs.py` and `ci-download-verified.sh`; all their invocations use that
path. The scope job checks out `github.sha` directly. Keep this rule when adding
workflow-coupled `scripts/ci-*.py` or `scripts/ci-*.sh` helpers to any workflow:
check them out before their first call and invoke them from that checkout.
Candidate validation scripts such as `ci-guardrails.sh` stay in the primary
checkout so they validate the PR's code and tests. Workflow/helper interface
changes therefore require neither compatibility with older PR-head helpers nor
rebasing older PRs. `scripts/test-pr-state-workflows.py` guards helper ownership
and executes an old-head/two-revision checkout fixture. The CI-failure sweep
reads the workspace-root checkout as the commit under test and ignores a
checkout whose directory is a strict descendant of another checkout directory
in the same job log, so that nested helper checkout is not a second identity.
Two workspace checkouts that name different commits stay ambiguous.

In `ci.yml`, a `Change scope` job ([ci-change-scope.sh](../scripts/ci-change-scope.sh)) narrows each run before build runners start:

- **`docs`**: every changed path, on a PR or in a push, is Markdown outside `crates/` and `plugin/`. A push additionally requires a successful completed GitHub Actions `Check / Clippy / Test` on its `before` commit. A missing, pending, cancelled, failed or skipped check, or an API error, runs the full gate instead: the previous tip may contain code whose pending run was replaced. Markdown under `crates/` is compiled in as assets, `plugin/` is the generated skill mirror, `orbit-cli`'s tests read `website/src/content/docs/reference/config.md`, and the `scripts/test-*` self-tests execute or digest `RELEASING.md` and `npm/README.md`, so each still takes the full gate. Only `Docs guards` runs (`ci-guardrails.sh --static`: doc links and indexes, the skill mirror and the other file and metadata guards).
- **`affected:<crate>,...`**: a PR whose changed paths all sit inside workspace crates, apart from Markdown outside `crates/` and `plugin/`. Every job runs, but `Test build` compiles and archives only the tests of the changed crates and their reverse dependents, so the shards run only those (see [PR test selection](#pr-test-selection)). Pushes are never narrowed this way.
- **`skip`**: a push whose commit has exactly the tree of the merged PR's head, when that head's latest `Check / Clippy / Test` succeeded in a run that tested every crate. The same bytes already passed the full gate, so nothing reruns. A PR run that tests every crate says so in its `Change scope` job with a `Test selection` notice reading `full`, and the push requires that notice from the same run (check suite) as the passing check. An `affected:` PR run, or a head whose run left no notice, gets a full push run. A merge whose tree differs from what the PR tested also runs in full. With non-strict required checks, most merges differ: the PR was tested against an older base.
- **`full`**: everything else, including any classification error. The job never fails, and an empty mode admits the full run.

Push runs of `ci.yml` share one group per branch without cancelling the running workflow, as `ci-macos.yml` does: one run executes, one waits, and a newer push replaces the waiting run. The surviving tip runs in full unless its entire tree already passed on the PR head, or its changes from a successfully checked `before` commit qualify for docs mode. Replacing a pending code push therefore cannot let a later Markdown-only delta hide its untested code. Skipped jobs keep their check names and report as skipped, which GitHub's required checks treat as passing. Orbit's landing pays an owed check only with an explicit success, so a skipped owed check keeps it waiting; its absent-context repair recognises this exact scope job when it proves that a missing required check will report after a branch update. The macOS and CodeQL workflows are unaffected. `scripts/test-pr-state-workflows.py` runs the classifier against recorded API answers and checks which jobs each mode admits.

The ephemeral Linux runner installs sandbox packages with needrestart's
per-invocation `NEEDRESTART_MODE=l` (list-only) setting through `sudo env`.
This avoids restarting unrelated image services while retaining signed apt
installation, the new Bubblewrap binary's actual namespace probe, and the
guarded packaged AppArmor profile checks. See the
[Ubuntu needrestart manual](https://manpages.ubuntu.com/manpages/noble/man1/needrestart.1.html)
for the environment override; this does not change a persistent host setting.

The test gate in `ci.yml` runs on parallel runners from one build:

- `Test build` runs `ci-guardrails.sh --test-archive PATH`. It runs the folded
  workflow test build, then `cargo nextest archive` of the workspace's lib, bin
  and test targets under the same lint environment, so the archive costs no
  compile. `ci-repack-nextest.py pack SOURCE DESTINATION --parts N` re-encodes
  the same tar stream with zstd level 6, a 512 MiB history window and every
  core for transfer, then splits it into `DESTINATION.1` to `DESTINATION.N`
  (`NEXTEST_ARCHIVE_PARTS` in `ci.yml`).
  This captures repeated linked executable sections beyond the native
  archive's 2 MiB window. It requires the runner's `zstd` CLI; the decoder
  is capped at the same 512 MiB window the encoder may emit. The job uploads
  each part as its own one-day artifact, then runs
  `ci-guardrails.sh --test-partitions PATH`:
  [check-nextest-partitions.py](../scripts/check-nextest-partitions.py) lists
  the archive once unpartitioned and once per `--partition count:k/N`
  (`NEXTEST_SHARDS` in `ci.yml`). It fails unless every test the full list
  selects is in exactly one shard, every shard selects at least one test
  (except for an [affected-crates archive](#pr-test-selection)), and no shard
  selects a test outside the full list. A failure fails `Test build`,
  and so the Check job's wait, while the shards already run.
- `Test shard k/N` jobs, one per shard, start beside the build and set up
  their runner. Each waits through
  [ci-await-jobs.py](../scripts/ci-await-jobs.py) `--step` for the build's
  last `Upload nextest archive part N/N` step rather than the whole job,
  downloads every part at once (one stream per artifact; a single stream ran
  at 3–10 MB/s on some runners), restores a native small-window encoding of
  their concatenation with
  `ci-repack-nextest.py unpack SOURCE DESTINATION --parts N`, and runs
  `cargo nextest run --profile ci --partition count:k/N`
  from it. A build that fails before the upload fails the wait at once.
  Repacking preserves every tar byte; it never extracts members or replaces
  an existing destination. Both zstd stages must succeed before publishing
  the destination, and corrupt input, a missing part, missing zstd or encoder
  failure fails the job. Download artifact digest verification still precedes conversion;
  nextest retains responsibility for safe extraction. The original build-side
  archive remains the input to the partition proof. The transfer's larger
  window cannot be passed directly to nextest's default decoder.
  Extracting into the checkout puts `target/debug/orbit` and the test binaries
  at the paths they were built for. Each shard uploads its nextest JUnit report
  (`target/nextest/ci/junit.xml`) as `nextest-junit-k`, which carries the
  per-test durations. The Linux sandbox/policy enforcement gate runs in shard 1
  from that extraction (`--binaries-metadata` and `--cargo-metadata` under
  `target/nextest/`), without extracting the archive again. The shards are
  separate literal jobs with no matrix, gated on `Live PR state` like the other
  jobs: Orbit's PR-landing recovery certifies a rerun after a failed gate only
  for such jobs. agent-main's ruleset requires the exact names
  `Test shard 1/3` to `Test shard 3/3`, so changing `NEXTEST_SHARDS` or a shard
  name needs the same ruleset change before merge. Requiring only
  `Check / Clippy / Test`, which awaits every shard, would not depend on the
  shard count. `scripts/test-pr-state-workflows.py` runs each shard's nextest
  step and wait and the Check job's wait with stub tools, and fails unless the
  shards select `count:1/N` through `count:N/N` once each, each shard waits for
  the upload step ahead of the partition guard, and the Check job's wait names
  the build and every shard.
- `Check / Clippy / Test` runs `ci-guardrails.sh --checks`: formatting, every
  repository guard and self-test, production Clippy, doctests, documentation
  and the supply-chain gate, with no workspace test build or nextest. It then
  waits for `Test build` and every shard, and fails as soon as one of them
  completes without success. Its elapsed time therefore covers the whole gate,
  including any queue wait for the other jobs' runners. That is the time the
  10-minute budget and the `ci-budget-watch` auto-task measure, and its
  conclusion is the gate's result. The wait prints one
  `ci-await: <job> succeeded started=... completed=... elapsed_s=...` line per
  job. A shard's wait prints `ci-await: Test build step 'Upload nextest
  archive' succeeded started=...` instead.
  `--timeout-seconds` (default 2400) is one absolute budget for the whole
  wait, for the Check job and each shard's upload wait alike: every `gh api`
  listing runs under the remaining budget, the poll sleep is clipped to it, and a
  listing still running at expiry is killed and reaped. A blocked listing
  therefore ends the wait at the timeout plus at most 5 s of cleanup, with a
  `timed out` diagnostic naming the unavailable listing or the jobs still
  pending. A listing that arrives after expiry is never accepted as success,
  even when every awaited job succeeded. Failed, skipped or cancelled jobs and
  steps still fail the wait at once, and transient API errors are retried within
  the remaining budget.

Locally, `make ci` keeps the single-process full mode: the folded build, then
nextest from its binary listing.

`ci-guardrails.sh` repeats its phase breakdown in the final log tail, including
on failure: format, workflow test listing/build (which folds in the all-targets
Clippy lint), the exclusive self-test lane, workspace test listing, workspace
tests, production Clippy, doctests, documentation, supply-chain checks and
repository guards. `--test-archive` mode records only
`workflow-test-list-build` and `workspace-test-archive`, `--test-partitions`
mode only `workspace-test-partitions`, and `--checks` mode records every phase
except the workflow test build and the workspace test phases.
`guard-pool` times the background pool from its start to its join. In fast mode
that is the pool's own wall time. In full mode the join follows the workflow
test build, so `guard-pool` includes the overlapped build.
`exclusive-self-tests` times the exclusive lane the same way, from its start
to its join.
`workspace-test-listing` lists the test binaries of the folded build, and
`workspace-tests` runs nextest from that listing without invoking Cargo.
Meanwhile production Clippy, doctests and then documentation run in the
background, with each command launched in Linux's idle scheduling class
(or directly under `nice -n 19` where idle scheduling is unavailable).
The idle class uses only CPU time the tests leave idle; nice scheduling
lowers competition for CPU without changing a parent process's priority.
Production Clippy checks lib and bin targets and writes no test binary nextest executes. It runs as `cargo check`
under the environment `cargo clippy` sets (`RUSTC_WORKSPACE_WRAPPER=clippy-driver`
and `CLIPPY_ARGS` denying warnings and `clippy::disallowed_methods`), and
documentation runs under the same environment. Cargo fingerprints it, so
`cargo doc` reuses that check of every workspace library and only runs rustdoc. Their records time each
command from its own start and overlap `workspace-tests`; the chain joins
before the supply-chain gate, and a failure in it still reports the later
phases.

The workflow sets `CARGO_PROFILE_DEV_DEBUG` and `CARGO_PROFILE_TEST_DEBUG` to
`0`, so the job's dev and test builds, dependencies included, carry no
debuginfo. Nothing in those jobs reads it: panic messages keep their source
locations, and no job sets `RUST_BACKTRACE`. Coverage runs in its own workflow
with line tables. A local build keeps the `[profile.dev]` line tables from
`Cargo.toml`.
`repository-guards` runs from the start of the pool and the exclusive lane to
the later of their joins; in full mode that includes the overlapped build.
These are wall seconds at Bash's one-second resolution with
the original command exit status. They let bounded hosted excerpts retain build/check costs
separately from nextest testcase sums and the full GitHub job elapsed time.

The Rust build caches in `ci.yml` are the `Swatinem/rust-cache` dependency
archives, one each for `Test build`, the Check job and the MSRV job. They hold
dependency artifacts only; every run compiles the workspace members from its
checkout. Each step sets `save-if: ${{ github.event_name == 'push' }}`, so a
pull request restores the base branch's entries and never writes one: only a
push to `main` or `agent-main` saves. A missed key or a failed restore leaves a
cold or partial `target/`, and Cargo rebuilds what is missing. Hosted CI keeps
no compiler-object cache: `Test build` runs `rustc` directly
(`ORBIT_COMPILER_CACHE=0`), and the Check job installs sccache only for the
compiler-cache namespace regression, with a runner-local cache that nothing
restores or saves. sccache cannot cache the workspace members, which Cargo
compiles through `clippy-driver`, and rust-cache already supplies every
dependency. The
[compiler-cache runbook](runbooks/compiler-cache.md#ordinary-linux-ci) records
the measurements, including why caching workspace members would not pay.
`scripts/test-pr-state-workflows.py` fails if any cache step in `ci.yml` can
save on a non-push event.

#### PR test selection

On a pull request that is not docs-only, the `Change scope` job runs `scripts/ci-test-affected.py --pull-request-scope`, the same graph walk as `make ci-test-affected`, over the PR's changed paths and the merge ref's Cargo metadata (read with the runner's stable toolchain). It selects the changed crates, their transitive reverse workspace dependents, and the crates `FILE_READERS` declares as readers of a changed path, and outputs `affected:<crate>,...`. It runs every test instead (`full`) when any of these holds:

- a path outside `crates/`: the root `Cargo.toml` or `Cargo.lock`, `rust-toolchain.toml`, `.cargo/`, `.github/`, `scripts/`, `plugin/` or any other repository file, since a test may read it at run time. Markdown outside `crates/` and `plugin/` is the exception unless `FILE_READERS` names a reader for it.
- a member's `Cargo.toml`, which can change the features of every crate in the workspace build;
- a path under `crates/` that no current member owns, such as a removed crate;
- every workspace crate is selected anyway (for example, a change to `orbit-types` or `orbit-common`);
- the PR file listing is incomplete, or the selection fails for any reason, including unreadable Cargo metadata.

Cargo metadata cannot see a test reading another crate's files. The ones that matter are those whose reader does not already depend on the owner, and they are declared in `FILE_READERS`. `orbit-engine`'s tests read `orbit-core`'s job and activity assets, and `orbit-mcp` compiles in `orbit-web`'s vendored dashboard scripts. Every other cross-crate read found (`orbit-cli` reading `orbit-core` and `orbit-tools` fixtures, `orbit-web` compiling an `orbit-store` fixture, `orbit-core` running an `orbit-engine` fixture) is by a dependent, so the reverse-dependency walk covers it. Reads of repository files outside `crates/` (workflows, `scripts/`, `server.json`, `plugin/hooks/`) are covered by the outside-`crates/` rule.

`Test build` passes the mode to `ci-guardrails.sh --test-archive` as `CI_TEST_SCOPE`. For an `affected:` scope, `ci-test-affected.py --test-build-packages` checks the crates against the PR head's metadata and adds the unselected members that keep every dependency's resolved features equal to the workspace build's (`cargo tree`). Resolver 2 unifies features over the selected packages only, so a narrower set would rebuild dependencies the CI caches hold with the workspace's features. `orbit-cli` depends on every other crate, so it is in every selection, and `orbit-web`'s dev-dependency features on `tokio` and `rusqlite` usually add `orbit-web`. Both the folded build (`check-ci-macos.sh --workspace-build --packages`, which lists only the macOS workflow's filters for crates in the build) and `cargo nextest archive` then take `-p` for that set. A crate the head lacks, or a feature gap no member closes, builds the whole workspace. The archive's package list is written to `PATH.packages`, and the partition guard then runs with `--allow-empty`: an archive with few tests can leave a count partition empty, but every test must still be in exactly one shard.

The shards keep their names and run with `--no-tests pass`, so a shard whose partition is empty runs nothing and passes. The full archive cannot have an empty shard, because its partition guard fails `Test build`. Shard 1's Linux sandbox/policy enforcement gate runs on every full run and on every `affected:` run that includes `orbit-exec`; without `orbit-exec`, neither the sandbox code nor its dependencies changed, and the gate is skipped. Production Clippy, doctests, documentation, the guardrails and the MSRV check stay workspace-wide, and the `Check / Clippy / Test` wait is unchanged: every test job still runs, and a failed or skipped one fails it.

A PR run can still miss a failure only the full suite shows, such as a test reading another crate's file through a path neither a dependency edge nor `FILE_READERS` records. The `agent-main` push run after the merge is the check of the merged tree and always tests every crate.

`scripts/test-pr-state-workflows.py` replays the selection against a recorded crate graph (`scripts/fixtures/ci-crate-graph.json`): a leaf crate, a mid-graph and a core crate, workspace-level and toolchain files, a member manifest, non-crate paths, cross-crate fixture paths, a removed crate and a selection error. It also checks the push `skip` evidence and the shards' empty-partition and sandbox-gate conditions.

## Filesystem writes

The production Clippy pass in `make ci-lint` disallows `std::fs::create_dir_all`, `std::fs::create_dir` and `std::fs::write`, including aliases. Use the `orbit_common::fs::io` helpers:

| Need | Helper |
|---|---|
| Orbit-owned directory | `create_private_dir_all`, or `create_private_dir` for exclusive creation. Owner-only on Unix regardless of umask; existing modes are left alone. |
| Replace a durable file | `atomic_write_text` / `atomic_write_bytes`: stage a private sibling, sync, rename, sync the parent. Permissions are preserved. |
| Validate between staging and commit | `StagedTextFile`; dropping an uncommitted stage removes it. |
| New private file, exclusively | `write_new_private_text`: stage and sync a private sibling, then hard-link it into place, failing if the path exists. The path never holds partial content. |
| Deliberate new-file or disposable write (not atomic) | `write_text_with_parent` |

State and task-artifact writers must use the private helpers; `orbit doctor --fix-state-directory-permissions` repairs legacy writable state. An exception is scoped to the one intentional call and names its reason in `#[allow(clippy::disallowed_methods, reason = "...")]`. Current exceptions: external source, installation, service-manager and Cargo output directories; in-place edits of dotfile-linked user TOML configs; artifact exports to user-selected paths (including symlinks and devices); scaffold sources with their `--force` behavior. None promises crash-safe replacement. Test fixtures may write directly.

## Child processes

Git failure diagnostics retain the child's exit code even when stderr is empty.
When a supervised child exits without a code, the executor appends its actual
termination status (including the signal number and name on Unix) to captured
stderr. Timeout, cancellation and output-capture limits remain separate causes;
an absent exit code alone does not identify a signal.

Production code never waits on a child without a deadline. Spawn through `orbit_common::process::run_bounded` or `run_bounded_capped`, which put the child in its own process group, kill the group at the deadline and cap captured output. `run_bounded_capped_typed` returns a failed spawn as its `io::Error` for callers that branch on the kind (the `ETXTBSY` retry in `orbit update`, the sandbox probes). `run_bounded_capped_complete_typed` also requires stdout and stderr to reach EOF within the deadline. Update version and admission-contract probes use this mode: a parent that exits while a descendant retains either pipe still times out, and the runner tears down the group and closes both pipes. The four bounded runners delegate to `orbit_common::process::supervisor` with null stdin, keep-prefix sinks, a 200 ms termination grace, and a 500 ms post-exit drain capped by the deadline. Signal interception and the stopped-descendant watch are disabled. Unix capture runs on two stop-aware pipe workers, both joined within the drain budget, so no capture worker survives the probe. Output-worker setup, readiness and read failures return an execution error after group cleanup and worker joins, including in EOF completion mode; prefix truncation and a bounded post-exit drain remain intentional outcomes. The other modes keep their existing behavior of tearing down descendants and accepting the direct child's status as soon as it exits.

The production Clippy pass disallows `std::process::Command::output` and `Command::status`; an exception needs `#[allow(clippy::disallowed_methods, reason = "...")]`. The current exceptions have no natural deadline: the SSH stdio relay in `orbit-mcp` and the interactive or operator-watched `sudo` steps of `orbit init` on Linux. Fixtures may use both.

### Recorded owner and provider cancellation

Run cancellation captures identity-bound process handles before identity-bearing liveness and token verification, so a replacement during verification cannot be adopted even if its finite-resolution start time matches. It retains those handles across TERM, both two-second grace intervals, and KILL. Linux uses `pidfd_open`/`pidfd_send_signal` and descriptor liveness; macOS uses libproc unique process IDs and audit-token PID/version signalling. macOS refreshes an exec version only while the unique process ID still matches; a concurrent exec refuses the signal rather than treating the worker as gone. Missing kernel support, unreadable identity, and other Unix platforms fail closed without numeric PID signalling. Non-Unix cancellation remains unsupported.

After the caller verifies the recorded token with the original handle retained, a live group leader authorizes a snapshot of its group members, each bound to a separate identity before the leader is rechecked. Cancellation signals those retained identities individually, never a numeric PGID. Owners in shared groups are stopped individually. Exit or group change during snapshot acquisition refuses group teardown; no later identity probe can suppress that refusal. If the retained leader exits during verification, a live or unreadable numeric group also refuses teardown rather than authorizing a replacement snapshot. Members created after the snapshot, a reused PGID, or unavailable group liveness can leave cancellation incomplete: group probes are evidence for refusal only and never authorize additional signals. Claims and reservations therefore stay held until teardown is confirmed. This favors protecting unrelated processes over forcibly clearing an uncertain group.

## Safe Mutable CLI Fixtures

Fixtures and manual reproductions that mutate Orbit task, run, workspace or registry state run isolated from the process that launches them; authorized operator work keeps normal routing. Hand-run QA from a managed lane follows the recipe in [Manual QA from a managed lane](#manual-qa-from-a-managed-lane). Spawn the CLI as a child with absolute disposable paths, clearing inherited authority before setting `HOME` and `USERPROFILE`:

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

`clear_inherited_authority` removes the managed-run routing, identity and grant variables, including `ORBIT_WORKER_CONTEXT_REQUIRED`. [`tool_list.rs`](../crates/orbit-cli/tests/tool/tool_list.rs) uses this pattern; [`isolated_cli_fixture.rs`](../crates/orbit-cli/tests/support/isolated_cli_fixture.rs) is a shared disposable home, registry and workspace. Orbit core's libtest fixtures use `application::tests::run_isolated_test`, which re-executes one exact test this way and verifies it passed in the child. CLI MCP and output bodies that write databases or change process authority use [`isolated_test::run_in_child`](../crates/orbit-cli/tests/support/isolated_test.rs) before any setup; it verifies exactly one passed test and supplies a private home under the checkout's `.orbit/tmp/`, outside Bubblewrap's `/tmp` overlay. It includes ignored entry points only when their parent has already been selected by the caller.

Runtime integration permutations use `scenario_table!` in the existing runtime binary. Each named row keeps its setup and assertions; the shared [table fixture](../crates/orbit-core/tests/runtime/scenarios.rs) re-executes one isolated table with inherited authority cleared, gives each sequential row a fresh HOME/cwd, restores its environment, and verifies one passed child. Families that install process-wide worker substitutions retain a separate child per row. Keep self-reexecuting entry points as standalone tests, since their exact libtest names are part of their fixture protocol. Table failures report the original row identity, and sandbox deferrals still reach the gate output. Each row and child prints its elapsed time, and a fixture may copy setup from a per-process template under the [fixture template rules](design-patterns/test_strategy.md#quality-and-the-10-minute-budget).

Runtime fixtures create write-heavy temporary roots with `bulk_write_temp_dir()`. Runtime tables use the host build budget's resolved test-thread count, without a binary-wide serialization group. Record both wall and summed time under matched host build-budget settings when measuring the binary. The owner-read retry fixture uses the feature-gated `with_owner_read_retry_wait` hook to exercise the same retries in milliseconds; production keeps its one- and three-second waits. Outcome fixtures may opt into `with_pipeline_poll_wait` while retaining real elapsed time, deadlines and child/process transitions. Immutable proc fixtures use `with_host_load_sampling_wait` only when sampling cannot gain tick data by waiting; raw reads, CPU unknowns and reservation behavior remain real, and native `/proc` keeps the production interval. These callbacks have independent thread-local scopes and restore the previous callback on return or unwind. Changing-proc, polling-cadence and deadline controls keep system waits.

Before adding tasks or starting runs, make the checkout a Git repository (`git_repo::init(&work)`), run `workspace init`, then check routing read-only: `workspace show --format json` must report `registered: true`, and `checkout.repo_root` and `checkout.orbit_dir` must equal the canonicalized fixture paths (the child reports physical paths). This catches a fixture routed to an ambient workspace before it mutates anything.

For committed Git fixtures, use `git_repo::SeededRepo::new(&work).build()` from `tests/support/git_repo.rs`. The builder pins `main` and commits a README by default; use `branch`, `readme`, `empty_commit`, `no_commit` and `identity` for specialized seeds. `host_default_branch` explicitly preserves plain `git init` semantics where any host default is acceptable. Use `git_repo::run` for checked Git commands and trimmed stdout; both initialization and commands scrub inherited repository authority.

For task-list, search and rendering setup, use `tests/support/task_workspace.rs`. Its shared `TestWorkspace` verifies routing and `seed_tasks` writes a batch through the public runtime in one isolated child, including statuses and tags, without starting an `orbit` process for each record. Use the CLI explicitly when creation or tag parsing is itself the behavior under test. Threshold tests should pass a small explicit `--limit`; the default-limit and bare-array JSON case keeps its 60-record batch.

Detached-worker tests use `tests/support/workflow_gate.rs` to hold a step at a socket rendezvous through the existing `local_shell` action. Wait for readiness, assert the CLI's wait or detach behavior, then release the waiter and observe the terminal run. Socket and action deadlines only bound failures; successful execution does not wait for a sleep to expire. Release the waiter after interrupting its owning worker as well.

- **Every fixture checkout gets its own Git repository,** negative controls included. Root discovery walks past a plain directory or empty `.git`, so with `TMPDIR` inside another checkout (including a managed `.orbit/tmp`) the fixture resolves the enclosing Orbit root and `workspace init` refuses, or fails on a managed run's read-only `.orbit`. A tmpfs `TMPDIR` avoids this only when it is outside the checkout; it does not relax the fixture's own-root requirement. Never answer that refusal with `--force`. Ordinary lookup and the plugin's SessionStart hook still accept an `.orbit` at any ancestor, so a control that needs "no workspace above" must check that precondition and report a skip.
- **`export HOME=/tmp/...` is not isolation.** A managed child can inherit `ORBIT_MANAGED_RUN_CONTEXT` and `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE`, which carry durable authority and override home discovery. Never run bare mutable CLI commands in a managed worker ([regression test](../crates/orbit-cli/tests/workspace/ambient_authority_isolation.rs)).
- **Build every fixture Git command, reads included, with `git_repo::command()`** (`crates/orbit-cli/tests/support/git_repo.rs`), which scrubs authority first; set another initial branch with a scrubbed `symbolic-ref` before the first commit. Inherited `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, index or object-directory variables can amend the parent repository or force-push to its origin.
  The shared-helper containment regression (`cargo test -p orbit-cli --test workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy`) re-executes one small fixture body with these locators aimed at a disposable sentinel. It exercises repository setup, commits/refs, status, ignore rules, push and linked worktrees through the shared helper, then checks the decoy HEAD, refs, config, index, worktrees, sentinel bytes and remote refs. Table rows cover all locators together and `GIT_COMMON_DIR` alone; process-wide scrubbing does not replace the command builder. Each CLI suite runs its ordinary boundary fixtures once, without another per-binary containment wrapper.
- **In core, engine, store, exec and web tests, use the crate's `tests/support/git.rs::command()` builder** for every fixture Git command, including reads and expected failures. The unit-test crate and integration binaries include that same private source as `git_test_support`; it uses `orbit_common::test_env::clear_inherited_authority` before callers set deliberate environment or arguments. Core also provides a bounded `run` helper for text-output setup, preserving config isolation without production argv admission. Calls that exercise a production Git API as the subject still use that API. Keep specialized stdin, output, branch, identity, hooks/template and transport controls at the caller. A disposable child's initial scrub does not replace scrubbing each Git command, and deliberate hostile `GIT_*` inputs belong after the builder's scrub.
- **Engine fixtures that publish Git branches** create repositories from scratch, verify the root before writing, and set `GIT_ALLOW_PROTOCOL=file` plus a file-only protocol policy in the child's disposable `HOME/.gitconfig` (the engine's VCS adapter clears Git variables but keeps `HOME`). [`git_fixture.rs`](../crates/orbit-engine/tests/engine/git_fixture.rs) refuses any effective push URL, after rewrites, that is not an absolute local path; fetch failures use local upload-pack programs. Keep this policy when a test replaces its global Git config.
- **Change process-wide state only in a re-executed child,** never through a guard in the parallel libtest parent. Pass tool stub directories through `Command::env("PATH", ...)` when re-executing the test, as the engine VCS fixtures do; a scoped environment lock does not protect sibling readers. This covers forge-outage Git wrappers on `PATH` (a sibling could resolve the wrapper after its directory is dropped), tracing's callsite-interest cache in the log-capture fixture, and the Bubblewrap environment and credential-mask fixtures, whose launcher reads `HOME` and `CARGO_HOME`: they run with a cleared environment and a private `HOME` outside `/tmp`, and receive fixture variables through the child command.
- **Workflow-dispatch fixtures wait for every detached worker** to reach a terminal state in the fixture's run history before dropping the temporary home; a successful submission proves only admission. Capture all submitted IDs before assertions, including control-workspace runs, and use a cleanup guard that releases rendezvous waiters and settles workers on assertion unwinds. Wait for the owning processes to exit too, since terminal history can precede their final cleanup. The replica routine fixture exercises this with a held owner while the replica finishes. Check that the expected runs are recorded and that a disposable parent registry exposed through the managed environment gains none, as the output-goldens task-pilot fixture does.

`cargo test -p orbit-cli --test tool plugin_secrets::secret_sinks::seeded_secrets_stay_out` seeds fake provider, SCM, cloud and database credentials before redactor initialization, drives task writes and provider output through the CLI, and reports each leak by sink: task bundles, audit rows and blobs, run logs, attached diagnostics, dashboard log views and the provider's environment. Raw task attachments keep their byte-preserving contract. Its executor runs with `sandbox: off`.

### Manual QA from a managed lane

A managed lane exports Orbit routing and authority: `ORBIT_REGISTRY_ROOT`, `ORBIT_WORKSPACE`, `ORBIT_ROOT`, `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_TASK_ID` and about two dozen more. Unsetting a few of them, or overriding only `HOME`, still lets a dev binary resolve the live global registry. Before a hand-run QA command that mutates state, clear the whole environment and point `HOME` at a disposable directory. Run the steps below as one shell session, from the managed checkout:

```bash
CHECKOUT="$(git rev-parse --show-toplevel)"
ORBIT_BIN="$CHECKOUT/target/debug/orbit"   # cargo build -p orbit-cli --bin orbit --locked
QA_HOME="$(mktemp -d "$CHECKOUT/.orbit/tmp/manual-qa.XXXXXX")/home"
QA_REPO="$(mktemp -d "${TMPDIR:-/tmp}/orbit-qa-repo.XXXXXX")"
mkdir -p "$QA_HOME"
qa() { env -i PATH=/usr/bin:/bin HOME="$QA_HOME" "$@"; }

qa git -C "$QA_REPO" init -q -b main

# Confirm the global root from the scratch home, outside any repository, so no
# other workspace can be discovered.
cd "$QA_HOME"
qa "$ORBIT_BIN" config show --scope global --format json | jq -r '.global_root'

# Register the scratch repository, then confirm its routing before any mutating command.
cd "$QA_REPO"
qa "$ORBIT_BIN" workspace init --name manual-qa
qa "$ORBIT_BIN" config show --format json | jq -r '.global_root, .shared_root, .local_root'
qa "$ORBIT_BIN" workspace show --format json | jq -r '.checkout.repo_root, .checkout.orbit_dir'
```

The global root must print `$QA_HOME/.orbit`; the real `~/.orbit` must never appear. After `workspace init`, `shared_root` and `local_root` must be under `$QA_REPO`, and the workspace must report `$QA_REPO` as its checkout. Do not add `--confirm` until those paths match. Preview a destructive command first, then confirm it with the same `qa "$ORBIT_BIN" …` prefix. Stderr warnings such as "no actor identity resolved" are expected in a cleared environment.

Do not run any other Orbit command in `QA_REPO` before `workspace init`. The first command there writes a `.orbit/config.yaml` with a generated workspace identity, and `workspace init` then refuses with an identity conflict. Keep `QA_REPO` outside the checkout, not under `.orbit/tmp/`, because the checkout's own `.orbit` is an ancestor of that directory. Keep `QA_HOME` under `.orbit/tmp/`, as the fixtures do, and delete both directories when the QA is finished.

## Testing & Coverage

Test design (boundary first, unit tests by exception, binary layout) is in [test_strategy.md](design-patterns/test_strategy.md). This section covers the mechanics.

### Test process environment

- **Login-shell fallback fixtures.** `orbit-exec`'s `validation_env` integration target keeps real native bash/profile startup for exit-status and missing-marker fallbacks. Its two 300ms timeout rows use the existing thread-scoped supervisor clock: readiness at the rc stall (or before either startup marker) advances only the failing interactive probe's deadline. Healthy login startup does not consume that fixture clock; an independent five-second wall bound still expires stuck setup through the supervisor. All four rows require successful login tooling, PATH and CARGO_HOME, secret exclusion and cached no-probe reuse. The short rows also verify both attempted children were reaped. The separate failing/hanging and SIGTERM-ignoring shell cases retain real wall-clock timeout coverage. This avoids making successful fallback depend on native startup fitting within 300ms, without changing production deadlines.
- **Worker binding.** A claimed executor exports `ORBIT_WORKER_CONTEXT_REQUIRED`, so Orbit commands its agent runs refuse to start without a worker binding. A test has none, so any in-process runtime would fail with `managed worker runtime binding unavailable`. `make ci-test-affected` drops the marker; tests of the refusal set it on their own child.
- **Focused `cargo test` in a managed run.** Every test binary of `orbit-common` and `orbit-core`, and the CLI MCP integration target, calls `orbit_common::isolate_test_process!()`, which clears `INHERITED_AUTHORITY_ENV` before `main`; re-executed children keep what their parent set. Add the macro to any other crate whose tests build a runtime: under `#[cfg(test)]` in the lib and at the top of each integration target. This scrub does not isolate process-wide changes made later by a test: those complete bodies still belong in exact-test children.
- **Seatbelt on macOS.** Tests that launch Seatbelt-confined children call `orbit_exec::macos_sandbox_test_guard(test_name)`. Only a probe exit 71 with the `sandbox_apply` marker (a nested macOS executor) yields a named `SKIP: sandbox-exec cannot apply a profile: <test>: <reason>` notice, which an implementer's gate may hand off (below); a missing binary, spawn error, timeout or other failure fails the test. macOS host coverage sets `ORBIT_REQUIRE_SANDBOX_EXEC=1`, which makes an apply refusal fail too; a skipped run never satisfies `host_sandbox_test` evidence. Binary-absence tests use `sandbox_exec_available()`.
- **`ps` in a macOS agent lane.** Process-identity tests that compare Orbit's start-time token against `ps -o lstart=` run `ps` through `orbit_common::test_env::ps_lstart_utc`. Orphaned-fixture tests use `test_env::ps_stat` for process state and probe their own PID before spawning fixtures. Both helpers use the same bounded runner and spawn-denial classifier. Only a `PermissionDenied` spawn (a macOS agent sandbox refusing the setuid `ps`) lets such a test return early, and it then prints a named `DEFERRED: ps denied: <test>: <reason>` notice through `test_env::report_ps_denied`, which a claimed leaf's gate may hand off on macOS (below) [ORB-15587]; any other spawn failure or unexpected `ps` failure fails the test. The state probe accepts exit 1 with empty stdout/stderr for a process that no longer exists.
  The provider registration-refusal and registration-panic fixtures use this pre-spawn probe too. Native replay must exercise their leader, group and grandchild cleanup assertions; a denied inspection establishes neither a live process nor successful cleanup. An inspection denied after spawning fails the assertion and leaves fixture cleanup armed.
- **Default signal dispositions in re-executed helpers.** macOS can retain `SA_SIGINFO` when exec resets a caught signal handler to `SIG_DFL` (observed under nextest). The supervisor classifies `SIG_DFL` and `SIG_IGN` by their disposition value regardless of that flag. The SIGTERM helper exercises this state explicitly and asserts both the interruption notice and termination by SIGTERM, without a sandbox skip.
- **Bubblewrap on Linux.** Inside an agent-executor or job-run worktree, live `bwrap` fails with `No permissions to create new namespace` because the outer sandbox blocks nested user namespaces. That is the environment, not a defect: replay the live spawn checks (`spawn_under_linux_bwrap`, `--run-ignored`) on the owning Linux host, and never disable `linux-bwrap` or try to make it nest ([runbook](runbooks/linux-sandbox.md#verify-the-bubblewrap-boundary-natively)).
- **Deferred fixtures in an implementer's gate.** A deferred fixture prints `DEFERRED: bubblewrap unavailable: …` on Linux, the Seatbelt guard's `SKIP: sandbox-exec cannot apply a profile: …` on macOS, or `DEFERRED: ps denied: …` where the sandbox refuses `ps`, and passes, so `make ci-test-affected` can exit 0 without having run it. Inside the agent sandbox that is expected, and the implementer hands the gap off instead of failing [ORB-15287] [ORB-15409] [ORB-15587]. It runs the gate with a [validation summary](#validation-summary-and-base-reruns) in its scratch and the base pinned, then names only that summary:

    ```sh
    ORBIT_VALIDATION_SUMMARY="$PWD/.orbit/tmp/gate-summary.json" make ci-test-affected CI_TEST_BASE=<base_sha>
    ```

    ```json
    {"deferred_sandbox_validation": {"summary": ".orbit/tmp/gate-summary.json"}}
    ```

  Orbit builds the record itself [ORB-15516]. The implement step reads the summary, which must be a file under the checkout's `.orbit/tmp/`, and takes the gate's `command`, `exit_code`, `tests_run`, `base` and `notices` from it. It refuses, before commit and before any probe, unless the command is one of the owner's `workflow.required_validation_commands` or `review.baseline_commands`, the run finished with exit 0, executed tests and compared against the step's `base_sha`, and every notice is a deferral of one of the sandboxes below that the lane may defer. A gate's notices may be of several kinds: a macOS claimed leaf's broad gate prints Seatbelt and ps-denied ones together [ORB-15692]. It then runs one probe per kind in the implementer's lane (under the sandbox the implementer's provider resolves for the step, in the checkout) and refuses unless the lane refused every one of them:
  - **Linux (Bubblewrap):** `DEFERRED: bubblewrap unavailable:` notices; the probe `bwrap --unshare-user --ro-bind / / -- /bin/true` must exit nonzero with Bubblewrap's namespace refusal (`No permissions to create a new namespace` in 0.11, `No permissions to create new namespace` in 0.9).
  - **macOS (Seatbelt):** `SKIP: sandbox-exec cannot apply a profile:` notices; the probe `/usr/bin/sandbox-exec -p '(version 1) (allow default)' /usr/bin/true` must exit 71 with `sandbox_apply: Operation not permitted`.
  - **macOS claimed leaf (ps denied):** `DEFERRED: ps denied:` notices, only when the implement step is claimed (`claimed: true`) and the host is macOS; the probe `/bin/ps -o pid= -p 1` must exit nonzero with `Operation not permitted` and without `sandbox_apply: Operation not permitted`. An owner-run or non-macOS implementer printing these notices is refused.

  Only an executed probe's exit status can justify a deferral. A failure to resolve or prepare the lane, a spawn failure, a timeout or a missing exit status is recorded as `-1` and is refused, even when its diagnostic contains the sandbox-refusal text.

  The derived record (`command`, `exit_code`, `tests_run`, `base`, `notices` and `probe` with `command`, `exit_code` and `output`; when the notices are of several kinds, `probe` is the first kind's in the order above and `other_probes` lists the rest) replaces the handoff in the implement step's output, so a claimed leaf's validation reads it from there. A field the implementer writes beside `summary` is accepted only when it equals the derivation; any other key, and a hand-written record that names no summary, fail the step.

  An owner run keeps the accepted record on the task as `deferred-sandbox-validation.json`, so a resumed candidate still carries it; a later implementation that defers nothing clears it. `candidate_validate` (owner) and `claim_validate` (claimed, from the implementer output, on the claim host before publication; on a clean-base NoDiff handoff too) then run the gate after the required commands, outside any agent sandbox, where Bubblewrap creates its namespaces, Seatbelt applies and `ps` runs: a failure is judged like any required command's (a red base is `baseline_red`), a pass that still prints a `DEFERRED:` (including `DEFERRED: ps denied:`) or `SKIP: sandbox-exec cannot apply a profile:` line is a `validation_environment` failure that holds the candidate, a run that reports no executed tests through `ORBIT_VALIDATION_SUMMARY` is refused before any known-red continuation, and a passing replay is pinned with the required commands as the gate's evidence. A record of several kinds replays exactly like a single kind's. The execution summary names the gate, the summary and its notices and marks the coverage `not_run` (which belongs only in the execution summary, never as a handoff key); review reads a deferred pass as `not_run` ([review gate](runbooks/review-gate.md#host-sandbox-tests)). Nothing else is deferrable: a nonzero exit, any other `SKIP:` or `skipping` notice, a gate that ran no tests or selected nothing, notices of a kind whose probe the lane does not refuse, `DEFERRED: ps denied:` notices outside a macOS claimed leaf, a host-required criterion and a `baseline_red` command keep their handling, and `make ci-fast`, `make ci-lint`, `make goldens` and CodeQL extraction each stay individually required.

### Fixtures on a loaded host

Fixtures whose assertions concern evidence rather than elapsed execution use
scoped clocks from `orbit_core::test_support`: `with_delivery_source_clock`
controls command, lookup and whole-pass budgets; `with_cli_supervision_clock`
controls a provider's supervision deadline. Freeze time while real commands
complete, or advance it only after a child signals readiness to exercise a
timeout. The distributed-drain landing and auth fixtures demonstrate both.
For callers that use fixed deadlines through the shared process supervisor,
`orbit_common::test_env::with_supervision_clock` supplies the same scoped clock
under `orbit-common`'s dev-only `test-support` feature. Explicit caller clocks
and extendable deadlines keep their own policy. The proc-spawn, inspection-copy
and pre-push probe fixtures advance this clock only after the intended child
signals readiness; prerequisite commands retain their ordinary budgets. Remote
probe fixtures can also scale backoff with
`orbit_engine::test_support::with_remote_probe_backoff`, retaining all three
attempts and the elapsed/waited diagnostics. A fixture of what follows a
task-commit lock timeout shortens the boundary's 30 s acquisition deadline with
`orbit_core::test_support::with_task_commit_lock_timeout` (`orbit-store`'s
dev-only `test-support` feature) for the sections its thread enters; the lock
error, its holder record and the caller's handling stay real. Broker request
fixtures capture a
per-listener request timeout when starting their workers. The isolated stdin
fixture also scales its output-drain allowance with a `cfg(test)` guard and
asks its escaped helper to acknowledge kernel `POLLHUP` after finalization,
while that helper still holds the unread pipe.

These thread-local hooks restore prior clocks on unwind and require the
dev-only `test-support` feature. Cleanup and the outer child-test hang guard
keep their real clocks, so a broken handshake still fails and is reaped.

Gates often run beside a busy drain, where a fixture's wall-clock time stretches tenfold. Keep outcomes independent of host load:

- Re-execute child tests through `orbit_common::test_env::run_child_test` and verify them with `assert_child_test_passed`. The quiet window opens at the child's first byte, so exec and libtest startup do not consume it. After that byte, it kills a child that prints nothing further for `CHILD_TEST_DEADLINE` (300 s): a child that reports progress is slow, not hung. A child that never prints, or that keeps printing, runs until `CHILD_TEST_CEILING` (540 s, below nextest's ten-minute kill). Either overrun reports load averages and the child's output. The child leads its own process group; a watchdog process kills that group if the test process dies first (nextest interrupt or timeout), so no fixture child outlives its parent.
- End a wait when its event arrives, size its ceiling for a saturated host, and on expiry report what it saw plus `orbit_common::test_env::host_load()`. `test_env::wait_until` does this for a readiness condition under `FIXTURE_STEP_DEADLINE` (120 s). Bound a single CLI command or HTTP request by the same guard, and call `test_env::assert_step_finished` before asserting on a command's outcome, so a command killed at the guard does not read as a refusal.
- Dashboard fixtures launch with `--port 0` and read the actual bound address from their owned child's startup announcement. MCP signal fixtures use `127.0.0.1:0` and enable the existing bind tracing event with a fixture-only `RUST_LOG`. The shared announcement reader bounds startup and output, checks child exit before accepting readiness, and keeps draining pipes until child cleanup. An early exit drains only until the remaining readiness deadline, including partial lines and inherited output pipes. Handover needs a nonzero command-line port because exec reuses argv: its child claims a candidate directly, retrying only a confirmed address-in-use failure, and the replacement must announce the original assigned address. Never select a port by binding and dropping a temporary listener or accept a TCP connection as child readiness.
- Dashboard handover and shutdown fixtures use a shared TCP helper: one operation deadline covers connect, request writes and response reads, including SSE setup. Each socket call uses the remaining budget, so partial progress cannot reset the deadline; a failure reports the IO stage, peer and budget and unwinds through `ChildGuard`. The shutdown stopwatch starts at SIGTERM, after connection setup.
- Admission reads the host resource monitor, so a fixture not testing throttling pins a calm sample with `OrbitRuntime::with_host_resource_probe` on every runtime it opens, including reopens; a CLI fixture sets `workflow.resource_throttle.enabled` to false with `orbit config set` in its disposable config. Throttling tests drive their own probe, stamping each sample's age when the probe is read rather than at setup, so slow setup cannot make it stale.
- Prefer a deterministic behavioral guard over a timing ceiling: cold search hydration uses corrupted payloads and a canonical-read negative control to prove it skips artifact verification, with a bounded seed. When a cost guard is necessary, bound work rather than elapsed time using CPU time where possible.
- Synchronize generation fixtures with the updater's admission lock or installation rename before releasing held participants. The dashboard handover fixture hard-links the built old image into its private installation (copying only across volumes), since installation replacement never modifies the old inode; this avoids another first-exec macOS assessment. It creates its distinct, assessed replacement directly beside the installed executable and renames that inode into place, avoiding a second image copy and macOS assessment while retaining the real installation boundary. Its pre-install participant record must still name the old generation. The MCP oversized-request fixture observes structured `orbit.mcp.handover` deferral events before completing the request; elapsed time alone does not establish that a lifecycle check ran.
- macOS assesses every new executable file on its first exec and holds the process meanwhile. For the debug `orbit` image that takes seconds, and it queues behind every other new image on the host, so a timer started at spawn includes the wait. Run a freshly written binary once before anything times it: the generation fixture's `distinct_copy` calls `assess`, which runs and validates `update --contract --json` with inherited authority cleared before installation. The assessment survives a rename. When Orbit itself probes a copy it just staged, as `orbit update` does, set `ORBIT_UPDATE_PROBE_TIMEOUT_SECS` to a hang guard rather than relying on the 30 s default.
- Test runs skip durability syncs. Every `File::sync_all`/`sync_data` goes through `orbit_common::fs::io::sync_file`/`sync_file_data`, and every SQLite `synchronous` pragma takes its level from `fs::io::sqlite_synchronous`. With `ORBIT_TEST_RELAXED_DURABILITY=1` they return without flushing and the pragma becomes `OFF`. On macOS each sync is an `F_FULLFSYNC` that flushes the whole drive's write cache (about 20 ms), so concurrent suites queued behind each other's flushes and Mac gates ran 5-20x slower than Linux. `.cargo/config.toml` sets the variable in `[env]`, so `cargo test`, nextest and every `make` gate get it, and so do the `orbit` children and re-executed child tests they spawn. A value already in the environment wins: `ORBIT_TEST_RELAXED_DURABILITY=0 cargo test …` restores real syncs. The variable is read once per process, and only a build with `debug_assertions` honors it. A release or installed binary always syncs, which `orbit-common`'s `durability_syncs` tests check. A `cargo run` debug build inherits the variable too, so don't point one at state you need to survive a power loss. Only a power loss or kernel crash can tell a skipped sync from a real one. A test whose subject is the sync, or a durability or crash-recovery path that should exercise the real call, opts back in: in-process with the `orbit_common::test_env::require_durable_syncs()` guard (process-wide while it lives), or by removing the variable from a child's command, as `orbit-core`'s `upgrade_resume` does. `make ci-lint` (clippy `disallowed-methods`) and the `check-durability-syncs` guardrail in `make ci-fast` reject a direct sync or a pragma with a literal level. This deviates from STD-03@2 §R5, which requires the fsyncs, in test runs only: production durability is unchanged.
- Every `orbit` command joins the executable generation by the SHA-256 of its own image, remembered per generation root in `.generation-image-digest.json` ([upgrades runbook](runbooks/upgrades.md)). A fixture with a fresh `HOME` starts without that cache: its first command hashes the whole debug binary (about 200 MB), and the first writer, usually `workspace init`, records the digest once its join creates the root. Outside a debug test run, read-only commands before any writer hash every time. In a debug test run a process with no entry in its root reads one shared cache first: `.cargo/config.toml` sets `ORBIT_TEST_SHARED_IMAGE_DIGESTS` to `target/test-image-digests`, which holds one cache file per image name, so each test binary and the `orbit` image are hashed about once per build rather than once per fixture home. That cache keeps the root cache's identity key and settle rule: a rebuilt or replaced image misses it, and an image whose inode changed less than two seconds ago is hashed without being shared. The root's own cache is still read and written exactly as if the image had been hashed, so the first-writer rule above holds in tests too. Only a build with `debug_assertions` honors the variable, it must name an absolute directory, and a value already in the environment wins. On a shared 32-core Linux host with the CI test settings (nextest profile `ci`, six threads, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`), alternating in-place builds and runs of the whole workspace suite cut summed test time from a mean of 1,341 s over two base runs to 1,005 s over two runs with the cache (0.75x), and nextest wall time from 229 s to 173 s. The same 2,502 tests ran on both sides. Most of the saving was in binaries that spawn the CLI with fresh fixture homes (`orbit-cli::output` 0.58x, `orbit-cli::workspace` 0.61x, `orbit-web::http_api` 0.62x, `orbit-engine::engine` 0.70x, `orbit-core::runtime` 0.76x). The workspace depends on `sha2` 0.11 directly, which selects the SHA-2 instructions at run time on both x86 and Apple silicon; 0.10 hashed in software on aarch64. On an M4 Pro one digest of a 206 MB debug `orbit` took 364 ms with 0.10 and 86 ms with 0.11 (median of three sequential runs, page cache warm). That hash was the main Mac test-time contributor found: between a claimed Mac leaf run before it and one after it (different commits, about 2,650 tests each), summed test time fell from 3,588 s to 2,925 s, most in binaries that run the CLI with fresh fixture homes (`orbit-cli::output` 0.54x, `orbit-cli::tool` 0.57x, `orbit-cli::task` 0.63x). The largest Mac binaries by summed time remain `orbit-cli::process`, `orbit-core::runtime` and `orbit-engine::engine`. Encode a digest with `orbit_types::identity::lower_hex`, since 0.11 outputs do not implement `LowerHex`, and import `sha2` directly rather than through `rsa::sha2`. Share one fixture `HOME` across a test's commands rather than giving each command its own.
- Seeding pays an fsync per task outside relaxed test runs. Root a fixture that seeds thousands at `tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())`, which prefers tmpfs on Linux, and report its phases with `orbit_common::test_env::FixtureProgress` so an overrun names the phase. Its deadline bounds the gap between two items (`FIXTURE_STALL_DEADLINE`), not the fixture's total, and its stderr reports keep an isolated child's quiet deadline from expiring while it advances; `FIXTURE_CEILING` bounds the whole fixture.
- A production fallback taken under load (such as a timed-out login probe) may satisfy a fixture only if the result reports the fallback and its reason.

### Tests that submit pipeline runs

Submitting a run (ship, resume, auto, job) spawns a detached worker by re-executing the current binary. From a test, that binary is the libtest harness, which would rerun the spawning test without bound ([RCA](dev/rca/2026-09-23-cross-crate-test-worker-oom.md)), so only the CLI `main` may act as a worker and an unsubstituted submission fails with `OrbitError::Execution`.

Install a substitute with `orbit_core::test_support::install_substitute_pipeline_worker` (`crate::test_support` inside `orbit-core`), enabling the orbit-core `test-support` feature from `[dev-dependencies]` only. It is process-wide and the last install wins, so it covers other threads (such as a dashboard's blocking pool); install one argv per test binary, as `crates/orbit-web/tests/http_api/support.rs` does. `{run_id}` in the argv becomes the run id. CLI tests that run the real `orbit` binary use its production entry point instead.

### Tests that depend on host process visibility

`orbit_common::process::identity` derives process-start tokens from `/proc` on Linux, libproc on macOS and `ps -o lstart=` elsewhere, keeping the UTC/C-locale `ps` format so persisted tokens stay compatible. When the probe is `Unavailable`, production takes its fail-safe branch: an owner it cannot verify is neither finalized nor signalled.

A test whose subject is the token first calls `orbit_common::test_env::start_identity_probe_blocker()`. `None`: assert fully. `Some(reason)`: return early or assert the fail-safe branch, and log `reason`. Never weaken the full assertion. For a live process outside the test, use `orbit_common::test_env::spawn_unrelated_process()`, not pid 1, whose start time macOS hides from unprivileged callers.

### Process fixture cleanup and readiness

- Launch dashboard and persistent MCP readiness fixtures with `BoundServer::spawn` ([`bound_server.rs`](../crates/orbit-cli/tests/support/bound_server.rs)). On Unix it reserves a process group with a separate unreaped child blocked on an owned stdin pipe. The anchor reserves the group identity even after the server is explicitly reaped, so teardown can kill inherited-pipe holders without signaling a reused PID or group. Pipe readers own their nonblocking descriptors (Windows readers inspect available bytes before reading); cancellation bounds draining and joins every reader without detaching or closing another thread's handle. Assertion teardown shares a two-second reader/child cleanup budget. Descendants that intentionally leave the owned process group require their own identity-bound guard.
- Hand every dashboard and persistent MCP child in `crates/orbit-cli/tests` to `ChildGuard` ([`child_guard.rs`](../crates/orbit-cli/tests/support/child_guard.rs)) right after spawn, before pipe extraction or readiness checks. It sends SIGTERM, waits two seconds, then kills and reaps, also on assertion unwinds and across executable handover, and never re-signals a reaped PID. Explicit shutdown tests may still signal and wait on the child.
- A backend reaped by a separate supervisor needs identity-bound cleanup. The [signal fixture guard](../crates/orbit-cli/tests/process/backend_guard.rs) obtains a Linux pidfd through `SO_PEERPIDFD` on a private Unix connection (Linux 6.5+) or reads the backend's own macOS PID/version identity from its readiness marker. Liveness follows that identity, and cleanup signals it atomically. A start-time check followed by `kill(pid, signal)` still races with PID reuse. The backend re-executes an ignored child entry point from an assessed copy of the integration binary inside the plugin's existing read/execute grant, and does not exec again after announcing its identity. Linux readiness needs no backend `/proc` access or extra sandbox grant.
- Wait for observable readiness under a generous ceiling and measure shutdown separately (SIGINT/SIGTERM ceiling: ten seconds). Readiness waits detect early supervisor exit and keep its output. Never add launch retries or relax shutdown assertions to hide startup failures.
- Observe persisted state rather than CLI timing: the registry-lock fixture reads the worker's run through a read-only SQLite connection while holding the registry writer.
- Before asserting a task's final state after a detached resume, wait for the recorded worker (PID and start identity) to exit, within a deadline: the run turns terminal before coupled-task cleanup finishes.
- Keep disposable homes and checkouts alive through detached worker exit, including assertion unwinds. Arm fixture cleanup before submitting, so a failed command or malformed response still owns any admitted run. Enumerate the private registry, wait for pending workers to claim, complete or cancel each run, and probe its recorded PID/start identity until it has exited. A terminal row alone does not establish cleanup. If exit cannot be confirmed, retain the roots and fail the fixture rather than deleting paths a worker may still use. The env-pass warning fixture exercises this ordering with a held worker and an unwind.

### Tests and macOS temp paths

macOS `TMPDIR` is under `/var/folders`, and `/var` is a symlink to `/private/var`. Orbit resolves the roots it is given, so fixtures there have a symlinked ancestor Linux CI never sees.

- Production code treats a symlinked ancestor above the Orbit or workspace root as ordinary: compare resolved with resolved, never resolved with unresolved, and police symlinks only inside the tree Orbit controls.
- A fixture that compares Orbit's reported paths with paths it spelled creates its temp dir with `tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir())`, or canonicalizes its side. A bulk-write fixture still needs that resolved spelling: `bulk_write_temp_dir` keeps the tmpfs but returns the spelled path (`/var/...` on macOS, `/dev/shm` when the process temp is not a tmpfs). Canonicalize that parent before `tempdir_in`. A test that must hold everywhere builds its own symlink with `std::os::unix::fs::symlink`.
- On unrestricted hosts keep the default `TMPDIR`. Managed executors keep scratch, logs, evidence and task-artifact source files in a run subdirectory of the injected `ORBIT_SCRATCH_DIR` (`.orbit/tmp`). For Cargo test gates, point the test process's `TMPDIR` at a tmpfs the executor sandbox can write, using `/dev/shm` on Linux when present and writable. A file-and-directory fsync probe measured a 1,145.6 ms median on `.orbit/tmp` and 0.0 ms on tmpfs, so fixture temp roots on disk can dominate a gate. This follows `test_env::bulk_write_temp_dir`: on Linux it prefers the system temp when already tmpfs, otherwise the first writable tmpfs among `/dev/shm` and `/tmp`, then falls back to the system temp. For the [cross-revision helper](runbooks/compiler-cache.md#cross-revision-beforeafter-validation), follow its runbook: set the helper's own `TMPDIR` to an existing run directory beneath `ORBIT_SCRATCH_DIR` and keep its workdirs and logs there. The helper requires `ORBIT_SCRATCH_DIR` as explicit authorization for workdirs in Orbit state; `TMPDIR` alone does not grant it. To put Cargo fixture temp roots on tmpfs too, override `TMPDIR` for the Cargo command after `--` (for example, `-- env TMPDIR=/dev/shm cargo test ...`). Use only paths the executor sandbox permits.
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

Browser checks need Playwright and Chromium, plus Chromium's shared libraries and a font configuration on Linux. A prepared host has a kit at `~/.local/chromium-deps`, built and verified by `orbit browser-kit install` (`status`, `verify` and `remove` manage it; see the orbit-setup [browser kit reference](../crates/orbit-core/assets/skills/orbit-setup/references/browser-checks.md)). Once it is verified, every agent receives `PLAYWRIGHT_BROWSERS_PATH`, `PLAYWRIGHT_MODULE` (Playwright's `index.mjs`) and `ORBIT_BROWSER_KIT_ENV`, the kit's `env.sh`, which on Linux also exports `LD_LIBRARY_PATH` and `FONTCONFIG_FILE` when the kit unpacked them. Source it in the shell that runs the check; in a hand-run shell, source `~/.local/chromium-deps/env.sh`. `orbit doctor` reports a missing, mismatched or unverified kit, unresolved libraries and `node` missing from workers' PATH. On a host without a kit, run `orbit browser-kit install` there rather than installing Playwright into a checkout: `.orbit/tmp/` is cleared, so it holds only a sandboxed run's disposable copy when the kit is absent ([runbook](runbooks/website-validation.md#stage-playwright-and-chromium-without-root)). Skia FontConfig crashes and missing `lib*.so` errors are host evidence, not a failed check.

```bash
. "${ORBIT_BROWSER_KIT_ENV:-$HOME/.local/chromium-deps/env.sh}"
node crates/orbit-web/src/tests/dashboard_operations_browser.mjs \
  "$PLAYWRIGHT_MODULE" .orbit/tmp/dashboard-browser
```

`.orbit/tmp/` is cleaned with the worktree, so screenshots kept only there are lost. A task whose diff changes something a user sees (the dashboard, the website, docs pages, a desktop app UI) attaches screenshots of each changed view as task artifacts, whether or not it is tagged `ui`:

- every theme the view supports (the dashboard is dark-only; the website has light and dark), and a 375px phone width when the layout is responsive;
- a before shot from the base revision when it is practical and it shows the change;
- each attached with `orbit.task.artifact.put` from `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`) at a stable path such as `screenshots/<view>-<theme>-<width>.png`, and the paths listed in the execution summary;
- each under the 1 MiB artifact limit (`MAX_TASK_ARTIFACT_CONTENT_BYTES`): prefer a viewport capture to a full-page one, or JPEG or WebP when a PNG is too large.

When the host has no browser kit or the view cannot render, say so in the handoff instead of claiming visual verification. A claimed leaf on another host attaches the same way; the broker forwards the put to the owner.

To check a fixture under the worker sandbox on macOS without dispatching a run, substitute the placeholders in the leaf-worker profile golden and run the fixture under `sandbox-exec`:

```bash
SCRATCH="$(mktemp -d)"
sed -e "s#<HOME>#$HOME#g" -e "s#<ROOT>#$SCRATCH#g" \
  crates/orbit-exec/tests/sandbox_profile_goldens/macos/leaf_worker.sbpl >"$SCRATCH/leaf_worker.sb"
sandbox-exec -f "$SCRATCH/leaf_worker.sb" /bin/zsh -c '. ~/.local/chromium-deps/env.sh &&
  node crates/orbit-web/src/tests/dashboard_operations_browser.mjs "$PLAYWRIGHT_MODULE" '"$SCRATCH"'/dashboard-browser'
```

The golden only approximates a live run's profile, so use a real worker run as the final proof.

### Dashboard browser fixtures

The gzip forward check uses two real dashboards and the stub SSH tunnel.
Set `ORBIT_PLAYWRIGHT_MODULE` and `ORBIT_COMPRESSION_BROWSER_EVIDENCE_DIR`,
then run the ignored HTTP test
`host_forward::gzip_forward_returns_json_to_the_browser`. Chromium fetch must
decode the forwarded routines and job-runs JSON; `result.json` records both
responses in the evidence directory.

The run-detail scenarios also check the Requests held panel for open and half-open
breakers, refused settlements and owner-busy backoff, including healthy-drain absence
and safe text wrapping at 1440 and 375 px. `dashboard_loading_browser.mjs --run-detail`
(with the usual module and evidence arguments before the flag) writes
`requests-held-result.json` and `requests-held-*.png`. Both browser color preferences
are checked; the dashboard retains its supported dark palette in each.

Each runs as `node <script> "$PLAYWRIGHT_MODULE" .orbit/tmp/<evidence-dir>` and saves screenshots and any JSON named below. The dashboard is dark-only, so there is no light-theme pass. The loading and distributed scenarios also run in the QA sweep.

The Cost scenario in `dashboard_loading_browser.mjs` measures every rendered model-token caption against its computed card background at 1280, 1440 and 375 px, and checks visible Cost text for disabled-token use (`cost-contrast-result.json`, `cost-{1280,1440,375}.png`).

| Script | Evidence dir | Covers |
|---|---|---|
| `crates/orbit-web/src/tests/dashboard_loading_browser.mjs` | `loading-browser` | Task search dismissal (`--task-lookup` narrows to this control): Clear beside search works with empty input and listed or filtered-out ID results, direct links, keyboard Enter/Space at 1440 and 375 px, query/route reload and history, not-found/server errors and held lookup/hash/refresh responses (`task-lookup-clear-result.json`, `task-lookup-*.png`, `task-deep-link-*.png`). The UTC/Local clock (`--clock` narrows to this check): with unchanged API responses in America/Los_Angeles, switching repaints an open task detail's History and comment times and an unchanged run row's time, while the detail keeps an opened section, a comment draft with its selection, and focus (`clock-task-local-dark-1440.png`, `clock-task-local-dark-375.png`). Task labels and navigation (a run's task link opens the task under the operator's filter, a copied URL reopens it in a new tab, Back returns to the run, and a link into another workspace adds one history entry so Back and Forward restore each entry's workspace and requests; `task-link-history-result.json`), workspace scope (the Events workspace chip names the selected workspace as the header picker does, in one- and multi-workspace mode; actual host-picker transitions with a shared workspace ID remove or restore the chip removal control to match the destination cardinality, hold the destination workspace response open while current and retired chip activations preserve workspace state, request scope and the full URL (`events-host-loading-dark-{1440,375}.png`), preserve sole-workspace request scope after loading, and allow aggregate selection on return to multiple workspaces (`events-host-scope-result.json`, `events-host-single-dark-{1440,375}.png`); a host or window change retires the previous Reliability report while the replacement read is held or fails; a same-scope refresh keeps it labeled refreshing or stale), responsive layout, row keyboard access, Ship dispatch. Controlled `/api/fleet/drains` responses show a drain outside the idle selected workspace in the header; opening the header shows both fleet rows and retains the count in aggregate scope without workspace readiness or crew reads (`result.json` → `fleetScope`). These mock responses exercise shipped dashboard assets, not production drain execution. Task titles stay on one line with an ellipsis and a consistent compact row height, the full title stays reachable by tooltip and accessible name, and the status/crew chips reveal their native editors on focus or click. Checks title capacity beside the default dock at 1280×800 and 1440×900, the stacked row at 390 and 375 px, and edits (`task-titles-result.json`, `task-titles-<width>.png`): Tab reveals the crew editor before a selected-option mutation; status uses native keyboard typeahead (`i` for `in-progress`) and requires a trusted change event plus the exact status PATCH. Native popup ArrowDown/Enter does not select an option in macOS headless Chromium. A saved status change from the detail pane closes it, clears `open=` and keeps its feedback and undo on the row, or in a notice when the new status is filtered out; a failed or declined change keeps the detail open with its feedback, and a refreshed status leaves rows as they were (`task-status-folded.png`, `task-status-notice.png`). Review and in-progress rows and the detail link a task's pull request only when its `github-pr` ref has an http(s) page (`pull-request-links-result.json`). `--run-detail` narrows it to run detail, cancel/replay and child outcome tallies, state colors, durations and navigation at desktop/mobile widths (`run-detail-result.json`, `run-actions-result.json`, `run-child-outcomes-result.json`). |
| `crates/orbit-web/tests/http_api/dashboard_provider_usage_browser.mjs` | `provider-usage-browser` | Runs through `cargo test -p orbit-web --test http_api provider_auth::dashboard_top_bar_shows_provider_usage_on_every_tab -- --ignored --exact --nocapture` with `ORBIT_PLAYWRIGHT_MODULE="$PLAYWRIGHT_MODULE"` and `ORBIT_PROVIDER_USAGE_BROWSER_EVIDENCE_DIR="$PWD/.orbit/tmp/provider-usage-browser"`. Seeds stored readings (one stale, one with gating off), then checks the top bar's per-provider usage chips (name, worst value, meter and pressure, set apart by a divider only while the host warning chip is shown; no native tooltip) against the real server at 1920 px and the same chips on five destinations. It then replaces the readings with a busy host's (a five-window scoped provider, a gating-off one and `+2 more`) to drive the dialogs with a real pointer, keyboard and touch (hover intent and grace, focus, click and Pin, Esc, outside click, one dialog at a time, `aria-expanded`, dialog placement inside the viewport), resizes across 1900 px with the all-providers dialog open or as a focus preview (`aria-expanded` and keyboard focus move to the chip on screen, Tab reaches Pin, Esc returns focus, and both stay there through an unchanged poll), and a provider's dialog closes with focus moved to the summary chip; the summary chip's all-providers dialog at 1440, 1280 and 375 px (`topbar-*`, `provider-*`, `provider-resize-*`, `all-providers-*`, `summary-*` screenshots, `result.json`). |
| `crates/orbit-web/tests/http_api/dashboard_run_events_browser.mjs` | `run-events-browser` | Newest event page and total count, earlier pages, timeline retry markers and legend independent of paging/Refresh, delayed retry responses after run/workspace round trips, retry truncation and scan failure/recovery, ISO day labels and absolute seconds on the dashboard clock, activity folding within one invocation and separation of interleaved children (including equal or missing messages and legacy missing parents, `activity-result.json`), first-load skeleton and loaded steps retained during delayed Refresh, read failure, desktop and narrow layout (`result.json`). |
| `crates/orbit-web/src/tests/dashboard_operations_browser.mjs` | `dashboard-browser` | Responsive shell at 375×812: the first panel or view tabs start at or above y=200 while every destination and active view remains reachable by keyboard; panel layout and focus, Jobs Live / History / Catalogue visits and retired list routes, Catalogue routine navigation, Drain blockers and the **Proposed tasks** control, the Start window result beside Start on Tasks and again on Drain (pending, success, refusal and lost response, one POST each, `tasks-dock-feedback-*.png`, `drain-feedback-lost-*.png`), the Drain **Start a window** composer (every `orbit run auto` option with its flag, the live command and Copy, CLI-worded refusals for local and pull, the claim token kept out of the command, confirm and receipt, `drain-start-*.png`) and the Overview's **How this window was started** grid and pull claim budget, auto-task status tokens, a task's running run with a long job id wrapping inside its 280px Running now card while the task and run links stay scoped (`running-now-280.png`). |
| [dashboard_rail_browser.mjs](../crates/orbit-web/src/tests/dashboard_rail_browser.mjs) | `rail-browser` | Rail label/count geometry at 1440, 1280, 1024, 761, 760, 390 and 375 px. Health and Analytics retain readable labels, Health with its three counts (doctor, denials, routines), including large pills and removal of the attention count; desktop attention pills use a second line, while mobile destinations remain horizontal. Checks text clipping, sibling separation, button containment and page overflow, and captures every mobile destination. Repeated destination and Health subtab changes through the shipped router keep one indicator per bar (`measurements.json`, `rail-<width>-<case>[-<tab>].png`). |
| `crates/orbit-web/src/tests/dashboard_orchestrators_browser.mjs` | `orchestrators-browser` | Analytics › Agents › Orchestrators ranking header boxes do not overlap at 1440 and 375 px and the ranking scrolls in its own box at 375 px, the Jobs badge reads `N runs`, and the UTC/Local control defaults to UTC, switches the refresh line, and survives a reload. The refresh line and the one clock control sit at the foot of the rail at every width from 1024 px while the top bar stays one row (`agents-orchestrators-chrome-dark-<width>.png`, `clock-dark-<width>.png`, `jobs-rail-dark-<width>.png`, `result.json`). |
| `crates/orbit-web/tests/http_api/dashboard_agents_browser.mjs` | `agents-browser` | Analytics › Agents from contract-shaped `/api/agents` payloads: `#analytics/orchestrators` opening `#analytics/agents/orchestrators`, role tab counts, row selection driving the detail column, "How the score works" staying open across a refresh that brings a newer scan, the Reviewers provisional banner, `accepts often` tag and verdict-outcome bars, the Pilots tier matrices, Implementers with no role panel and no empty panel grid row or its gap at 1440 and 375 px (that track is 16 px and 12 px after the disclosure; `implementers-track.json`), the not-computed and empty states, `all` labelled as 90d, and the 375 px layout (no page overflow, ranking scrolling in its own box, detail stacked below, 44 px touch targets) (`agents-<role>-dark-<width>.png`, `result.json`). |
| `crates/orbit-web/tests/http_api/dashboard_fleet_drains_browser.mjs` | `fleet-drains-browser` | Initial Tasks and Run detail render local data while a remote fleet response is held; warmup and the first refresh share one in-flight fleet read per host, and late completion updates the indicator while preserving the selected view (`warmup-*-held.png`). Fleet indicator from an idle default workspace, host errors, idle eligible work, keyboard selection on a remote host and row selection on the serving host through the complete app, and 375 px page/card geometry with one line per workspace that has nothing waiting. With two registered hosts, one refresh on Tasks, Run detail, and the other listed views issues at most 10 requests and does not read `/fleet/drains`; the indicator and fleet card still show phase, slots, eligible count, top wait, and holds (`result.json`, `fleet-375.png`). |
| `crates/orbit-web/tests/http_api/dashboard_durations_browser.mjs` | `durations-browser` | Duration formats and wrapping in the Metrics actor tables and Automation, running `↻` durations (`measurements.json`). |
| `crates/orbit-web/tests/http_api/dashboard_metrics_browser.mjs` | `metrics-browser` | Analytics › Metrics trends: duration series cards and deltas, line gaps and hollow low-sample points, hover and keyboard tooltips, series toggles (the last one stays), Succeeded / All outcome filtering with URL reload and history, statistic and Table views, the Tasks completed stacks, the Defects lineage and origin groupings, detail cards, a window change, the empty window's widen action and a 390px phone (`metrics-assertions.json`). |
| `crates/orbit-web/tests/http_api/dashboard_resource_profiles_browser.mjs` | `resources-browser` | Analytics › Resources from its rail subtab: the work-class selector (Change, No-diff, a template), complexity series ending at their horizons on the shared minutes axis, the dimmed and hollow sparse profile, hover and keyboard tooltips with the live-region reading, series toggles (the last one stays), % of host with both admit thresholds, the profile table, the empty state, the aggregate placeholder issuing no read, an OS light preference and a 390px phone (`resources-assertions.json`, `resources-*.png`). |
| `crates/orbit-web/tests/http_api/dashboard_audit_browser.mjs` | `audit-browser` | Health › Incidents › Events and Analytics › Reliability › Tools tables across widths; Tools window controls through the complete app (24h/7d/30d clicks, summary requests, shared state and URL reload, labeled 24h fallback for 1h/all, roving keyboard focus and 1440/375 px layout; `tools-windows.json`, `tools-window-<window>-dark-<width>.png`, `--before` captures the base view); flush Tools column bottoms with either column taller and every nonempty subset of side cards at 1500/1600/1920 px, with natural single-column spacing at 1499/375 px (`tools-columns.json`, `tools-columns-<case>-<mask>-<width>.png`); a long Events list growing the panel under the pane's scroll, keyboard expansion, Events paging (Load older, shown-of-window header, refresh keeping loaded pages, status and hide-unconfirmed filters on every page). Held responses order an older page against a filter change: a retired older page that succeeds while the new head is pending, or succeeds or fails after that head fails, paints no rows or paging feedback, and a same-scope refresh landing under a held older page keeps the loaded tail. The full-width Analytics › Codebase views: the Hotspots table (signal columns, flagged paths first), a poll keeping the scroll, a path opening its file view (tiles, regressed tasks, the below-minimum Changes with message and Use all time) and the back link, the Group by segment, view tabs and Coupling empty-state action rewriting the hash, the toolbar wrapping and the table scrolling in its own box at 375 px against 1280 px, and 4.5:1 text contrast in Hotspots, the file view and Coupling (`codebase-<width>.png`, `codebase-file-1440.png`). |
| `crates/orbit-web/tests/http_api/dashboard_task_create_browser.mjs` | `proposal-browser` | Runs through `cargo test -p orbit-web --test http_api task_create::dashboard_proposal_form_retry_and_scope_recovery -- --ignored --exact --nocapture` with `ORBIT_PLAYWRIGHT_MODULE="$PLAYWRIGHT_MODULE"` and `ORBIT_CREATE_BROWSER_EVIDENCE_DIR="$PWD/.orbit/tmp/proposal-browser"`. Uses a disposable HTTP server and browser profile to check cancellation, inline validation, proposed-only creation, committed/lost-response retry, reload, frozen key/payload/target, double submit, scope changes, aggregate target selection, correction after refusal and mobile layout; also boots the complete dashboard. Saves `proposal-result.json` and `proposal-mobile.png`. |
| `crates/orbit-web/tests/http_api/dashboard_polish_browser.mjs` | `polish-browser` | Settings, Health, Analytics and Automation deep links, including Analytics › Agents › Pilots; at 375 px, load/reload with counts arriving after routing keeps the destination and selected sub-view visible together, as do clicks through every rail sub-view (`navigation-<route>-dark-375-<visit>.png`, `navigation-<section>-click-dark-375.png`); a click walk through every Observe destination, rail view and secondary tab (Incidents, Events and Errors; Pipeline and Tools) checking the one visible view, the active rail entry, view and tab, the hash, remembered views and filters, Back and direct loads (`observe-<n>-<route>.png`); crew labels and usage references, Crews geometry (`measurements.json`). |
| `crates/orbit-web/src/tests/dashboard_refresh_browser.mjs` | `refresh-browser` | Failed and recovered refreshes: retained counts with `as of HH:MM`, the connection dot, failed run streams. |
| `crates/orbit-web/src/tests/dashboard_host_switch_browser.mjs` | `host-switch` | Host switcher: requests routed through `/api/on/<host>/…`, host precedence, failure and recovery. The log tail drops the old host's rows, buffered count and status line at the switch, while the new host's snapshot is held, failed, stale or empty, and reopens the stream from that host's cursor. Back and Forward between hosts, with a shared workspace ID and with different ones, restore routing, both pickers and write capability, and a held workspace list from the host left behind repaints nothing. Back and Forward from a single-workspace host restore All workspaces on a multiple-workspace host, preserve aggregate intent while destination discovery is held, and fetch aggregate tasks or runs after it settles; a single-workspace destination selects its only workspace and repairs the URL. State assertions import the same `/static/js/common.js` module as the shipped app (`host-switch-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_doctor_browser.mjs` | `doctor-browser` | Health › Doctor rows, remediation, cached report age, the busy state of a first run and of a refresh while the fixture holds the response (Refresh disabled, progress shown, controls restored after success or failure), a cached read delayed past a newer refresh or a host switch never replacing the shown report or rail count, a newer cached report still shown, remote and too-old hosts (`doctor-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_incidents_browser.mjs` | `incidents-browser` | Health › Incidents class tiles and filtering, lifespan bars for a recurring and a single-burst signature, the detail panel (full message, Copy, truncation note and its audit drill-down, related signatures, runs), last-hour and no-message filters, cause headers over signatures that share a cause, the By cause / By signature toggle and the same-cause list with its grouping basis, the local 6h window, the sticky panel and a 375px phone. Full-message cache isolation uses independent host stores with the same audit row ID, plus held successes and errors across host/workspace round trips. The full dashboard covers a shared concrete workspace; a browser module harness covers All workspaces with the shipped common.js routing because the dashboard currently gates Incidents behind a concrete workspace (`incidents-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_provider_limits_browser.mjs` | `provider-limits-browser` | Crews Limit column, Drain provider-limit waits, contrast under both emulated colour schemes (`provider-limits-assertions.json`). |
| `crates/orbit-web/tests/http_api/dashboard_task_block_browser.mjs` | `task-block-browser` | Blocked-task explanation in the dock and full page at 1440 and 375 px: reason, absolute timestamp, recorded next step, matching escalation comment and scoped run link. Checks explicit missing-reason/time fallback and absence on other statuses; captures each changed view (`results.json`, `task-block-{dock,page,unknown}-dark-<width>.png`). `--before` captures the same fixture without asserting the new callout. |
| `crates/orbit-web/tests/http_api/dashboard_task_panel_browser.mjs` | `task-panel-browser` | No Rust runner: source the kit environment first (`. ~/.local/chromium-deps/env.sh`; without its `LD_LIBRARY_PATH` Chromium fails to launch, for example on missing `libatk-1.0.so.0`), then run it directly as `node crates/orbit-web/tests/http_api/dashboard_task_panel_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/task-panel-browser`. The Tasks side pane on a 31-task fixture: a row click opens the pane to the right without moving the row, rebuilding the list or losing its scroll, and a refresh keeps both; `aria-expanded`/`aria-controls` and `open=`; the five tabs in order, Runs read from `/api/job-runs?task_id=` only once its tab opens, History, Comments, Artifacts and tab-list arrow keys; drag, keyboard (End clamp, Shift step) and double-click resizing, the width stored in `localStorage` and restored with the selection on reload, and refused storage falling back to the default without page errors. Keyboard: j/k and Up/Down move the selection, j inside the pane keeps focus on the new heading, Enter opens `#tasks/<id>` with the list hidden, Back to list restores the scroll, selection and row focus, Esc and the close control close the pane. Deep links: `open=` reveals its row, a direct `#tasks/<id>` returns to the list with the task selected. Row Approve and View run act without opening the pane. A done task outside the filter, pinned by `#tasks/<id>` or `open=`: the full page at 1440 and 375 px drops the outside-the-list box and shows the same visibly labeled Crew select in the lever row for listed and pinned tasks, with matching geometry and styles; crew changes save from both pages and the dock, which keeps its outside-the-list note. Side-by-side full-page comparisons are captured at both widths. At 375 px the pane is a full-screen modal sheet named by its heading over an inert page, and Back releases it and returns focus (`task-panel-result.json`, `tasks-panel-dark-1440.png`, `tasks-page-dark-1440.png`, `tasks-sheet-dark-375.png`, `tasks-page-pinned-dark-1440.png`, `tasks-page-pinned-dark-375.png`, `tasks-page-listed-dark-1440.png`, `tasks-page-listed-dark-375.png`, `tasks-page-comparison-dark-1440.png`, `tasks-page-comparison-dark-375.png`, `tasks-dock-pinned-dark-1440.png`, `tasks-dock-pinned-dark-375.png`). |
| `crates/orbit-web/tests/http_api/dashboard_context_contention_browser.mjs` | `context-contention-browser` | No Rust runner: source the kit environment (`. ~/.local/chromium-deps/env.sh`, which sets `LD_LIBRARY_PATH`/`FONTCONFIG_FILE` for Chromium's shared libraries), then run it directly as `node crates/orbit-web/tests/http_api/dashboard_context_contention_browser.mjs "$PLAYWRIGHT_MODULE" .orbit/tmp/context-contention-browser`. Checks context contention on disposable API data: reservation holders, contended backlog (amber) and active (red) selector rows whose full selector identity stays in the link `title` and contended `aria-label` while the visible text is the basename or kind-prefixed name, scoped blocker links, contrast, refresh clearing and recovery, and readiness wording (`results.json`, `backlog.png`, `active.png`, `locks-*.png`, `drain-summary-*.png`, `readiness-*.png`). |
| `crates/orbit-web/tests/http_api/dashboard_context_basename_color_browser.mjs` | `context-basename-browser` | Runs through `cargo test -p orbit-web --test http_api dashboard_assets::context_selector_basenames_keep_contention_colors_and_return_to_normal_on_refresh -- --ignored --exact --nocapture` with `ORBIT_PLAYWRIGHT_MODULE="$PLAYWRIGHT_MODULE"` and `ORBIT_CONTEXT_BASENAME_BROWSER_EVIDENCE_DIR="$PWD/.orbit/tmp/context-basename-browser"` after sourcing the kit environment; the same script also runs directly as `node <script> "$PLAYWRIGHT_MODULE" .orbit/tmp/context-basename-browser`. Checks that a contended selector's basename keeps the theme contention colour (`--warn-fg` backlog, `--danger-fg` active) for root-level and duplicate-basename files — duplicates are distinguished by their folder groups and full selector in the link `title`, with no duplicate marker — and returns to the normal colour after a refresh clears contention (`results.json`, `backlog-dark-1440.png`, `active-dark-1440.png`, `cleared-dark-375.png`). |
| `crates/orbit-web/src/tests/dashboard_task_artifacts_browser.mjs` | `task-artifacts-browser` | Task detail artifact previews under the dashboard's real CSP (read from `DASHBOARD_CSP`): a mounted image renders, opens in a new tab and downloads, and a download saves its bytes, from a blob URL that closing, replacing or changing workspace away from the detail revokes; a response held before its headers or inside its body is cancelled when the detail retires, and one that settles afterwards anyway is released without being mounted; clicks during a read do not refetch (`task-artifacts-assertions.json`, `artifact-preview.png`). |

### Website changelog validation

After changing the changelog renderer, run `npm ci`, `npm run build` and `npm run check` in `website/`, serve `website/dist`, then:

```bash
node website/scripts/check-changelog.mjs \
  "$PLAYWRIGHT_MODULE" http://localhost:4187 .orbit/tmp/changelog-browser
```

It checks release dates, merged-PR links, version and subsection anchors, keyboard and no-JavaScript disclosure controls, the version badge and future-dated headings, and records heights and screenshots at 1440px and 375px in both themes. Source `~/.local/chromium-deps/env.sh` for `PLAYWRIGHT_MODULE` and `PLAYWRIGHT_BROWSERS_PATH`. In a restricted executor, keep browser output, screenshots and JSON evidence under `.orbit/tmp/`; point transient `TMPDIR` at a sandbox-writable tmpfs when available. A minimal Linux runner may also need `FONTCONFIG_FILE` naming a config with local fonts.

## MCP Apps compatibility prototype

[The isolated reproduction and native desktop probe](dev/qa/mcp-apps-probe.md) covers the Control Center, operator controls, rendered preview, routing boundaries and evidence template. Record automated protocol/bridge results and native desktop results separately.

## Desktop app

`desktop/` is the Tauri shell around the dashboard, a separate Cargo workspace (its own `[workspace]` table), not a member of the root one. The workspace gates (`make ci-fast`, `ci-test-affected`, `ci-lint`, `goldens`) never build it. Validate a change from `desktop/` with `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`, which drives the server supervisor against a fake `orbit` without a webview. The root `clippy.toml` applies there too. `cargo test --locked --no-default-features --test watcher_notifications` runs the polling/notification integration regressions alone; this target needs no native webview packages. Then run the app with `cargo run` against your installed `orbit`. `cargo clippy` doesn't rebuild the executable, so build before a manual run. [`desktop.yml`](../.github/workflows/desktop.yml) runs the same checks, including the webview-free watcher regressions, on macOS and Linux. Its separate Linux advisory job checks the committed desktop lockfile without building the native stack. The workflow runs on changes to `desktop/`, the workflow, `deny.toml`, the cargo-deny/download wrappers or the advisory regression; see [Supply-chain](#supply-chain-cargo-deny) for commands and exceptions. A manual run also bundles unsigned packages: a macOS disk image, and the Linux `.deb` and `.AppImage` from the same runners, targets and command as the release's Linux legs. Release tags build the signed bundles in `release.yml` ([Releases](../desktop/README.md#releases)). Process lifecycle, binary discovery and the security model are in [`desktop/README.md`](../desktop/README.md).

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

## Windows

Orbit supports macOS and Linux. On Windows the supported route is the Linux build inside [WSL2](runbooks/windows-wsl2.md). Native Windows is not built or checked in CI.

- Code using Unix-only APIs (`libc`, `std::os::fd`, `std::os::unix`) sits behind `#[cfg(unix)]`; its `#[cfg(not(unix))]` counterpart returns an explicit unsupported error or `Unknown` and never invents Windows semantics.
- Tests that need those APIs or drive a `#!/bin/sh` fake are gated `#[cfg(unix)]` at the narrowest level (test, module, or `#![cfg(unix)]` for a POSIX-only file). Gate them; never delete them.

## Supply-chain (cargo-deny)

[`cargo-deny`](https://embarkstudios.github.io/cargo-deny/) checks two independent Cargo workspaces using the shared [`deny.toml`](../deny.toml). The main PR job runs all checks on the root graph through `scripts/ci-guardrails.sh`: advisories, yanked versions, licenses and sources. That command does **not** cover `desktop/Cargo.lock`. The **Desktop advisories** job in [`desktop.yml`](../.github/workflows/desktop.yml) separately runs the advisory check against `desktop/Cargo.toml` with `--locked`, including native default dependencies and all target platforms. It fails if cargo-deny is missing. Both lanes pin cargo-deny 0.19.9; the desktop lane never adds a native build to the main Check job's 600-second budget. Run both commands from the repository root before landing a dependency change:

```bash
cargo install cargo-deny --version 0.19.9 --locked   # once
make audit                                         # root graph, all checks
scripts/cargo-deny.sh --manifest-path desktop/Cargo.toml --locked check advisories
python3 scripts/test-desktop-advisories.py           # requires PyYAML
```

- **New license:** add its SPDX identifier to `[licenses].allow` only if it is permissive or public-domain-equivalent, with a comment naming the crate(s) and, for weak copyleft such as MPL-2.0, a justification. Copyleft that would bind Orbit's own sources is never added; replace the dependency.
- **Advisory exception:** only when no patched release exists (otherwise bump the dependency), as an individually reviewed `[advisories].ignore` object with `id` (`RUSTSEC-YYYY-NNNN`) and a `reason` documenting Orbit's exposure, exact dependency chain, upstream constraint, re-review owner and a `Re-review YYYY-MM-DD` date, about six months out. Re-review by that date and remove the entry when a fix lands. Ignores apply by ID across both graphs, not by package version or workspace: review any new occurrence outside the documented chain. For `advisory-not-detected`, confirm the ID no longer matches **either** workspace before retiring the entry, then rerun both commands. Desktop-only exceptions naturally produce that warning on the root graph. A reintroduced advisory is reviewed afresh; do not allow all unmaintained crates or future unknown IDs.
- **Read-only advisory database:** set `CARGO_DENY_DB_PATH` (or `ORBIT_CARGO_DENY_DB_PATH`) to a writable directory; `scripts/cargo-deny.sh` points `[advisories].db-path` there. The invoking runner cleans it up.
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

## Vendored dashboard JavaScript

DOMPurify and marked are vendored under [`crates/orbit-web/assets/dashboard/vendor/`](../crates/orbit-web/assets/dashboard/vendor/); [`VENDOR.md`](../crates/orbit-web/assets/dashboard/vendor/VENDOR.md) has the refresh procedure. Refresh with `./scripts/refresh-dashboard-vendor.sh` after changing the pins in `package.json`; never edit the blobs by hand. `make ci-fast` runs [`check-dashboard-vendor.py`](../scripts/check-dashboard-vendor.py), which fails on a digest mismatch or drift between the manifest, `package.json` and `package-lock.json`. Dependabot and GitHub security alerts watch these packages, and the `dependabot-alert-sweep` job files remediation tasks; `cargo-deny` does not see them.

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
