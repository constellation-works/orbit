---
type: runbook
summary: Mint, execute, and judge the workspace-local complete pre-release Orbit QA sign-off.
tags: [operations, qa, release, automation]
paths: [".orbit/auto_tasks/qa-full-sweep.yaml", "scripts/qa-full-sweep*", "crates/orbit-core/assets/auto_tasks/qa-full-sweep.yaml"]
related_features: [auto-tasks, activity-job, dashboard, task-publication]
related_artifacts: [ORB-12010, ORB-15327]
last_validated: 2026-10-14
---

# Full pre-release QA sweep

`qa-full-sweep` is the intentional, workspace-local sign-off for Orbit. It is
disabled for periodic scheduling and routes minted work to the `opus` crew.
It complements the recent-change `qa-sweep`.

Orbit also ships a generic, product-agnostic `qa-full-sweep` default that
coordinates per-area surface-QA chores and a roll-up release decision
([auto-task overview](../design/auto-tasks/1_overview.md#embedded-default-catalog)).
This workspace's own `.orbit/auto_tasks/qa-full-sweep.yaml` predates it and
overrides it: `orbit workspace sync` and `orbit update` keep the file byte for
byte and never retire it, and sync warns that it occupies the bundled
default's name. That warning is expected here; do not move or rename the file.
The rest of this runbook describes this workspace's definition and harness.
The generic default's area brief also points workers here when the repository
provides this harness, including browser-kit and website preparation.

## Mint and run

From the registered `ws_orbit` owner checkout, inspect the definition before
creating work:

```bash
orbit auto-task show qa-full-sweep --format json
orbit auto-task mint qa-full-sweep --json
```

Manual minting intentionally ignores `enabled`, the schedule, and open-instance
dedupe. Record the returned task ID, then run that task through the normal
authorized delivery workflow. Do not enable the definition merely to perform a
release check. `skip_if_open` prevents periodic duplicates if an operator later
chooses to enable it.

The executor runs the harness from the repository root:

```bash
./scripts/qa-full-sweep.sh --build-candidate \
  --output .orbit/tmp/qa-full-sweep/qa-full-sweep-report.json --run-commands
orbit tool run orbit.task.artifact.put --input \
  '{"id":"<TASK_ID>","source_path":".orbit/tmp/qa-full-sweep/qa-full-sweep-report.json","path":"qa-full-sweep-report.json","model":"codex"}'
```

`orbit.task.artifact.put` is intentionally fail-closed: its `source_path`
must resolve beneath `workspace_root`, so ORB-11694 makes it refuse files
that are still in `/tmp` (and symlinks inside the workspace that resolve
outside it). Stage retained evidence under `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`;
gitignored, so it does not appear in `git status`) and attach from there:

```bash
stage_dir=.orbit/tmp/qa-full-sweep
mkdir -p "$stage_dir/browser"

# Attach each staged evidence file; this example attaches the browser result.
orbit tool run orbit.task.artifact.put --input \
  '{"id":"<TASK_ID>","source_path":".orbit/tmp/qa-full-sweep/browser/result.json","path":"browser/result.json","model":"codex"}'
```

Retained evidence must be inside the checkout (under `.orbit/tmp/`) before
`artifact put` runs. Do not stage into a tracked or untracked-visible path;
leftover copies outside `.orbit/` (or a generated report that is untracked)
are delivery-boundary failures. If the retained evidence is represented
by a file hash manifest instead of individual files, stage and attach that
manifest by the same sequence.

`--build-candidate` compiles the executable from the checkout into a disposable
target directory. The harness snapshots HEAD, the tracked diff, and untracked
inputs before the build, verifies that identity is unchanged after every local
scenario, and binds every result to the resulting candidate ID. Supplying an
installed binary without that build attestation intentionally makes provenance
FAIL, even when `--version` exits zero. The harness constructs a clean child
environment and disposable Orbit root and Git repositories for mutations.
Its disposable `orbit init` calls pass `--skip-host-prerequisites`, so they
never install packages or change security policy and need no supported Linux
distribution; the built-in scenarios do not exercise native sandbox dispatch,
which stays fail-closed until `orbit doctor providers` reports it ready. If a
disposable init or another prerequisite step fails, the report keeps that FAIL
and marks every scenario it blocked `NOT_RUN` with the reason in `stderr`.

For the browser leg, prefer the prepared host kit. Source
`~/.local/chromium-deps/env.sh` when it exists, then pass its module, browser
directory and optional Linux loader/font inputs explicitly. macOS needs only
the module and browsers; a loader search path may contain multiple directories
separated by colons. The harness forwards these declared inputs into its clean
browser child environment. The report records the launched Chromium version,
the capability probe and the declared paths, and retains screenshots and JSON
results in the output-side evidence directory.

The worker sandbox permits reads of `~/.local/chromium-deps` on both Linux
(`linux-bwrap`) and macOS (`sandbox-exec`). The plugin build sandbox is a
different boundary and is not used by this sweep. See the
[browser kit reference](../../crates/orbit-core/assets/skills/orbit-setup/references/browser-checks.md)
for host preparation and sandbox checks. Do not install into or modify the
shared kit during a sweep.

```bash
browser_args=()
if [ -f "$HOME/.local/chromium-deps/env.sh" ]; then
  . "$HOME/.local/chromium-deps/env.sh"
  browser_args+=(--playwright-module "$PLAYWRIGHT_MODULE"
    --playwright-browsers-path "$PLAYWRIGHT_BROWSERS_PATH")
  if [ -n "${LD_LIBRARY_PATH:-}" ]; then
    browser_args+=(--browser-ld-library-path "$LD_LIBRARY_PATH")
  fi
  if [ -n "${FONTCONFIG_FILE:-}" ]; then
    browser_args+=(--browser-fontconfig-file "$FONTCONFIG_FILE")
  fi
else
  # Prepare the disposable kit first using website-validation.md's rootless
  # Playwright recipe with SANDBOX_ROOT="$PWD/.orbit/tmp/orbit-browser-check".
  browser_args+=(--playwright-module .orbit/tmp/orbit-browser-check/playwright/node_modules/playwright/index.mjs
    --playwright-browsers-path .orbit/tmp/orbit-browser-check/playwright/browsers)
  # On Linux, add --browser-ld-library-path with the extracted libs search
  # path, and --browser-fontconfig-file if a font configuration was prepared.
fi

./scripts/qa-full-sweep.sh --build-candidate --run-commands --website-build \
  --output .orbit/tmp/qa-full-sweep/qa-full-sweep-report.json \
  "${browser_args[@]}" --browser-evidence-dir .orbit/tmp/qa-full-sweep/browser
```

When the host kit is absent, keep the disposable recipe under `.orbit/tmp/`:
follow [website validation](website-validation.md#stage-playwright-and-chromium-without-root)
to install Playwright/Chromium and unpack missing Linux libraries without root.
Pass the resulting paths, for example:

```bash
./scripts/qa-full-sweep.sh --build-candidate --run-commands \
  --output .orbit/tmp/qa-full-sweep/qa-full-sweep-report.json \
  --playwright-module .orbit/tmp/orbit-browser-check/playwright/node_modules/playwright/index.mjs \
  --playwright-browsers-path .orbit/tmp/orbit-browser-check/playwright/browsers \
  --browser-ld-library-path .orbit/tmp/orbit-browser-check/libs/usr/lib/x86_64-linux-gnu \
  --browser-evidence-dir .orbit/tmp/qa-full-sweep/browser
```

Attach both the JSON report and its retained evidence directory (or its file
hash manifest in `results[].retained_evidence`) to the task. If any prepared
path is absent or Chromium cannot launch, the browser scenario remains
`NOT_RUN` rather than passing; retain its capability probe in the report for
operator-owned post-merge verification.

Include `--website-build` for a complete sweep. It prepares dependencies and
builds through the [isolated build-only procedure](website-validation.md#isolated-build-only-sweep)
in a scratch copy of the candidate website and its imported changelog, using
the worker's Node/npm toolchain and a private npm cache. Preparation output,
exit code and build working directory are retained on the website row. A failed
install or build is `FAIL`; omitting the flag remains `NOT_RUN`. Candidate
dependencies and generated files are preserved. This flag never deploys.

Hosted evidence can be combined with `--platform-evidence <report.json>`. A
schema-version 2 full-sweep report must name the exact candidate ID and exact
inventory command. The macOS workflow also uploads a bounded
`macos-platform-evidence-<run>-<attempt>` artifact after all of its required
tests pass. That report records the physical checkout commit, exact required
command, assertion set, workflow/run identity, and hashes of the workflow,
checker, inventory, and importer. Import accepts it only against a clean
checkout of that exact commit;
stale revisions, dirty candidates, wrong commands, missing assertions, failed
or unrun outcomes, incomplete GitHub Actions provenance, and changed source
hashes are rejected. The imported file hash is retained in the combined
report. Each required workflow Cargo command now runs through the sweep's
`--record-native-command` transport, which retains the executed arguments,
checkout revision, exit status and complete output under
`.orbit/tmp/macos-native-execution/`. The emitter and importer both require
all exact workflow commands, nonzero completed test counts, and no skip or
deferral notices. Artifacts emitted by the older listing-only contract lack
that execution record and must be regenerated by the current workflow.
This permits the hosted runner to contribute its actual macOS check
without describing a Linux run as macOS or requiring an executor to access a
manual Mac.

After the candidate lands, an operator must select the successful hosted
`macOS Platform` run for the landed commit, download its evidence artifact
through the authenticated GitHub Actions interface, and run from a clean
checkout of that same commit:

```bash
./scripts/qa-full-sweep.sh --build-candidate --run-commands \
  --platform-evidence /absolute/path/to/macos-platform-evidence.json \
  --output .orbit/tmp/qa-full-sweep/qa-full-sweep-report.json
```

The workflow creates the bounded report with
`./scripts/check-ci-macos.sh --evidence-output macos-platform-evidence.json`.
That emission mode deliberately refuses non-Darwin and non-GitHub-Actions
environments. The ordinary `./scripts/check-ci-macos.sh` command remains the
inventory contract and can be run locally to validate workflow paths and test
filters. On every OS, including Darwin, this listing remains inventory evidence:
the row is `NOT_RUN` until actual matching-candidate native execution is imported.
OS identity and a `matched N tests` line cannot earn native execution assertions. It lists each filter
with `cargo test -p <crate>`, which reuses the macOS job's per-crate builds;
`ci-guardrails.sh` passes `--workspace-build` instead so the Linux `ci` job
lists from the workspace test build its nextest pass already made rather than
compiling the crates a second time under per-crate feature resolution.

## Executed cases and retained evidence

A source-bound `--run-commands` sweep first compiles and lists every exact Rust
selection. `--check --check-cargo-selections` runs the same inventory guard
without executing behavior. The review-evidence fulfilment fixture compiles only
on Linux; other hosts retain that selection as `NOT_RUN` and require matching
Linux execution rather than treating the absent native case as stale. A zero selection or absent required case fails the
guard before sign-off. Listing does not earn the scenario's assertions.

`required_tests` names current executable cases. For consolidated fixtures,
`required_rows` preserves the original behavioral row names: the fixture emits
`qa-scenario-pass: <row>` only after that row's assertions return, and isolated
child fixtures forward those records to the public command output. The sweep
requires both the wrapper's passing test and every required row completion;
an unrelated aggregate pass cannot substitute for them. Commands retain
successful output with `--show-output`. Any `SKIP:`, `DEFERRED:` or `skipping`
notice across built-in, command, Cargo, browser, website or imported execution
makes its coverage `NOT_RUN`, keeps the raw output, and records the missing
capability. Required unavailable coverage makes sign-off `INCOMPLETE`.

Rust execution evidence requires an outer libtest frame for each Cargo target
announcement, complete named result rows, and matching suite counts. Captured
`--show-output` diagnostics belong to an identified outer test and close at the
complete outer successes/failures table. A child summary inside that diagnostic
does not count as an executed target or override its parent's result. The raw
transcript remains intact for behavioral rows and capability notices. Unframed
or interleaved reports, absent targets, incomplete rows, and any failing outer
suite are refused, including a failure before a later successful target.
The native recorder records both requested and executed arguments, replacing
`--nocapture` with `--show-output` to preserve diagnostics in owned blocks.
Prefixes such as `sandbox-exec -p '<profile>'` or `./scripts/build-budget.py --`
are preserved. The recorder inserts or uses the libtest `--` separator after
`cargo test` and its Cargo arguments, independently of any prefix delimiter;
other existing libtest arguments remain intact. For example:

```bash
python3 scripts/test-qa-full-sweep.py \
  --native-execution-dir .orbit/tmp/qa-native \
  --record-native-command ./scripts/build-budget.py -- \
  cargo test -p orbit-common --test test_env --locked
```

Fixture forwarders that bypass libtest capture frame their complete child stdout
with `qa-child-output-begin/end: <owner>` under one stdout lock. Every frame must
close and its owner must appear as a passing outer case. The child bytes remain
in the transcript, and genuine notices still invalidate coverage.
With one libtest thread, the begin marker may follow `test <owner> ... ` on
the same line. The parser retains that pending case prefix across its child
frames and requires the matching outer result after the final frame closes.
The public regression compiles a miniature libtest executable and records its
actual one-thread and two-thread output, including genuine outer failures.
The public recorder/import regression replays the retained hosted `test_env`
transcript, including its intentional failed-child control, alongside contrasting
outer failures. This parser check does not establish native release readiness;
the committed candidate still needs fresh hosted execution and independent review.

The apply-probe negative control injects a refusal without probing Seatbelt.
Its parent asserts the exact named optional skip and strict required failure,
then excludes only that simulated notice from child-output forwarding. Real
child skips and deferrals still reach the command output and invalidate coverage;
the sweep classifier has no exception for test names or injected refusal text.

Retired cases without a verified equivalent stay in `coverage_gaps`; their
scenario remains `BLOCKED` with all original assertions intact. The current
inventory explicitly preserves gaps for the task-eligibility stdio contract,
the pending breaking-switch safe-point contract, and the shipped typed
final-recovery decision contract. Restoring or verifying equivalent coverage
is required before these scenarios can earn sign-off.

Browser producers receive a unique run directory and a separate directory per
scenario. Each row retains its candidate-bound `harness-result.json`, screenshots,
and producer results there. Earlier runs and scenarios are never overwritten.
Before the report's final decision, the sweep reopens every retained reference
and verifies its path and hash; a missing or changed byte fails that row.
`python3 scripts/test-qa-full-sweep.py --self-test` exercises the public sweep
entrypoint with deterministic producers, including a later producer that
corrupts or removes an earlier log. These checks use generated command output,
not source-text matching.

## Capability-bound legs

The inventory is the checklist. Local commands may run automatically; browser,
provider, macOS, Linux-only runtime, and website-build legs require explicit operator capability.
Each must retain its exact command, output/log reference, and PASS, FAIL,
BLOCKED, or NOT_RUN outcome in the attached report. Provider execution must be
bounded in advance and may not recursively dispatch agents or jobs from the
sweep. Browser setup uses the host kit when available and the documented
disposable Playwright recipe otherwise; an
unavailable browser is NOT_RUN, not a clean dashboard result.

The local `npm-package` row runs
`./scripts/smoke-npm-install.sh --local-package-check` from the candidate root.
It checks the Cargo workspace, npm, server and registry-package versions and
identities, then runs `npm pack ./npm --ignore-scripts --offline --json
--pack-destination <candidate>/.orbit/tmp/npm-package-<unique>`.
Lifecycle scripts are disabled, so this builds the proxy archive without
downloading a release binary or publishing. It inspects the actual tarball for
`package.json`, `bin/orbit.js`, the packaged installer at
`./scripts/install-binary.js` inside the tarball (source:
`npm/scripts/install-binary.js` in the checkout),
`release-signing.pub`, `README.md`, and `LICENSE`, checking nonempty files,
candidate contents, identity and the npm-reported file inventory.
The JSON output retains the pack command/output, input hashes, versions, packed
file hashes and archive hash/path. The harness rechecks that evidence against
the candidate before earning either npm assertion; retain and attach the
tarball named by `results[].retained_evidence` alongside the sweep report.
Malformed metadata, version drift or missing/excluded runtime files fail the
row. `python3 scripts/test-qa-full-sweep.py --self-test` exercises these isolated
controls with real local packs. The older `--dry-run-version-assertion` only
tests a narrow version predicate and cannot earn candidate packaging assertions.
The no-argument smoke still exercises the published npm install chain and is
reserved for the existing post-release workflow.

Website deployment and npm publication are user-owned post-release handoffs.
The pre-release sweep validates source builds, packaging, installers, and
dry-run/version contracts but never deploys or publishes. Live website/npm
freshness is reported as `PENDING` and is deliberately excluded from the
pre-release decision, so it cannot create a version/tag cycle. Linux evidence
does not verify the required macOS leg. Post-merge hosted execution and
exact-commit artifact import remain explicit operator verification; a
pre-merge run for an earlier commit is stale by design.

## Sign-off decision and findings

`PASS` is allowed only when every required inventory scenario supplies its
complete, exact assertion set for one candidate. An exit-zero command with
empty or wrong structured output, missing assertions, duplicate mixed-candidate
evidence, a declared capability gap, or any required FAIL, BLOCKED, or NOT_RUN
makes the decision `INCOMPLETE`. Cargo test scenarios also require completed
test-suite summaries with at least one passing test; a renamed filter that
selects zero tests cannot earn the scenario's assertions. The report retains
passing, failing, ignored, and suite counts for these checks.
Reviewed scenarios can declare `required_tests`: each named behavioral case
must appear as passing in the Rust harness output. An unrelated passing test,
or a required case that was skipped, renamed, or filtered out, cannot satisfy
that scenario. The report retains the observed passing case names. Suite and
surface mappings identify the selected boundary checks; they do not establish
that every workflow asset or every subcommand has been executed.
All Cargo rows require named behavioral cases and an explicit `--test` target
or `--lib`. The inventory guard checks targets through `cargo metadata`
without compiling. To compile and list each exact selection, retaining its
selected names without claiming execution, run:

```bash
python3 scripts/test-qa-full-sweep.py --check --check-cargo-selections
```

The consolidated CLI targets are `mcp`, `output`, `process`, `task`, `tool`,
and `workspace`; core runtime and fake-provider cases use `runtime` and
`provider`. MCP case names start with `mcp_roundtrip::`, including nested
`desktop::`, `transport_operations::`, and `internal_drain::` cases. HTTP
boundary checks run in `orbit-web --test http_api`, and workflow boundary
checks run in `orbit-engine --test engine`. Keep filters and `required_tests`
module-qualified, especially when using `--exact`.

A retired case without equivalent admitted boundary coverage stays in the
inventory as `kind: coverage-gap` with a concrete `coverage_gap` explanation
and its original assertions. It has no executable command or passing cases:
the harness emits `BLOCKED`, and even unrelated PASS evidence cannot make it
earn release sign-off. Browser page navigation does not replace HTTP contracts.

`task-list-pagination` runs `cargo test -p orbit-web --test http_api pagination::
-- --nocapture`. Its two required cases exercise both `/api/tasks` and
`/api/tasks/all` through a real server in isolated child processes:

- `pagination::filtered_pages_reach_every_match_and_continue_stably_on_both_endpoints`
  traverses multiple pages with combined status, tag, type, and search filters,
  verifies exhaustive ordered results without duplicates across interleaved
  workspaces, replays continuations, and confirms a newer matching insert does
  not shift an existing continuation but appears on a fresh traversal.
- `pagination::invalid_filter_workspace_and_endpoint_cursors_are_refused_without_state_changes`
  verifies malformed and oversized cursors, changed filters or page size,
  another workspace, the other endpoint, and a different aggregate workspace
  set are refused with a structured HTTP client error. Refusals preserve task
  records, history, comments, artifacts, and valid continuations.

The row retains `pagination-reaches-every-match`, `invalid-cursor-refused`, and
`cross-workspace-cursor-refused`; only passing named cases on the matching
candidate earn those assertions.

The command harness requires a POSIX host. It drains stdout and stderr while
retaining at most one MiB per stream and marks truncated output. A disposable
supervisor owns each command's process group until cleanup finishes. Timeout,
normal command completion, and loss of the QA parent sweep the owned group.
The report records whether cleanup was verified. A refused group signal or
expired reap deadline fails with an explicit unverified cleanup outcome;
timeout and lost-supervisor outcomes also fail.
`python3 scripts/test-qa-full-sweep.py
--self-test` exercises those fail-closed rules. Logs and the JSON report are
task artifacts, not a parallel results store.
Before task handoff, report `make ci-fast`, `make ci-test-affected`,
`make ci-lint`, and `make goldens` as passed, failed, or not run with reasons.
`make ci-fast` runs no Rust tests; the affected-test gate covers complete
test targets of changed crates and their workspace dependents
([validation and CI](../DEVELOPMENT.md#validation-and-ci)). Full `make ci`
runs on PRs.

For a subcommand grammar audit, run
`python3 scripts/test-qa-full-sweep.py --orbit-bin target/debug/orbit --list-cli-paths`.
This traverses executable help in a disposable environment and returns command
paths. Its output explicitly records `behavioral_coverage: false`: reaching help
does not verify command execution, persistence, errors, or every argument.

For direct command-path evidence from the disposable task administration and
run observation fixtures, run
`ORBIT_QA_TRACE_CLI=1 cargo test -p orbit-cli --test task --test process -- task_admin_cli:: run_observation:: --nocapture`.
After a successful JSON command completes, these fixtures emit a `QA_CLI`
record containing its argv, exit code, and named test. Compare those records
with the passing harness cases. Help-only paths and source references inside
other tests remain separate evidence; a passing suite alone does not prove
every discovered path.

For a failure, search open and closed `ws_orbit` tasks using the scenario ID,
exact error, and boundary. Reuse an open task only when its reproducer covers
the same defect. Reassess closed repairs against the current revision. File a
new `qa-full-sweep` task only for a non-duplicate, including the command,
environment, expected and observed result, source revision, and evidence
artifact. Map every finding to its owning task in the report. The sweep stops
there: repairs, nested dispatch, tags, releases, npm publication, and website
deployment are outside its leaf mandate.
