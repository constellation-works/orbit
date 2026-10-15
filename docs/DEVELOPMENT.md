---
type: context
summary: "Orbit development reference: validation, tests, docs index, MSRV, supply chain, vendored JS"
last_validated: 2026-10-14
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

For tasks that deliver a diff, run these gates before implementation handoff or before-PR review:

```bash
make ci-fast           # MSRV compile check, formatting, script guardrails; no Rust tests
make ci-test-affected  # complete test targets of changed crates and their workspace dependents
make ci-lint           # clippy for production and all targets, then rustdoc
make goldens           # CLI/MCP, CI log, sandbox and SQLite ledger schema goldens; UPDATE=1 regenerates
```

When an edit changes any generated row in `docs/INDEX.md` (for example, indexed Markdown frontmatter, its first heading, or summary), run `make docs-index`, review the generated index, and run `scripts/generate-doc-indexes.sh --check`. Include any resulting `docs/INDEX.md` change with the source edits, even when `make ci-fast` is pipeline-owned final validation.

When renaming or consolidating a test selected by macOS CI, update the filter
in `.github/workflows/ci-macos.yml` and run `make check-ci-macos`. This lists
actual compiled tests, including the workspace feature set, so a filter cannot
silently become a zero-test pass.

The Windows, macOS and website workflows run their real jobs on every open
PR, including docs-only changes, so requiring those checks cannot leave a PR
pending because of workflow path filters. Their check names stay stable:
`Windows cargo check (x86_64-pc-windows-msvc)`, `macOS Clippy (informational)`,
`macOS Sandbox` and `Check and build`. The historical `informational` label
does not weaken Clippy: the job runs `make ci-lint` and fails on errors.
`scripts/check-ci-macos.sh` requires an unfiltered PR event and retains exact
target/filter listings and native execution evidence validation. Push filters
and branch selection remain independent; website pushes still run only on
`main` for website, changelog or website-workflow changes. Required-check
settings are managed separately in the repository's provider rules.

The task and MCP CLI integration binaries use named contract tables, with
four and eight nextest slots respectively. Each row keeps its assertions and disposable state; rows that
share a workspace address independent task IDs. Exact-child fixtures isolate
the complete table once, rather than re-executing removed test entry points.
The [MCP and task test budget report](reviews/cli-mcp-task-test-budget.md)
records the coverage mapping and candidate-host before/after measurements.

Tasks tagged `no-diff-expected` run no workspace gates or required validation
commands and attach no `no-diff.json` or workspace validation logs, in owner or
claimed mode. Run only commands the task's own brief requires; a QA sweep may
build and run the product, and a review may reproduce a finding when its brief
asks for it. Keep the brief's evidence artifacts and execution summary.

- `make ci-fast` runs independent repository script checks in concurrent batches of eight, captures each script's output separately, prints per-script wall times, and names every failing script before returning the first failure's exit code. Fixture self-tests run sequentially only when their inputs change: `scripts/`, `.github/`, `Makefile`, `.cargo/`, the root Cargo manifest/lockfile, `install.sh`, `RELEASING.md`, `plugin/`, `npm/`, `.agents/` or `.claude-plugin/`. Selection uses the same `CI_TEST_BASE` override and merge-base fallback as affected tests and includes committed, staged, unstaged, deleted and untracked paths. If Git cannot resolve the base or diff, every self-test runs. The QA sweep inventory check still runs for crates-only changes because it reads live CLI/MCP/web surfaces. Full `make ci` always runs all self-tests. The MSRV compile check is timed on its own, separate from those scripts: make prints `ci-fast: MSRV check <seconds> s` for `cargo +<MSRV> check --workspace --locked`, and each guardrail keeps its own `ci-guardrails: <name> <seconds> s` line.
- `make ci-fast` passing says nothing about the Rust tests. Focused test filters help you investigate but do not replace `make ci-test-affected`: an existing test can encode behavior your change breaks even when your new tests pass.
- Run Cargo-based gates (`ci-fast`, `ci-test-affected`, `goldens`, `ci-lint`, `cargo test` / `nextest`) one at a time when they share a target directory, or give each its own `CARGO_TARGET_DIR`. Concurrent gates in one target can rebuild `target/debug/orbit` under running generation-bound fixtures and fail them.
- Guardrails need Python 3.11+ for `tomllib`; stock macOS `/usr/bin/python3` is 3.9. [require-python.sh](../scripts/require-python.sh) runs first in `make ci-fast`, `make ci-lint` and `ci-guardrails.sh` and stops with the version it found. It installs nothing: put a newer `python3` first on `PATH`.
- Linux CI installs nextest, cargo-deny and uv through [ci-download-verified.sh](../scripts/ci-download-verified.sh): `scripts/ci-download-verified.sh HTTPS_URL SHA256 NEW_FILE`. Each invocation allows at most three GET attempts under one 60-second monotonic budget, including backoff and digest verification. Only curl DNS, connection and timeout exits (6, 7, 28), and HTTP 429/500/502/503/504 with curl exit 22, can retry. TLS, authentication, partial transfers, unknown outcomes and digest mismatches fail closed. HTTPS-only redirects and normal certificate checks remain enabled; curl configuration files and internal retries are disabled. Each attempt uses fresh bytes, and only a matching digest publishes the new destination; existing destinations are refused. Installer traps remove scratch archives on success or failure, and archive/version checks never retry. Attempt diagnostics and total elapsed time remain in the Check job log: all setup attempts, backoff, failures and post actions count toward the complete job's 900-second budget. Offline public helper and workflow installer fixtures run in the existing guardrails via [test-ci-download-verified.py](../scripts/test-ci-download-verified.py).
- Skill docs are canonical under `crates/orbit-core/assets/skills/`. Edit them there and run `scripts/sync-plugin-skills.sh` to regenerate `plugin/skills/`; `make ci-fast` fails on a stale mirror.
- [check-doc-links.py](../scripts/check-doc-links.py) checks tracked and non-ignored untracked Markdown, and fails on a missing relative link target, heading anchor or backticked repository path that is not a tracked or non-ignored untracked file present in the worktree. Website links resolve through Starlight routes and code blocks are ignored; its exclusions (historical decisions, RCAs, templates, changelog, fixture prose) are listed in the script. Run `python3 scripts/check-doc-links.py`; its fixtures are in `python3 scripts/test-ci-fast-guards.py`.
- Repository source guards reuse work only within one invocation. The orphan-module guard enumerates the same production and sibling-test trees and masks each owner once for lexical module declarations. Its ripgrep-selected `#[path = "..."]` scan resolves direct plain path literals using [Rust's enclosing module directory rules](https://doc.rust-lang.org/reference/items/modules.html#the-path-attribute): paths outside inline modules start at the source directory; inline modules add their directory components and, for ordinary file modules, the source stem. An inline module's own path attribute replaces its default directory. The scan masks comments and literals, tracks nested braces, and compares actual destination files, including `.` and `..` components; an unrelated file with the same basename cannot register a production module or sibling test. It accepts declarations regardless of cfg, rather than evaluating build configurations, and does not expand macros. The disposable command fixtures in `python3 scripts/test-ci-fast-guards.py OrphanModuleTests` compile miniature crates with `rustc --test` and enumerate their tests to verify inline and nested path registration while keeping same-basename orphan files rejected. The CLI import guard reads each source once with the same comment/string masking and per-crate diagnostics. The unused-dependency guard collects reference names once per file and combines them only within the original normal, dev or build scope. Each later invocation reads the current files again; no persisted source cache, reduced guard selection or wider concurrency is involved.
- The guard fixtures retain full runner success/failure checks for shell-wait routing, then exercise the remaining syntax and exception rows directly through the checker executable. Action-pin fixtures record interleaved curl attempts and exact sleep requests instead of waiting for simulated network retries; the production guard still performs its normal backoff.
- [check-unused-dependencies.py](../scripts/check-unused-dependencies.py) fails on a manifest dependency its sources never use, a normal dependency only test targets use, or a `[workspace.dependencies]` entry no member declares. Keep a feature-only dependency in its `FEATURE_ONLY` allowlist with a reason. Self-test: `python3 scripts/test-check-unused-dependencies.py`.
- [check-windows-portability.py](../scripts/check-windows-portability.py) runs in `make ci-fast` and rejects recognizable Unix std APIs reachable on Windows, including test helpers. It checks Rust files under `crates/`, follows external and inline module declarations (including `#[path]`), and accepts enclosing item, module or file gates that exclude Windows. It evaluates `unix`, `windows`, `target_os`, `target_family`, `all`/`any`/`not`, and conditional `cfg_attr`; unknown features and test flags remain potentially enabled. `cfg!(unix)` does not remove code from compilation. The static approach runs on every worker without installing a Windows target or linker, and needs no path allowlist. It scans Unix std paths/grouped imports, distinctive extension calls such as `from_mode`, `pre_exec` and `ino`, qualified permission/metadata trait calls and `permissions().mode()`. It ignores comments and literals. It does not expand macros or infer receiver types, so ambiguous application methods such as `mode`, `set_mode` and `blocks` are checked only in recognizable std forms; hosted Windows compilation remains the complete check. Self-test: `python3 scripts/test-windows-portability.py`.

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
cleanup remain real. Build-slot fixtures use the existing CLI supervision clock
only after a real wrapper heartbeat proves the queue interval. This removes
host-speed assumptions without extending timeout or cleanup budgets.

`make ci-test-affected` (`scripts/ci-test-affected.py`) reads Cargo metadata without compiling. It selects every crate with changed paths plus all transitive reverse workspace dependents (dev, build, renamed, optional and target-specific edges), then runs their library, binary and integration test targets with nextest (Cargo if nextest is missing) and Cargo doctests, under the shared [build-budget admission](runbooks/build-budget.md). A change under `crates/orbit-core/` selects `orbit-core`, `orbit-cmd`, `orbit-web` and `orbit-cli`.

- **Diff:** committed, staged, unstaged and non-ignored untracked paths; a move selects both crates.
- **Base:** the merge base of `HEAD` with `origin/agent-main` (local `agent-main` if the remote ref is absent). Override with `CI_TEST_BASE=<rev>`; a reviewer with a pinned delivery base must run `CI_TEST_BASE=<base.commit> make ci-test-affected`. An unavailable base fails the gate.
- **Everything or nothing:** a change to `Cargo.toml`, `Cargo.lock`, `.cargo/`, `.config/` or the toolchain files, or a removed crate, selects every crate. A docs-only diff selects nothing and passes without compiling.
- **Cross-crate file reads:** Cargo cannot see a test reading files outside its crate. Declare the path prefix and reading crates in `FILE_READERS` in `scripts/ci-test-affected.py`; nothing detects them for you, and a declared crate missing from metadata fails the gate.
- **macOS workflow names:** when the diff adds, deletes, renames or edits a path under a `tests/` directory of a crate (`crates/**/tests/**`, `crates/**/src/**/tests/**`) or edits `.github/workflows/ci-macos.yml`, the gate first runs `scripts/check-ci-macos.sh` under build admission. It lists the tests the macOS workflow names and runs none, and exits before any test runs if one of its paths or filters no longer matches. The failure names the stale entry in `.github/workflows/ci-macos.yml`, which is the file to fix. A diff touching neither skips it. A replayed `ORBIT_VALIDATION_SELECTION` carries no diff, so it skips the check too: the candidate run that named the selection already ran it.
- **Inspect:** `python3 scripts/ci-test-affected.py --list [--base <commit>]`.
- **Test environment:** nextest runs with `--success-output immediate`. The gate archives its complete merged stdout/stderr in a unique `ci-test-affected-nextest-*.log` under `.orbit/tmp/` and streams progress, summaries, diagnostics and failing-test blocks. Passing-test blocks stream only host-evidence notices (`DEFERRED:`, `SKIP:`, `skipping:` or `skipping `, and sandbox-unavailable markers), so a full selection stays within the executor's default capture limit while the host judge still sees every refusal. The no-nextest `cargo test` fallback is unchanged and does not expose passing-test captured output. A temporary directory inside the checkout is added to `GIT_CEILING_DIRECTORIES`. `ORBIT_WORKER_CONTEXT_REQUIRED` is dropped ([why](#test-process-environment)); the rest of the run envelope passes through.
- **Observed streams:** Cargo fallback and doctest commands drain stdout and stderr independently and capture whole lines. A relay I/O failure stops the command's process group before waiting for the readers, then propagates the original error. These commands own a separate session; the gate forwards graceful SIGTERM shutdown through cleanup, with SIGKILL escalation after a short grace to stop descendants holding the pipes. Killing the gate directly with SIGKILL cannot run this cleanup.

To check whether a failure is pre-existing, replay the same command on the unmodified base: extract it under `.orbit/tmp/` (for example `git archive <base> | tar -x -C .orbit/tmp/base`), refresh its timestamps, give it its own `CARGO_TARGET_DIR` (any path works; the `orbit update` fixtures copy the tested binary to their own `target/debug/orbit`), and set `GIT_CEILING_DIRECTORIES` to `.orbit/tmp` so its fixtures cannot find the surrounding checkout. Replay only the failing tests, not the whole selection: hand the candidate's summary `selection` back with `failing_tests` added (below), or run nextest with `-E 'binary_id(=<binary-id>) & test(=<test>)'` for each failure it printed. Replay soon after the candidate run, since fixture deadlines depend on host load.

### Required validation commands

The owner workspace lists the four [gates](../AGENTS.md#gates) in [`workflow.required_validation_commands`](CONFIG.md), in ignored per-user `.orbit/config.toml`. The cheap gates come first, because validation stops at the first failing command:

```toml
[workflow]
required_validation_commands = ["make ci-fast", "make ci-lint", "make goldens", "make ci-test-affected"]

[workflow.validation_env]
build_output_env = ["CARGO_TARGET_DIR"]
```

The reviewable [workspace configuration example](../config/orbit-workspace.toml) includes
both settings. Merge it into the owner configuration during operator rollout. Cargo
output selection is this repository's policy; the generic runtime has no build-tool
default ([build output contract](CONFIG.md#workspace-build-output)).

For tasks that deliver a diff, candidate validation runs them once, in that order, on the final candidate after base synchronization, and before-PR review requires a passing record of each. Tasks tagged `no-diff-expected` do not run this list. A managed implementer finds the list in its envelope's `task.final_validation_commands` and leaves those commands to that validation ([required validation](CONFIG.md#required-validation)); it still runs focused checks and any gate the list omits, so a shorter list moves no gate out of the delivery.

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

Hosted CI runs the full `make ci` on open PRs. Each PR-triggered workflow starts with a checkout-free `Live PR state` job (`pull-requests: read` only); if the PR merged or closed while queued, the build and analysis jobs are skipped under unchanged check names, which GitHub treats as passing. An API error fails the gate rather than admitting an unknown state; a PR that closes after the gate read `open` can still finish. Push, scheduled and manual runs skip the gate, so agent-main/main CI and CodeQL still cover merged changes. `make ci-fast` selects `scripts/test-pr-state-workflows.py` when self-test inputs change; full `make ci` always runs it. The script replays each gate against recorded API responses.

The ephemeral Linux runner installs sandbox packages with needrestart's
per-invocation `NEEDRESTART_MODE=l` (list-only) setting through `sudo env`.
This avoids restarting unrelated image services while retaining signed apt
installation, the new Bubblewrap binary's actual namespace probe, and the
guarded packaged AppArmor profile checks. See the
[Ubuntu needrestart manual](https://manpages.ubuntu.com/manpages/noble/man1/needrestart.1.html)
for the environment override; this does not change a persistent host setting.

`ci-guardrails.sh` repeats its phase breakdown in the final log tail, including
on failure: format, workflow test listing/build, both Clippy passes, workspace
tests, doctests, documentation, supply-chain checks and repository guards.
These are wall seconds at Bash's one-second resolution with the original
command exit status. They let bounded hosted excerpts retain build/check costs
separately from nextest testcase sums and the full GitHub job elapsed time.

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

`clear_inherited_authority` removes the managed-run routing, identity and grant variables, including `ORBIT_WORKER_CONTEXT_REQUIRED`. [`tool_list.rs`](../crates/orbit-cli/tests/tool/tool_list.rs) uses this pattern; [`isolated_cli_fixture.rs`](../crates/orbit-cli/tests/support/isolated_cli_fixture.rs) is a shared disposable home, registry and workspace. Orbit core's libtest fixtures use `application::tests::run_isolated_test`, which re-executes one exact test this way and verifies it passed in the child. CLI MCP and output bodies that write databases or change process authority use [`isolated_test::run_in_child`](../crates/orbit-cli/tests/support/isolated_test.rs) before any setup; it verifies exactly one passed test and supplies a private home under the checkout's `.orbit/tmp/`, outside Bubblewrap's `/tmp` overlay. It includes ignored entry points only when their parent has already been selected by the caller.

Runtime integration permutations use `scenario_table!` in the existing runtime binary. Each named row keeps its setup and assertions; the shared [table fixture](../crates/orbit-core/tests/runtime/scenarios.rs) re-executes one isolated table with inherited authority cleared, gives each sequential row a fresh HOME/cwd, restores its environment, and verifies one passed child. Families that install process-wide worker substitutions retain a separate child per row. Keep self-reexecuting entry points as standalone tests, since their exact libtest names are part of their fixture protocol. Table failures report the original row identity, and sandbox deferrals still reach the gate output.

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
attempts and the elapsed/waited diagnostics. Broker request fixtures capture a
per-listener request timeout when starting their workers. The isolated stdin
fixture also scales its output-drain allowance with a `cfg(test)` guard and
asks its escaped helper to acknowledge kernel `POLLHUP` after finalization,
while that helper still holds the unread pipe.

These thread-local hooks restore prior clocks on unwind and require the
dev-only `test-support` feature. Cleanup and the outer child-test hang guard
keep their real clocks, so a broken handshake still fails and is reaped.

Gates often run beside a busy drain, where a fixture's wall-clock time stretches tenfold. Keep outcomes independent of host load:

- Re-execute child tests through `orbit_common::test_env::run_child_test` and verify them with `assert_child_test_passed`. It kills a child that prints nothing for `CHILD_TEST_DEADLINE` (300 s), so a child that reports progress is slow, not hung, and keeps running until `CHILD_TEST_CEILING` (540 s, below nextest's ten-minute kill). Either overrun reports load averages and the child's output. The child leads its own process group; a watchdog process kills that group if the test process dies first (nextest interrupt or timeout), so no fixture child outlives its parent.
- End a wait when its event arrives, size its ceiling for a saturated host, and on expiry report what it saw plus `orbit_common::test_env::host_load()`. `test_env::wait_until` does this for a readiness condition under `FIXTURE_STEP_DEADLINE` (120 s). Bound a single CLI command or HTTP request by the same guard, and call `test_env::assert_step_finished` before asserting on a command's outcome, so a command killed at the guard does not read as a refusal.
- Dashboard fixtures launch with `--port 0` and read the actual bound address from their owned child's startup announcement. MCP signal fixtures use `127.0.0.1:0` and enable the existing bind tracing event with a fixture-only `RUST_LOG`. The shared announcement reader bounds startup and output, checks child exit before accepting readiness, and keeps draining pipes until child cleanup. An early exit drains only until the remaining readiness deadline, including partial lines and inherited output pipes. Handover needs a nonzero command-line port because exec reuses argv: its child claims a candidate directly, retrying only a confirmed address-in-use failure, and the replacement must announce the original assigned address. Never select a port by binding and dropping a temporary listener or accept a TCP connection as child readiness.
- Dashboard handover and shutdown fixtures use a shared TCP helper: one operation deadline covers connect, request writes and response reads, including SSE setup. Each socket call uses the remaining budget, so partial progress cannot reset the deadline; a failure reports the IO stage, peer and budget and unwinds through `ChildGuard`. The shutdown stopwatch starts at SIGTERM, after connection setup.
- Admission reads the host resource monitor, so a fixture not testing throttling pins a calm sample with `OrbitRuntime::with_host_resource_probe` on every runtime it opens, including reopens; a CLI fixture sets `workflow.resource_throttle.enabled` to false with `orbit config set` in its disposable config. Throttling tests drive their own probe, stamping each sample's age when the probe is read rather than at setup, so slow setup cannot make it stale.
- Prefer a deterministic behavioral guard over a timing ceiling: cold search hydration uses corrupted payloads and a canonical-read negative control to prove it skips artifact verification, with a bounded seed. When a cost guard is necessary, bound work rather than elapsed time using CPU time where possible.
- Synchronize generation fixtures with the updater's admission lock or installation rename before releasing held participants. The dashboard handover fixture hard-links the built old image into its private installation (copying only across volumes), since installation replacement never modifies the old inode; this avoids another first-exec macOS assessment. It creates its distinct, assessed replacement directly beside the installed executable and renames that inode into place, avoiding a second image copy and macOS assessment while retaining the real installation boundary. Its pre-install participant record must still name the old generation. The MCP oversized-request fixture observes structured `orbit.mcp.handover` deferral events before completing the request; elapsed time alone does not establish that a lifecycle check ran.
- macOS assesses every new executable file on its first exec and holds the process meanwhile. For the debug `orbit` image that takes seconds, and it queues behind every other new image on the host, so a timer started at spawn includes the wait. Run a freshly written binary once before anything times it: the generation fixture's `distinct_copy` calls `assess`, which runs and validates `update --contract --json` with inherited authority cleared before installation. The assessment survives a rename. When Orbit itself probes a copy it just staged, as `orbit update` does, set `ORBIT_UPDATE_PROBE_TIMEOUT_SECS` to a hang guard rather than relying on the 30 s default.
- Test runs skip durability syncs. Every `File::sync_all`/`sync_data` goes through `orbit_common::fs::io::sync_file`/`sync_file_data`, and every SQLite `synchronous` pragma takes its level from `fs::io::sqlite_synchronous`. With `ORBIT_TEST_RELAXED_DURABILITY=1` they return without flushing and the pragma becomes `OFF`. On macOS each sync is an `F_FULLFSYNC` that flushes the whole drive's write cache (about 20 ms), so concurrent suites queued behind each other's flushes and Mac gates ran 5-20x slower than Linux. `.cargo/config.toml` sets the variable in `[env]`, so `cargo test`, nextest and every `make` gate get it, and so do the `orbit` children and re-executed child tests they spawn. A value already in the environment wins: `ORBIT_TEST_RELAXED_DURABILITY=0 cargo test …` restores real syncs. The variable is read once per process, and only a build with `debug_assertions` honors it. A release or installed binary always syncs, which `orbit-common`'s `durability_syncs` tests check. A `cargo run` debug build inherits the variable too, so don't point one at state you need to survive a power loss. Only a power loss or kernel crash can tell a skipped sync from a real one. A test whose subject is the sync, or a durability or crash-recovery path that should exercise the real call, opts back in: in-process with the `orbit_common::test_env::require_durable_syncs()` guard (process-wide while it lives), or by removing the variable from a child's command, as `orbit-core`'s `upgrade_resume` does. `make ci-lint` (clippy `disallowed-methods`) and the `check-durability-syncs` guardrail in `make ci-fast` reject a direct sync or a pragma with a literal level. This deviates from STD-03@2 §R5, which requires the fsyncs, in test runs only: production durability is unchanged.
- Every `orbit` command joins the executable generation by the SHA-256 of its own image, remembered per generation root in `.generation-image-digest.json` ([upgrades runbook](runbooks/upgrades.md)). A fixture with a fresh `HOME` starts without that cache: its first command hashes the whole debug binary (about 200 MB), and the first writer, usually `workspace init`, records the digest once its join creates the root. Read-only commands before any writer hash every time. The workspace depends on `sha2` 0.11 directly, which selects the SHA-2 instructions at run time on both x86 and Apple silicon; 0.10 hashed in software on aarch64. On an M4 Pro one digest of a 206 MB debug `orbit` took 364 ms with 0.10 and 86 ms with 0.11 (median of three sequential runs, page cache warm). That hash was the main Mac test-time contributor found: between a claimed Mac leaf run before it and one after it (different commits, about 2,650 tests each), summed test time fell from 3,588 s to 2,925 s, most in binaries that run the CLI with fresh fixture homes (`orbit-cli::output` 0.54x, `orbit-cli::tool` 0.57x, `orbit-cli::task` 0.63x). The largest Mac binaries by summed time remain `orbit-cli::process`, `orbit-core::runtime` and `orbit-engine::engine`. Encode a digest with `orbit_types::identity::lower_hex`, since 0.11 outputs do not implement `LowerHex`, and import `sha2` directly rather than through `rsa::sha2`. Share one fixture `HOME` across a test's commands rather than giving each command its own.
- Seeding pays an fsync per task outside relaxed test runs. Root a fixture that seeds thousands at `tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())`, which prefers tmpfs on Linux, and report its phases with `orbit_common::test_env::FixtureProgress` so an overrun names the phase. Its deadline bounds the gap between two items (`FIXTURE_STALL_DEADLINE`), not the fixture's total, and its stderr reports keep an isolated child's quiet deadline from expiring while it advances; `FIXTURE_CEILING` bounds the whole fixture.
- A production fallback taken under load (such as a timed-out login probe) may satisfy a fixture only if the result reports the fallback and its reason.

### Tests that submit pipeline runs

Submitting a run (ship, resume, auto, job) spawns a detached worker by re-executing the current binary. From a test, that binary is the libtest harness, which would rerun the spawning test without bound ([RCA](rca/2026-09-23-cross-crate-test-worker-oom.md)), so only the CLI `main` may act as a worker and an unsubstituted submission fails with `OrbitError::Execution`.

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
- A fixture that compares Orbit's reported paths with paths it spelled creates its temp dir with `tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir())`, or canonicalizes its side. A test that must hold everywhere builds its own symlink with `std::os::unix::fs::symlink`.
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

Browser checks need Playwright and Chromium, plus Chromium's shared libraries and a font configuration on Linux. A prepared host has a kit at `~/.local/chromium-deps`; its `env.sh` exports `PLAYWRIGHT_BROWSERS_PATH` and `PLAYWRIGHT_MODULE` (Playwright's `index.mjs`), and on Linux also `LD_LIBRARY_PATH` and `FONTCONFIG_FILE`. Source it first. To build, verify or upgrade the kit on Linux or macOS, see the orbit-setup [browser kit reference](../crates/orbit-core/assets/skills/orbit-setup/references/browser-checks.md). On a host without it, set `PLAYWRIGHT_MODULE` yourself and stage dependencies under `.orbit/tmp/` ([runbook](runbooks/website-validation.md#stage-playwright-and-chromium-without-root)). Skia FontConfig crashes and missing `lib*.so` errors are host evidence, not a failed check.

```bash
. ~/.local/chromium-deps/env.sh
node crates/orbit-web/src/tests/dashboard_operations_browser.mjs \
  "$PLAYWRIGHT_MODULE" .orbit/tmp/dashboard-browser
```

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

Each runs as `node <script> "$PLAYWRIGHT_MODULE" .orbit/tmp/<evidence-dir>` and saves screenshots and any JSON named below. The dashboard is dark-only, so there is no light-theme pass. The loading and distributed scenarios also run in the QA sweep.

| Script | Evidence dir | Covers |
|---|---|---|
| `crates/orbit-web/src/tests/dashboard_loading_browser.mjs` | `loading-browser` | Task search dismissal (`--task-lookup` narrows to this control): Clear beside search works with empty input and listed or filtered-out ID results, direct links, keyboard Enter/Space at 1440 and 375 px, query/route reload and history, not-found/server errors and held lookup/hash/refresh responses (`task-lookup-clear-result.json`, `task-lookup-*.png`, `task-deep-link-*.png`). Task labels and navigation (a run's task link opens the task under the operator's filter, a copied URL reopens it in a new tab, Back returns to the run, and a link into another workspace adds one history entry so Back and Forward restore each entry's workspace and requests; `task-link-history-result.json`), workspace scope (a host or window change retires the previous Reliability report while the replacement read is held or fails; a same-scope refresh keeps it labeled refreshing or stale), responsive layout, row keyboard access, Ship dispatch. Task titles stay on one line with an ellipsis and a consistent compact row height, the full title stays reachable by tooltip and accessible name, and the status/crew chips reveal their native editors on focus or click. Checks title capacity beside the default dock at 1280×800 and 1440×900, the stacked row at 390 and 375 px, and edits (`task-titles-result.json`, `task-titles-<width>.png`): Tab reveals the crew editor before a selected-option mutation; status uses native keyboard typeahead (`i` for `in-progress`) and requires a trusted change event plus the exact status PATCH. Native popup ArrowDown/Enter does not select an option in macOS headless Chromium. A saved status change from the detail folds it in, clears `open=` and keeps its feedback and undo on the collapsed row, or in a notice when the new status is filtered out; a failed or declined change keeps the detail open with its feedback, and a refreshed status leaves rows as they were (`task-status-folded.png`, `task-status-notice.png`). Review and in-progress rows and the detail link a task's pull request only when its `github-pr` ref has an http(s) page (`pull-request-links-result.json`). `--run-detail` narrows it to run detail, cancel/replay and child outcome tallies, state colors, durations and navigation at desktop/mobile widths (`run-detail-result.json`, `run-actions-result.json`, `run-child-outcomes-result.json`). |
| `crates/orbit-web/tests/http_api/dashboard_run_events_browser.mjs` | `run-events-browser` | Newest event page and total count, earlier pages, timeline retry markers and legend independent of paging/Refresh, delayed retry responses after run/workspace round trips, retry truncation and scan failure/recovery, ISO day labels and absolute seconds across midnight, first-load skeleton and loaded steps retained during delayed Refresh, read failure, desktop and narrow layout (`result.json`). |
| `crates/orbit-web/src/tests/dashboard_operations_browser.mjs` | `dashboard-browser` | Responsive shell at 375×812, panel layout and focus, Jobs Live / History / Catalogue visits and retired list routes, Catalogue routine navigation, Drain blockers and the **Proposed tasks** control, auto-task status tokens, a task's running run with a long job id wrapping inside its 280px Running now card while the task and run links stay scoped (`running-now-280.png`). |
| [dashboard_rail_browser.mjs](../crates/orbit-web/src/tests/dashboard_rail_browser.mjs) | `rail-browser` | Rail label/count geometry at 1440, 1280, 1024, 761, 760, 390 and 375 px. Health and Analytics retain readable labels, Health with its three counts (doctor, denials, routines), including large pills and removal of the attention count; desktop attention pills use a second line, while mobile destinations remain horizontal. Checks text clipping, sibling separation, button containment and page overflow, and captures every mobile destination. Repeated destination and Health subtab changes through the shipped router keep one indicator per bar (`measurements.json`, `rail-<width>-<case>[-<tab>].png`). |
| `crates/orbit-web/tests/http_api/dashboard_durations_browser.mjs` | `durations-browser` | Duration formats and wrapping in the Metrics actor tables and Automation, running `↻` durations (`measurements.json`). |
| `crates/orbit-web/tests/http_api/dashboard_metrics_browser.mjs` | `metrics-browser` | Analytics › Metrics trends: duration series cards and deltas, line gaps and hollow low-sample points, hover and keyboard tooltips, series toggles (the last one stays), Succeeded / All outcome filtering with URL reload and history, statistic and Table views, the Tasks completed stacks, detail cards, a window change, the empty window's widen action and a 390px phone (`metrics-assertions.json`). |
| `crates/orbit-web/tests/http_api/dashboard_audit_browser.mjs` | `audit-browser` | Health › Incidents › Events and Analytics › Reliability › Tools tables across widths, a long Events list growing the panel under the pane's scroll, keyboard expansion, Events paging (Load older, shown-of-window header, refresh keeping loaded pages, status and hide-unconfirmed filters on every page). Held responses order an older page against a filter change: a retired older page that succeeds while the new head is pending, or succeeds or fails after that head fails, paints no rows or paging feedback, and a same-scope refresh landing under a held older page keeps the loaded tail. The full-width Analytics › Codebase view: tiles, the overlap list, the detail panel, a poll keeping tile, row and scroll, segment and empty-state actions rewriting the hash, 375 px and 1280 px layout and 4.5:1 text contrast (`codebase-<width>.png`). |
| `crates/orbit-web/tests/http_api/dashboard_polish_browser.mjs` | `polish-browser` | Settings, Health, Analytics and Automation deep links; a click walk through every Observe destination, rail view and secondary tab (Incidents, Events and Errors; Pipeline and Tools) checking the one visible view, the active rail entry, view and tab, the hash, remembered views and filters, Back and direct loads (`observe-<n>-<route>.png`); crew labels and usage references, Crews geometry (`measurements.json`). |
| `crates/orbit-web/src/tests/dashboard_refresh_browser.mjs` | `refresh-browser` | Failed and recovered refreshes: retained counts with `as of HH:MM`, the connection dot, failed run streams. |
| `crates/orbit-web/src/tests/dashboard_host_switch_browser.mjs` | `host-switch` | Host switcher: requests routed through `/api/on/<host>/…`, host precedence, failure and recovery. The log tail drops the old host's rows, buffered count and status line at the switch, while the new host's snapshot is held, failed, stale or empty, and reopens the stream from that host's cursor. Back and Forward between hosts, with a shared workspace ID and with different ones, restore routing, both pickers and write capability, and a held workspace list from the host left behind repaints nothing. Back and Forward from a single-workspace host restore All workspaces on a multiple-workspace host, preserve aggregate intent while destination discovery is held, and fetch aggregate tasks or runs after it settles; a single-workspace destination selects its only workspace and repairs the URL. State assertions import the same `/static/js/common.js` module as the shipped app (`host-switch-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_doctor_browser.mjs` | `doctor-browser` | Health › Doctor rows, remediation, cached report age, the busy state of a first run and of a refresh while the fixture holds the response (Refresh disabled, progress shown, controls restored after success or failure), a cached read delayed past a newer refresh or a host switch never replacing the shown report or rail count, a newer cached report still shown, remote and too-old hosts (`doctor-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_incidents_browser.mjs` | `incidents-browser` | Health › Incidents class tiles and filtering, lifespan bars for a recurring and a single-burst signature, the detail panel (full message, Copy, truncation note and its audit drill-down, related signatures, runs), last-hour and no-message filters, cause headers over signatures that share a cause, the By cause / By signature toggle and the same-cause list with its grouping basis, the local 6h window, the sticky panel and a 375px phone. Full-message cache isolation uses independent host stores with the same audit row ID, plus held successes and errors across host/workspace round trips. The full dashboard covers a shared concrete workspace; a browser module harness covers All workspaces with the shipped common.js routing because the dashboard currently gates Incidents behind a concrete workspace (`incidents-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_config_effective_browser.mjs` | `config-effective-browser` | Settings › Effective layer counts and the Global card filter, row status and pool tokens, the restates-global filter, the inspector chain for an overridden and a not-inherited key, global-only and read-only keys, a save to the chosen file with its CLI preview, a refused save, removing a workspace override, crews by provider, paths, the Review findings switch writing `review.verify_findings` and reading it back after a reload, 44px targets and a 375px phone (`config-effective-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_config_workspace_file_browser.mjs` | `config-workspace-file-browser` | Settings › Workspace file summary counts and the card filter, keys grouped by TOML table, the If removed column for a global-restating, a built-in-restating, a not-inherited and an unreported key, the Not set sidebar, inline edit with its CLI preview and a refused save, Add key's candidates, selecting and removing the redundant keys with one refused unset, 44px targets and a 375px phone. Promise-held Remove, Save and Add responses across host switches and workspace round trips check stale success/refusal settlement, request scope and ownership of a newer pending Save; bound requests retain remote write authorization and reject expired visits (`config-workspace-file-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_config_global_file_browser.mjs` | `config-global-file-browser` | Settings › Global file header and machine card (id and task prefix read-only, Rename), the In effect / Overridden (same value) / Not inherited reach counts and card filter, each row's reach cell including an unreported one, the editor's reach warning for an in-effect, an overridden and a not-inherited key, a refused save, Remove from file, Add key, crew tables in place of "No crew is defined", 44px targets and a 375px phone. Promise-held Save, Add, Rename and Remove responses across host switches and workspace round trips check obsolete drafts, stale success/refusal settlement, request scope and ownership of a newer pending mutation. Owning refusals preserve drafts, and writes remain disabled through their reload, including obsolete reload finalizers (`config-global-file-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_provider_limits_browser.mjs` | `provider-limits-browser` | Crews Limit column, Drain provider-limit waits, contrast under both emulated colour schemes (`provider-limits-assertions.json`). |
| `crates/orbit-web/src/tests/dashboard_task_artifacts_browser.mjs` | `task-artifacts-browser` | Task detail artifact previews: a mounted image and download render from a blob URL that closing, replacing or changing workspace away from the detail revokes; a response held before its headers or inside its body is cancelled when the detail retires, and one that settles afterwards anyway is released without being mounted; clicks during a read do not refetch (`task-artifacts-assertions.json`). |

### Website changelog validation

After changing the changelog renderer, run `npm ci`, `npm run build` and `npm run check` in `website/`, serve `website/dist`, then:

```bash
node website/scripts/check-changelog.mjs \
  "$PLAYWRIGHT_MODULE" http://localhost:4187 .orbit/tmp/changelog-browser
```

It checks release dates, merged-PR links, version and subsection anchors, keyboard and no-JavaScript disclosure controls, the version badge and future-dated headings, and records heights and screenshots at 1440px and 375px in both themes. In a restricted executor, keep `PLAYWRIGHT_BROWSERS_PATH`, browser output, screenshots and JSON evidence under `.orbit/tmp/`; point transient `TMPDIR` at a sandbox-writable tmpfs when available. A minimal Linux runner may also need `FONTCONFIG_FILE` naming a config with local fonts.

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

Windows is supported through [WSL2](runbooks/windows-wsl2.md). Native Windows is compile-checked only: `.github/workflows/ci-windows.yml` runs `cargo check --workspace --locked --all-targets --target x86_64-pc-windows-msvc` on every open PR, so test targets must compile there, but no test runs.

The `x86_64-pc-windows-msvc` Rust target is a host prerequisite. An operator must install it outside the managed executor sandbox with `rustup target add x86_64-pc-windows-msvc`; once installed, the compile check only needs a writable Cargo target directory. A managed lane must not write to `~/.rustup` or install a global toolchain. If the target is absent, record the Windows cross-check as not run and give the reason, inspect the relevant source and platform-specific code paths, and rely on the PR's Windows Compile Check job for the cross-target result.

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
