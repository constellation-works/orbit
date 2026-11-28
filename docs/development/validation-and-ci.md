## Validation and CI

For a diff, these local checks are required before handoff: `make ci-guards` (static repository guards, no build or network) and `cargo fmt --all -- --check`, plus `rustfmt --check --edition <crate edition>` on each changed test `.rs` file. For changed workspace crates, also run `make ci-lint-affected` (it exits without compiling when none changed); for `desktop/`, run strict Cargo Clippy. Run these even when pipeline final validation (`task.final_validation_commands`) names the same guard, and fix what they report: pipeline final validation does not replace them. There is no required local full-suite run. The required PR checks (Check / Clippy / Test and Test shard 1/3, 2/3 and 3/3) gate formatting, guardrails, lint, rustdoc, goldens and the whole test suite, so the targets below are focused tools, not blanket pre-handoff gates. Run the conditional ones the change needs (`make goldens`, `make docs-index`, `make check-ci-macos`) and the tests of the changed behavior:

```bash
make ci-guards         # static repository guards; no build or network
make ci-fast           # MSRV compile check, formatting, script guardrails; no Rust tests
make ci-test-affected  # complete test targets of changed crates and their workspace dependents
make ci-lint           # clippy for production and all targets, then rustdoc
make ci-lint-affected  # production, all-targets and rustdoc wrapper checks for changed crates
make goldens           # CLI/MCP, environment reference, CI log, sandbox and SQLite ledger schema goldens
```

Run `make goldens` when the change touches a golden surface (CLI help, the CLI environment reference, MCP text, sandbox profiles, ledger schemas), and the tests of the behavior you changed. To regenerate, run `make goldens UPDATE=1`. For environment documentation alone, run `ORBIT_UPDATE_HELP_GOLDENS=1 cargo test -p orbit-cli --test output help_goldens::env_reference::config_environment_reference_matches_registry -- --exact` and review the generated change in [configuration reference](../config/environment.md#environment-variables). A red required check on the PR is repaired through the PR (auto-fix or a follow-up commit). `workflow.required_validation_commands` in the workspace config sets what the pipeline runs.

When an edit changes any generated row in `docs/INDEX.md` or `docs/design/FEATURES.md` (for example, indexed Markdown frontmatter, its first heading, or summary), run `make docs-index`, review the generated files, and run `scripts/generate-doc-indexes.sh --check`. Include any resulting generated change with the source edits, even when `make ci-fast` is pipeline-owned final validation.

When renaming or consolidating a test selected by macOS CI, update the filter
in `.github/workflows/ci-macos.yml` and run `make check-ci-macos`. This lists
actual compiled tests, including the workspace feature set, so a filter cannot
silently become a zero-test pass.

The macOS and CodeQL workflows do not run on pull requests: GitHub's
hosted-runner concurrency was the merge bottleneck, and none of their checks is
required. The macOS workflow runs on `agent-main` and `main` pushes; CodeQL runs
on `main` pushes and daily on `agent-main`. A failure there becomes a repair
task, as a CodeQL alert does. Website checks run
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
always does. A `workflow_dispatch` of `ci-macos.yml` keeps a group per run, so later pushes do not replace it. `codeql.yml` uses one group per ref for push, schedule and dispatch, still with cancellation off. A docs-only push
whose parent already has a successful completed run of the same workflow skips
that workflow's heavy jobs: both macOS build jobs, or all three CodeQL analysis
jobs. The parent's result still covers that unchanged code. A push that changes
any non-docs path, or a docs-only push whose parent has no such proof, starts
those jobs.
`workflow_dispatch` and CodeQL's daily schedule always run every job. A
docs-only `agent-main` tip therefore has no macOS result from the push; the
dispatch route under [Getting a completed result for a chosen agent-main SHA](hosted-ci.md#getting-a-completed-result-for-a-chosen-agent-main-sha)
is how that SHA gets one. A dispatch run is not docs-scoped. Required-check
settings are managed separately in the repository's provider rules.

The task and MCP CLI integration binaries use named contract tables, with
four and eight nextest slots respectively. Each row keeps its assertions and disposable state; rows that
share a workspace address independent task IDs. Exact-child fixtures isolate
the complete table once, rather than re-executing removed test entry points.
The [MCP and task test budget report](../agents/records/reviews/cli-mcp-task-test-budget.md)
records the coverage mapping and candidate-host before/after measurements.

Tasks tagged `no-diff-expected` run no workspace gates or required validation
commands and attach no `no-diff.json` or workspace validation logs, in owner or
claimed mode. Run only commands the task's own brief requires; a QA sweep may
build and run the product, and a review may reproduce a finding when its brief
asks for it. Keep the brief's evidence artifacts and execution summary.

- `make ci-guards` (or `scripts/ci-guardrails.sh --static`) runs the offline
  subset of the fast repository guards before candidate handoff.
  It includes unit-test admissions, QA surface inventory, public artifact IDs,
  test shell waits, test child runners, dependency direction, orphan modules,
  vendored standards integrity (`docs/standards/check.sh`), doc links and
  indexes, and the skill mirror check. The QA inventory uses `--check --static`
  to omit its fixture self-tests and explicitly request offline metadata. Cargo
  is used
  only for metadata, with `CARGO_NET_OFFLINE=true`; no compilation, formatting,
  Rust tests or network requests run. It needs Python 3.11+, PyYAML, ripgrep and
  an installed Cargo toolchain. The Claude Code Markdown link cases also run
  offline under Node 22.18 or later. The networked workflow action-pin check, Node UI
  behavior tests, scratch plugin installation, and all fixture self-tests remain
  in `make ci-fast` and full CI. The command uses the same bounded pool, names
  failing guards, returns a nonzero status on failure, and prints per-guard and
  total wall times. This early check does not replace any required delivery gate.
  For a diff, run it and fix its findings even when `make ci-guards` or
  `make ci-fast` is reserved for pipeline final validation. Run
  `cargo fmt --all -- --check` in Rust workspaces,
  including test targets. Include changed test files when applying rustfmt fixes,
  and check them explicitly with `rustfmt --check --edition <edition> <path> ...`
  using their crate's edition; do not limit formatting to production files.
  For Rust changes, run `make ci-lint-affected` as the pre-handoff lint check.
  It runs both `ci-lint` Clippy passes and warnings-denied rustdoc
  sequentially under `scripts/build-budget.py` for each workspace crate changed
  against `CI_TEST_BASE` (falling back to the merge base with `origin/agent-main`,
  or `agent-main`), and exits `0` without compiling when no workspace crate changed:

  ```bash
  make ci-lint-affected
  # equivalent per-crate commands:
  scripts/build-budget.py -- scripts/with-test-build-lint.sh --check cargo check -p <crate> --lib --bins --message-format=json
  scripts/build-budget.py -- scripts/with-test-build-lint.sh --check cargo check -p <crate> --all-targets --message-format=json
  RUSTDOCFLAGS="-D warnings" scripts/build-budget.py -- scripts/with-test-build-lint.sh --check cargo doc --no-deps -p <crate> --message-format=json
  ```

  Replace `<crate>` with the package name. For a crate with no library
  target (`orbit-cli`), `make ci-lint-affected` uses `--bins` in place of
  `--lib --bins` in the production Clippy command. Keep the whole
  final-validation commands with the pipeline. These scoped checks cover
  production channel restrictions, test targets, and private/broken
  documentation links.
  All three use the lint environment from `make ci-lint`. Crate-root
  `deny(clippy::disallowed_methods)` attributes override its command-line
  `-A` in production. The doc build reuses Clippy's check. The wrapper
  fails on compiler warnings except the recorded
  [STD-12 ordering debt](../design/architecture-restructure/4_decisions.md#roll-out-std-12-declaration-and-literal-order).

  On a macOS agent lane, the warnings-denied rustdoc command can fail with `EPERM` when its output path matches the agent sandbox's case-insensitive secret-file deny `**/*.env.*`. The example is orbit-common's `Env` page (`struct.Env.html`, in the `env` module): rustdoc tries to create it, the deny refuses the write, and the command exits 101. The deny is correct and stays, so do not add a sandbox policy or test exception for it. Record that run as `not_run`, quoting the `EPERM` denial and the exit status, and never describe it as passed. Its evidence is the hosted Linux CI documentation step, the `Check / Clippy / Test` job's format, Clippy and documentation step (`scripts/ci-guardrails.sh --checks`), which runs the same warnings-denied `cargo doc` over the whole workspace. Name that step in the execution summary.
- `make ci-fast` runs independent repository script checks through a background pool. Its width is the build budget's Cargo job count (`scripts/build-budget.py --cargo-jobs`: `ORBIT_CARGO_JOBS`, then `CARGO_BUILD_JOBS`, then the `cargo-jobs` file, default 4), so guard fan-out never exceeds the same budget Cargo receives. The pool admits a new guard whenever any running member finishes, so a slow early guard does not keep the slots of guards that have already exited; at most that many guards run at once. Each script's output is captured separately and printed in launch order, per-script wall times are printed, and every failing script is named before the first failure's exit code is returned. Fixture self-tests run only when their inputs change: `scripts/`, `.github/`, `Makefile`, `.cargo/`, the root Cargo manifest/lockfile, `.config/nextest.toml`, `crates/orbit-engine/tests/engine/pr_landing/infrastructure/fixtures/legacy-codeql.yml`, `install.sh`, `RELEASING.md`, `docs/agents/records/qa/path-leases-listener-sudo.md`, `docs/standards/`, `AGENTS.md`, `CLAUDE.md`, `plugin/`, `npm/`, `.agents/`, `.claude-plugin/`, `server.json` (the QA npm fixture's input) or `docs/development/examples/release-readiness/scripts/release-readiness.yaml` (the adapter self-test's input). Self-tests that assert wall-clock time or timeouts, measure build-budget admission, or drive a real Cargo target run one at a time, at normal priority, in a separate exclusive lane: `test-build-budget.sh`, `test-ci-await-jobs.py`, `test-ci-fast-guards.py`, `test-rust-source-order.py`, `test-ci-workspace-cache.py`, `test-installer-security.sh`, `test-ci-test-affected.py`, `test-ci-lint-affected.py`, `test-codeql-rust-local.py`, `test-compiler-cache.sh` and `test-compiler-cache-namespaces.sh`. The lane starts after formatting, beside the pool. Each pool guard and, in full mode, the workflow test build starts in Linux's idle scheduling class, with a direct `nice -n 19` launch where idle scheduling is unavailable. Priority changes apply to the launching command itself, so macOS workers need no permission to renice a parent process. Nice scheduling lowers competition for CPU; Linux's idle class uses only CPU time the exclusive suites leave idle. `test-ci-download-verified.py` spends nearly all of its wall time in fixed slow-child and hang delays, so it starts beside the pool, outside its window, and is reaped after the pool drains. Its helper deadlines run on an injected clock, and a fixture step fails only after 120 s without output. Pooled workflow and fixture self-tests that used a short subprocess wall-clock cap share one hang ceiling in `scripts/self_test_hang.py` (`HANG_CEILING_SECONDS`, 120 s, the fixture-step deadline). A step that exits is not failed for wall time. A step still running at the ceiling is killed, and the failure names the step or command. The removed 10 s and 20 s caps failed correct release and PR-state steps when idle scheduling stretched wall time. Timeouts that are the subject of a test, including the exclusive-lane suites and the QA sweep's supervision cases, stay as they are. Every other guard and self-test shares the pool. In full mode the pool and the exclusive lane overlap the workflow test build. Both join before the workspace tests start, the pool first, so no guard shares the host with their elapsed-time and timeout assertions and a pool failure stays the job's exit status. Nothing in the pool depends on that build or takes the Cargo build lock. Selection uses the same `CI_TEST_BASE` override and merge-base fallback as affected tests and includes committed, staged, unstaged, deleted and untracked paths. If Git cannot resolve the base or diff, every self-test runs. The QA sweep inventory check still runs for crates-only changes because it reads live CLI/MCP/web surfaces. Full `make ci` always runs all self-tests. The MSRV compile check is timed on its own, separate from those scripts: make prints `ci-fast: MSRV check <seconds> s` for `cargo +<MSRV> check --workspace --locked`, and each guardrail keeps its own `ci-guardrails: <name> <seconds> s` line.
- The pooled hang guard and verified-download fixture reserve each command's process group with a separate owned child blocked on an stdin pipe. Polling or communicating with an exited command may reap it; the anchor stays unreaped through the final group signal, then both owned children are reaped and their pipes closed. Cleanup also runs after successful commands and startup failures. This uses Python 3.11's `process_group`, without `preexec_fn`, so threaded fixtures can use it. Descendants that intentionally leave the group need their own identity-bound guard. The public runner regressions in `test-pr-state-workflows.py` and `test-ci-download-verified.py` share an intercepted process model in `scripts/self_test_hang_regression.py`: reaping immediately permits a replacement group, and every signal must still have an owned reservation. They cover exited leaders and escaped pipe holders without PID churn or synthetic load; the real descendant-cleanup fixtures remain enabled.
- Rust source style (STD-12): `scripts/check-source-style-diff.py` runs in `ci-guardrails.sh` and fails on a new R1, R12, R13, R16, R19, R21, R22 or R26 finding against the merge base (`CI_TEST_BASE`, else `origin/agent-main`). Existing findings are recorded debt in [the architecture decisions](../design/architecture-restructure/4_decisions.md#shared-source-style-checker-gates-new-findings). Check a change locally with `CI_TEST_BASE=<base commit> python3 scripts/check-source-style-diff.py`.
- `make ci-fast` passing says nothing about the Rust tests. Focused test filters help you investigate but do not replace `make ci-test-affected`: an existing test can encode behavior your change breaks even when your new tests pass.
- Run Cargo-based gates (`ci-fast`, `ci-test-affected`, `goldens`, `ci-lint`, `cargo test` / `nextest`) one at a time when they share a target directory, or give each its own `CARGO_TARGET_DIR`. Concurrent gates in one target can rebuild `target/debug/orbit` under running generation-bound fixtures and fail them.
- For a PR changing only `scripts/removed-dashboard-labels-allowlist.json` among self-test inputs, the core guardrails run its live guard and the owning `test-ci-fast-guards.py` fixture suite, rather than every fixture suite. Other changed self-test inputs still select all suites; QA and full push selection are unchanged.
- Guardrails need Python 3.11+ for `tomllib`; stock macOS `/usr/bin/python3` is 3.9. [require-python.sh](../../scripts/require-python.sh) runs first in `make ci-fast`, `make ci-lint` and `ci-guardrails.sh` and stops with the version and interpreter it found. The selected fixture self-tests also require PyYAML (`import yaml`), including the verified-download fixture. After resolving self-test selection, `ci-guardrails.sh` checks that import with the same `python3` before Cargo or any repository guard runs. Full CI always requires it; fast mode requires it whenever self-tests are selected, including unresolved selection. Missing prerequisites fail; nothing installs automatically. Put a supported `python3` first on `PATH`, then set up its environment as below. The standalone local workflow YAML linter may still warn and skip without PyYAML; that optional boundary never substitutes for the mandatory selected fixtures. The compiler-cache namespace regression (`scripts/test-compiler-cache-namespaces.sh`) also needs Python 3.11+ for `subprocess.Popen(process_group=...)`; when the cache and user namespaces are available, it runs the same preflight before starting any participant.
- Linux CI installs nextest, cargo-deny, uv and ripgrep through [ci-download-verified.sh](../../scripts/ci-download-verified.sh): `scripts/ci-download-verified.sh HTTPS_URL SHA256 NEW_FILE`. Each invocation retries transient responses until one monotonic deadline is spent; there is no fixed attempt count. The default and maximum budget is 60 seconds, and `--budget-seconds` may lower it. The deadline covers every GET attempt, the backoff between attempts (one second more after each failed attempt, capped at five seconds) and digest verification. A retry starts only when the remaining budget exceeds its backoff; if the budget runs out after transient responses, the last line reports `class=transient`. Only curl DNS, connection and timeout exits (6, 7, 28), and HTTP 429/500/502/503/504 with curl exit 22, can retry. TLS, authentication, partial transfers, unknown outcomes and digest mismatches fail closed. HTTPS-only redirects and normal certificate checks remain enabled; curl configuration files and internal retries are disabled. Each attempt uses fresh bytes, and only a matching digest publishes the new destination; existing destinations, including those created during publication, are refused without replacement. A warm-cache hit copies the cache entry into private destination-local scratch, verifies that copy and publishes it without replacement, so the destination holds the verified bytes whatever happens to the cache path meanwhile. Installer traps remove scratch archives on success or failure, and archive/version checks never retry. Attempt diagnostics and total elapsed time remain in the Check job log: all setup attempts, backoff, failures and post actions count toward the complete job's 400-second budget. Offline fixtures inject file and symlink publication races on warm and cold paths, exercise the cross-filesystem cache fallback, and check QA workspace and artifact readback against the real built-in validators. Public helper and workflow installer fixtures run in the existing guardrails via [test-ci-download-verified.py](../../scripts/test-ci-download-verified.py).
  The short-deadline fixtures publish completed receipts by atomic replacement and signal readiness through an inherited pipe. That self-test points `ORBIT_CI_DOWNLOAD_FAKE_CLOCK` at a file holding a decimal second count. The helper sleeps by advancing that clock exactly, and a stalled transport waits until the helper arms the attempt deadline, then crosses it so the same watchdog kills and reaps the child. Budgets stay 0.3s and 1.4s; host scheduling cannot move the outcome. The fixtures assert exact receipt counts, remaining second-attempt budget, unsuccessful exit, destination absence, scratch cleanup and child reaping. Cancellation barriers reproduce interrupted in-place receipts and transport startup without synthetic host load. Backoff in the transient-table, checksum and budget-exhaustion cases also runs on that virtual clock: the transient-table and checksum cases record one second, and the exhaustion case records 1, 2, 3 and 4 seconds before its next five-second delay exceeds the remaining budget. Each case records its delays instead of sleeping them. The real-TLS boundary case and the remaining download and installer cases leave the variable unset and use `time.monotonic` with the helper's real `subprocess` timeout, so the real-TLS recovery case keeps its one-second backoff as a real sleep. The self-test does not put a short wall-clock cap on those commands: a child that exits, or keeps writing, is not failed for host speed, and a child that produces no progress is killed at a hang ceiling no correct local run approaches, with a diagnostic that names the hung step.
- Skill docs are canonical under `crates/orbit-core/assets/skills/`. Edit them there and run `scripts/sync-plugin-skills.sh` to regenerate `plugin/skills/`; `make ci-fast` fails on a stale mirror.
- [check-doc-links.py](../../scripts/check-doc-links.py) checks tracked and non-ignored untracked Markdown, and fails on a missing relative link target, heading anchor or backticked repository path that is not a tracked or non-ignored untracked file present in the worktree. Website links resolve through Starlight routes and code blocks are ignored; its exclusions (historical decisions, RCAs, templates, changelog, fixture prose) are listed in the script. Run `python3 scripts/check-doc-links.py`; its fixtures are in `python3 scripts/test-ci-fast-guards.py`.
- [check-removed-dashboard-labels.py](../../scripts/check-removed-dashboard-labels.py) compares the delivery diff from the merge-base of HEAD and that guard's base, so labels the base branch added after a pull request's branch point never count as removed by it. A checkout that holds no merge-base with `CI_TEST_BASE` (a shallow clone without the common ancestor) fails with an insufficient-history report instead of comparing from the base tip. It fails when a dashboard label the delivery diff removes from `crates/orbit-web/assets/dashboard/` is still quoted in the user-interface design docs, `docs/agents/dashboard-map.md`, the website or the shipped skills. It reads removed, added and remaining labels by one rule, so a label the dashboard still renders elsewhere is not a removal, and one left only in a comment, a class list or a property name is. Legitimate keeps go in `scripts/removed-dashboard-labels-allowlist.json`. Its fixtures are in `python3 scripts/test-ci-fast-guards.py RemovedDashboardLabelTests`.
- [check-removed-code-identifiers.py](../../scripts/check-removed-code-identifiers.py) compares the delivery diff from the merge-base of HEAD and that guard's base, so identifiers the base branch added after a pull request's branch point never count as removed by it. A checkout that holds no merge-base with `CI_TEST_BASE` (a shallow clone without the common ancestor) fails with an insufficient-history report instead of comparing from the base tip. It takes the Rust `const`, `static`, `fn`, `struct`, `enum`, `trait` and `type` names, the `pub use` re-export paths and the CSS class selectors (`.name`) that the diff deletes, drops any the diff re-adds or the tree still defines, and fails when a live document names one as an inline backticked token. A Rust item or `pub use` inside a comment or quoted Rust literal is not a definition or re-export, whether the diff adds or deletes it, and items and re-exports are judged from the whole file rather than the changed lines. A class counts wherever a selector names it, so removing the last `.retired` from `.active.retired` is a removal, and class-shaped text in a CSS comment, a quoted string, a `url(...)` or a declaration value is not a class. Live documents are `docs/` (except `docs/design/*/4_decisions.md`, `docs/agents/records/rca/` and `docs/standards/`), `website/src/content`, the shipped skills, `ARCHITECTURE.md` and the per-crate `AGENTS.md` and `CLAUDE.md` files. Legitimate keeps go in `scripts/removed-code-identifiers-allowlist.json` with a reason. Its fixtures are in `python3 scripts/test-ci-fast-guards.py RemovedCodeIdentifierTests`.
- Repository source guards reuse work only within one invocation. The orphan-module guard enumerates the same production and sibling-test trees and masks each source once for lexical module declarations. Its scan includes hidden, ignored and symlinked source files, matching the physical tree checked for orphans so ignore rules cannot hide module owners. Symlinked `lib.rs` and `main.rs` entrypoints are discovered directly, including roots that share a physical source but own different logical directories. It resolves ordinary declarations and direct `#[path = "..."]` literals, accepting whitespace and Rust comments between attribute tokens, including between `#` and `[` (for example, `# /* comment */ [path = "other.rs"]`), as well as trailing line and nested block comments, ordinary Rust escapes (quote, backslash, ASCII `\xNN`, Unicode `\u{...}`, control characters and line continuations), and raw `r"..."` and hash-delimited `r#"..."#` strings (with matching hash counts), using [Rust's enclosing module directory rules](https://doc.rust-lang.org/reference/items/modules.html#the-path-attribute): explicit paths outside inline modules start at the source directory; ordinary declarations and inline modules add their directory components and, for ordinarily loaded file modules, the source stem. A file loaded through an explicit path instead owns its containing directory for ordinary children and inline descendants; the guard follows declarations to carry that ownership into each child. Symlinked source files retain their logical owner directory and filename for child resolution, while canonical file identity is used for registration and source caching. Indexing distinguishes each source-directory/child-directory context for a physical owner; canonical source directories make growing directory-symlink spellings converge on already indexed contexts, without rejecting finite nested reuse of the same physical file. Directory discovery does not follow symlinks; declared file destinations may resolve through them. An inline module's own path attribute replaces its default directory. An external module's explicit path replaces its ordinary destination: `#[path="other.rs"] mod lost;` registers only `other.rs`, leaving a sibling `lost.rs` orphaned. The scan masks comments and literals to find declarations, then masks only comments within each attribute to preserve its actual path literal. It tracks nested braces and compares actual destination files, including `.` and `..` components; an unrelated file with the same basename cannot register a production module or sibling test. It accepts declarations regardless of cfg, rather than evaluating build configurations, and does not expand macros. Traversal starts only at conventional Cargo entrypoints (`src/lib.rs`, `src/main.rs`, and binaries under `src/bin/`); directory `mod.rs` files, including sibling `tests/mod.rs`, must themselves be reachable. A disconnected owner cannot register its children. The disposable command fixtures in `python3 scripts/test-ci-fast-guards.py OrphanModuleTests` compile miniature crates with `rustc --test` and enumerate their tests to verify ordinary, raw-string, redirected, inline and nested path registration while keeping same-name and same-basename orphan files rejected. Escaped/spaced attribute (including separated hash/bracket openings), trailing-comment attribute, symlink-owner and symlinked crate-root regressions also execute the registered tests, retain genuine orphan controls, and check that a growing symlink cycle terminates. The CLI import guard reads each source once with the same comment/string masking and per-crate diagnostics. The unused-dependency guard collects reference names once per file and combines them only within the original normal, dev or build scope. Each later invocation reads the current files again; no persisted source cache, reduced guard selection or wider concurrency is involved.
- The guard fixtures retain full runner success/failure checks for shell-wait routing, then exercise the remaining syntax and exception rows directly through the checker executable. Action-pin fixtures record interleaved curl attempts and exact sleep requests instead of waiting for simulated network retries; the production guard still performs its normal backoff.
- [check-unused-dependencies.py](../../scripts/check-unused-dependencies.py) fails on a manifest dependency its sources never use, a normal dependency only test targets use, or a `[workspace.dependencies]` entry no member declares. Keep a feature-only dependency in its `FEATURE_ONLY` allowlist with a reason. Self-test: `python3 scripts/test-check-unused-dependencies.py`.
- [check-linux-gated-test-items.py](../../scripts/check-linux-gated-test-items.py) runs in the static guard list beside `check-windows-portability.py` (`make ci-guards`) and fails when an import, test helper `fn`/`const`/`static` or fixture `mod` in a Rust test file or `#[cfg(test)]` module is left ungated while every use site is compiled out on macOS (typically `#[cfg(target_os = "linux")]`), which turns native macOS Clippy red under `-D warnings` with `unused import` or `never used`. It reuses the Windows guard's cfg evaluation and module edges for a macOS target, scans `crates/` and `desktop/`, and names the file, line and item of each failure. It is lexical: it matches use sites by name within the item's block and the modules declared or `include!`d in it, skips `pub` items, UpperCamel imports (a trait import is also used implicitly through method calls) and items under `allow(dead_code)`/`allow(unused_imports)` (a `cfg_attr` allowance counts only where its condition may hold on macOS), and does not reach dead code behind macros. Self-test: `python3 scripts/test-linux-gated-test-items.py`.
- [check-openpty-portability.py](../../scripts/check-openpty-portability.py) runs in `make ci-guards` and rejects recognizable immutable fourth or fifth `libc::openpty` arguments in Rust code reachable on macOS. It shares the Windows guard tokenizer, cfg evaluation and module reachability, ignores comments and literals, and reports the argument and file:line. This lexical check does not resolve aliased calls, function pointers, macro-generated calls, pointer locals, turbofish `null::<T>()` or other unknown pointer expressions; native compilation remains necessary. Self-test: `python3 scripts/test-openpty-portability.py`, also selected by the existing guard self-test route when script inputs change.
- [check-windows-portability.py](../../scripts/check-windows-portability.py) runs in `make ci-fast` and rejects recognizable Unix std APIs reachable on Windows, including test helpers. It checks Rust files under `crates/`, follows external and inline module declarations (including `#[path]`). An inline module's `#[path]` replaces that module's directory for its children, the same way an external `#[path]` replaces the file, and both ordinary and raw string literals (`r"..."`, `r#"..."#`) count as path attributes. It accepts enclosing item, module or file gates that exclude Windows. Ordinary file modules resolve children under their stem directory; `mod.rs`, Cargo roots and files loaded through `#[path]` resolve children beside the file. The shared scanner tracks these loading contexts separately, including inline descendants, so excluding one context does not activate its children through another, while independent Cargo targets remain checked. It evaluates `unix`, `windows`, `target_os`, `target_family`, `all`/`any`/`not`, and conditional `cfg_attr`; unknown features and test flags remain potentially enabled. `cfg!(unix)` does not remove code from compilation. The static approach runs on every worker without installing a Windows target or linker, and needs no path allowlist. It scans Unix std paths/grouped imports (`std::os::{unix,linux,macos,fd}`), distinctive extension calls such as `from_mode`, `pre_exec` and `ino`, qualified permission/metadata trait calls and `permissions().mode()`. It ignores comments and literals. It does not expand macros or infer receiver types, so ambiguous application methods such as `mode`, `set_mode` and `blocks` are checked only in recognizable std forms. No CI job compiles for Windows (native Windows is unsupported), so this lexical check is the only Windows guard. Self-test: `python3 scripts/test-windows-portability.py`.

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

[check-codeql-production-paths.py](../../scripts/check-codeql-production-paths.py) in `make ci-fast` rejects Rust source
names matched by `.github/codeql/codeql-config.yml`'s `paths-ignore`, reporting
the file and pattern. It checks tracked and non-ignored untracked sources,
excluding `tests/`, `examples/` and `benches/`. Other matches need a direct
`#[cfg(test)]` or `#[cfg(feature = "test-support")]` on every declaring module;
the latter is the existing feature for helpers enabled from dev-dependencies.
This naming guard fails closed for indirect or composite cfg gates and
unrecognized module layouts: use a filename that CodeQL includes in those
cases. Its behavioral fixtures run in [test-codeql-production-paths.py](../../scripts/test-codeql-production-paths.py).

### Affected-test selection

Delivery remote-source and provider-attribution fixtures freeze monotonic source-budget time through
`ORBIT_TEST_SOURCE_ELAPSED_MS`, inherited by their isolated
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
Their [scenario mapping](../../crates/orbit-engine/tests/engine/SCENARIOS.md) lists
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
Three environment variables reach that child instead, and each is honoured
only in a debug build with the `test-support` feature or in a unit-test
binary; any other build, including a plain `cargo build`, ignores all three
and uses the production timings: `ORBIT_TEST_HOST_FORWARD_ATTACH_TIMEOUT_MS`
(host-forward attach budget), `ORBIT_TEST_MCP_LIFECYCLE_INTERVAL_MS` (stdio
lifecycle tick) and `ORBIT_TEST_PIPELINE_WAIT_VIRTUAL=1` (a pipeline wait adds
each poll to its elapsed time instead of sleeping, and does not wake on a
reaped worker). The quiet-deadline child fixture injects its bound through
`run_child_test_within`. The real child and the real reap stay in place.

`ORBIT_TEST_MERGEABILITY_WAIT=virtual` has the same extra condition for landing's
mergeability poll, which the external-merge fixtures run in-process rather than
in a child; they set it with `test_env::scoped`.

`make ci-test-affected` (`scripts/ci-test-affected.py`) reads Cargo metadata without compiling. It selects every crate with changed paths plus all transitive reverse workspace dependents (dev, build, renamed, optional and target-specific edges), then runs their library, binary and integration test targets with nextest (Cargo if nextest is missing), under the shared [build-budget admission](../runbooks/build-budget.md). Doctests are disabled workspace-wide (`[lib] doctest = false`); `scripts/check-doctest-fences.sh` fails a runnable fence instead of running it. A change under `crates/orbit-core/` selects `orbit-core`, `orbit-cmd`, `orbit-web` and `orbit-cli`.

- **Diff:** committed, staged, unstaged and non-ignored untracked paths; a move selects both crates.
- **Base:** the merge base of `HEAD` with `origin/agent-main` (local `agent-main` if the remote ref is absent). Override with `CI_TEST_BASE=<rev>`; a reviewer with a pinned delivery base must run `CI_TEST_BASE=<base.commit> make ci-test-affected`. An unavailable base fails the gate.
- **Everything or nothing:** a change to `Cargo.toml`, `Cargo.lock`, `.cargo/`, `.config/` or the toolchain files, or a removed crate, selects every crate. A docs-only diff selects nothing and passes without compiling.
- **Cross-crate file reads:** Cargo cannot see a test reading files outside its crate. Declare the path prefix and reading crates in `FILE_READERS` in `scripts/ci-test-affected.py`; nothing detects them for you, and a declared crate missing from metadata fails the gate.
- **macOS workflow names:** when the diff adds, deletes, renames or edits a path under a `tests/` directory of a crate (`crates/**/tests/**`, `crates/**/src/**/tests/**`) or edits `.github/workflows/ci-macos.yml`, the gate first runs `scripts/check-ci-macos.sh` under build admission. It lists the tests the macOS workflow names and runs none, and exits before any test runs if one of its paths or filters no longer matches. The failure names the stale entry in `.github/workflows/ci-macos.yml`, which is the file to fix. A diff touching neither skips it. A replayed `ORBIT_VALIDATION_SELECTION` carries no diff, so it skips the check too: the candidate run that named the selection already ran it.
- **Inspect:** `python3 scripts/ci-test-affected.py --list [--base <commit>]`.
- **Hosted CI:** `--pull-request-scope FILE` and `--test-build-packages CRATES` are the pull-request CI's entry points ([PR test selection](hosted-ci.md#pr-test-selection)). They read Cargo metadata (and `cargo tree`) but no diff and run no test.
- **Test environment:** nextest runs with `--success-output immediate`. The gate archives its complete merged stdout/stderr in a unique `ci-test-affected-nextest-*.log` under `.orbit/tmp/` and streams progress, summaries, diagnostics and failing-test blocks. Passing-test blocks stream only host-evidence notices (`DEFERRED:`, `SKIP:`, `skipping:` or `skipping `, and sandbox-unavailable markers), so a full selection stays within the executor's default capture limit while the host judge still sees every refusal. The gate reads the runner in 64 KiB chunks, writes each chunk to the log before decoding it, and judges and relays a longer line in pieces of at most that size, so one endless line cannot grow gate memory. The no-nextest `cargo test` fallback is unchanged and does not expose passing-test captured output. A temporary directory inside the checkout is added to `GIT_CEILING_DIRECTORIES`. `ORBIT_WORKER_CONTEXT_REQUIRED` is dropped ([why](testing.md#test-process-environment)); the rest of the run envelope passes through.
- **Observed streams:** The Cargo fallback command drains stdout and stderr independently and captures whole lines. A relay I/O failure stops the command's process group before waiting for the readers, then propagates the original error. These commands own a separate session; the gate forwards graceful SIGTERM shutdown through cleanup, with SIGKILL escalation after a short grace to stop descendants holding the pipes. Killing the gate directly with SIGKILL cannot run this cleanup.

To check whether a failure is pre-existing, replay the same command on the unmodified base: extract it under `.orbit/tmp/` (for example `git archive <base> | tar -x -C .orbit/tmp/base`), refresh its timestamps, give it its own `CARGO_TARGET_DIR` (any path works; the `orbit update` fixtures copy the tested binary to their own `target/debug/orbit`), and set `GIT_CEILING_DIRECTORIES` to `.orbit/tmp` so its fixtures cannot find the surrounding checkout. Replay only the failing tests, not the whole selection: hand the candidate's summary `selection` back with `failing_tests` added (below), or run nextest with `-E 'binary_id(=<binary-id>) & test(=<test>)'` for each failure it printed. Replay soon after the candidate run, since fixture deadlines depend on host load.

### Required validation commands

This repository currently has no blanket local required-validation commands. The required PR checks gate merges on `agent-main`: `Check / Clippy / Test` and `Test shard 1/3`, `Test shard 2/3`, and `Test shard 3/3`. Use focused local checks for the changed behavior as described in [AGENTS.md](../../AGENTS.md#gates); the PR checks report their results on GitHub.

```toml
[workflow]
required_validation_commands = []
unlocked_path_validation_commands = ["make ci-fast"]

[workflow.generated_paths]
"docs/INDEX.md" = "make docs-index"
"docs/design/FEATURES.md" = "make docs-index"

[workflow.validation_env]
build_output_env = ["CARGO_TARGET_DIR"]
```

A repair that changed only Markdown since the run's last full pass reruns only `make ci-fast` ([revalidating an unlocked-path repair](../config/workflow-validation.md#revalidating-an-unlocked-path-repair)), and a conflict on `docs/INDEX.md` or `docs/design/FEATURES.md` is resolved by regenerating it ([generated paths](../config/workflow.md#generated-paths-are-regenerated-not-merged)).

The reviewable [workspace configuration example](../../config/orbit-workspace.toml) shows
the current repository policy. Merge applicable settings into the owner configuration
during operator rollout. Cargo output selection is this repository's policy; the generic
runtime has no build-tool default ([build output contract](../config/workflow-validation.md#workspace-build-output)).

When an owner configures required validation commands, candidate validation runs them in order on the final candidate after base synchronization, and before-PR review requires a passing record of each. Tasks tagged `no-diff-expected` do not run this list. A managed implementer finds the list in its envelope's `task.final_validation_commands` and records each as `not_run` for that later validation ([required validation](../config/workflow-validation.md#required-validation)). For a diff it still runs, before handoff, each pre-handoff check the workspace guide requires (such as `make ci-guards`) even when the list names it, plus focused checks and any gate the list omits. Its task execution summary names each phase: the pre-handoff phase records each command's actual outcome, and the pipeline final-validation phase records each listed command as `not_run` ([task execution, Step 5](../../crates/orbit-core/assets/skills/orbit/references/task-execution-steps.md#step-5--summarize-and-hand-off)). A pre-handoff run of a listed command does not count as pipeline final validation; the pipeline's attached validation logs remain the record of committed-candidate validation ([required validation receipts](../config/workflow-validation.md#required-validation)).

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
with `orbit.task.artifact` action `get` using the owning task and artifact path. Recovery
uses those same artifacts before choosing a repair.

Parent gates echo only the leaf run, task, step and code. Incident codes use
recorded error codes, falling back to the typed terminal run state; command
stdout and diagnostic prose never supply a code. A red-base message retains
its exact persisted hold for admission. If that metadata itself exceeds the
message budget, validation refuses it rather than truncating a command or
selection needed for a safe replay; the artifacts retain the evidence.

### Validation summary and base reruns

When a required command fails, Orbit reruns it on the base to tell a red base from a candidate failure ([decision rules](../config/workflow-validation.md#workflowvalidation_env--the-toolchain-required-validation-runs-with)). A command that selects tests from the diff would test nothing on the base, so the runner passes two variables, both implemented by `scripts/ci-test-affected.py`:

- `ORBIT_VALIDATION_SUMMARY`: a file outside the checkout where the script writes `{"schema_version": 1, "selection": {"packages", "target_flags"}, "tests_run": N, "executed_tests": [...], "command", "base", "exit_code", "notices", "compile_ms", "test_ms", "package_timings"}`. `notices` keeps at most 1,000 host notices, each cut to 2,048 characters with a marker; when the run printed more, a last `ci-test-affected:` entry counts the unlisted ones, and because that entry is not a deferral the deferred-sandbox handoff (**Deferred fixtures in an implementer's gate**, below) refuses the summary. The relayed output and the log still carry every notice whole. `compile_ms` and `test_ms` are millisecond totals. `package_timings` is a list of `{package, compile_ms, test_ms}` sorted by package name. Each total equals the sum of the per-package integers for that phase, except under the `cargo test` fallback described below. Compile time is the sum of Cargo unit durations from the `--timings` report of the build the gate already runs (those durations overlap when compilation is parallel, so the total can exceed the build's wall time). Test time is the sum of nextest JUnit testcase durations (testcase durations overlap when tests run in parallel). A phase that was not measured is `null` on the total and on every package. The `cargo test` fallback has no per-package test split: libtest's `finished in` time has no package, so those `test_ms` values stay `null`, and the total is that libtest time. Under the fallback the total is therefore not the sum of the `test_ms` values. Nothing selected writes `compile_ms` 0, `test_ms` 0 and an empty `package_timings`. The record written before the run starts leaves all three `null`. `tests_run` comes from nextest's JUnit report (enabled with `--tool-config-file`); it is `null` after a build failure or under the `cargo test` fallback. `executed_tests` contains sorted, deduplicated nextest `<binary-id> <test>` identities from non-skipped JUnit testcases; a missing or malformed report leaves it `null`. The selection is written before the tests start (with `tests_run: null`), so a run stopped part-way still reports it. The other fields are what an [implementer's deferral](testing.md#test-process-environment) is derived from: `command` is `ORBIT_VALIDATION_GATE`, which the make target sets to `make ci-test-affected` (`null` when the script runs directly); `base` is the comparison base (`null` when given a selection); `exit_code` is the run's final status (`null` until it finishes); and `notices` lists every line of the nextest stream that leads with `SKIP:`, `DEFERRED:`, `skipping:` or `skipping ` (empty under the `cargo test` fallback).
- `ORBIT_VALIDATION_SELECTION`: the candidate's `selection`. The script tests exactly those packages; a package the checkout lacks fails the run without a summary. A selection that also names `failing_tests` (each a nextest `<binary-id> <test>` or a bare test name, as the failed run printed them) runs only those tests: it builds only the packages their binary ids name, filters with a nextest filterset (`cargo test -- --exact <names>` in the fallback), and reports the selection it was handed. A malformed list, or a binary id outside the packages, fails the run without a summary.

A base run counts only if it reports the same selection, and a base pass only if it ran at least one test; a replay of `failing_tests` must prove every requested identity ran, whether it passed or failed. Qualified requests match both binary and test; bare requests match the exact test path in any binary. Missing, malformed, empty or partial `executed_tests` evidence refuses comparison. A run without the required selection or execution evidence is not comparable: it never makes a failure `baseline_red`, never refutes a reviewer's red-base claim and never lifts a hold that recorded the selection.

Older narrow cache entries without this proof are declined and replayed under the existing cache lock; compatible full-selection caches remain reusable. Owner and claimed candidate validation first reuse a conclusive comparable full cache, otherwise make one exact base replay for fully identified non-timeout test failures. Compiler, lint, unknown-identity, missing-selection, timeout and host-refused outcomes keep full comparison. The full required candidate command is unchanged.

Required-command artifacts share a 256 KiB source-output budget across stdout and stderr; a short stream remains complete. Oversized streams retain their head and tail plus bounded nextest failure blocks, including captured scenario and assertion output between test statuses. Failure blocks share at most a quarter of each stream's budget (up to 64 KiB, 16 KiB per block and 128 blocks); oversized blocks keep their beginning and end so an early flood cannot crowd out later failures. Explicit byte-omission markers disclose cuts, and a block-count notice discloses when more failures were printed than selected. Output is valid UTF-8. Failure identities and host-coverage classifications still come from the complete supervised streams, independently of the bounded reader evidence.

A command that writes no summary has no selection check, so only `make ci-test-affected` is guarded against a zero-test base pass. Reproduction requires comparable completed runs. When the candidate reports failure identities, it must report at least one, and every candidate identity must be present in the base failure set. Full comparisons also require equal exit status; exact test replays can use a different status because they run only part of the selection. When neither run reports failure identities, matching nonempty `diagnostic fingerprint sha256:` values and the same exit status establish reproduction; timeouts and coverage refusals cannot establish this identity-less match, including on exact test replays. Both logs retain the parsed sets even when captured output is bounded. A run stopped by the executor's output capture limit or by a signal is an infrastructure failure, never a test verdict. When a capture limit closes a pipe and its producer exits with SIGPIPE, the primary diagnostic names the observed capture limit; the supervisor retains both causes and the original child status. SIGPIPE without a capture stop and other termination signals retain their signal diagnostics. Candidate stops report the validation-environment marker without a base replay; stopped base runs are inconclusive, never cached as conclusive results and never establish `baseline_red` or lift a hold. Owner known-red claim settlement retains inconclusive attempted runs separately so retries reuse its refusal; ordinary validation and reviewer base comparisons ignore those records. Candidate validation reports the infrastructure cause when its base comparison was stopped.

When a comparable base passes but the candidate names at most three failed tests, delivery validation makes one isolated candidate replay using the `failing_tests` selection. The affected-test runner executes that selection serially (`--test-threads 1`) with nextest retries disabled; it skips unrelated tests. Acceptance requires a passing replay with the exact requested selection, at least as many executed tests as named failures, and no host coverage refusals. Missing summaries, unknown failures, timeouts, or larger failure sets do not qualify. A repeated failure keeps the ordinary candidate verdict. A passing replay records `flaky_validation` in the run and task history, preserves the initial failure and passing base/replay outputs, and names the tests with captured rerun evidence in the PR. The owner files each command/test signature once as a friction, including for accepted claimed handoffs.
