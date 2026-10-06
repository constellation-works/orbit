---
type: runbook
summary: Mint, execute, and judge the workspace-local complete pre-release Orbit QA sign-off.
tags: [operations, qa, release, automation]
paths: [".orbit/auto_tasks/qa-full-sweep.yaml", "scripts/qa-full-sweep*"]
related_features: [auto-tasks, activity-job, dashboard, task-publication]
related_artifacts: [ORB-12010]
last_validated: 2026-10-05
---

# Full pre-release QA sweep

`qa-full-sweep` is the intentional, workspace-local sign-off for Orbit. It is
disabled for periodic scheduling and routes minted work to the `opus` crew.
It complements the recent-change `qa-sweep`; it does not ship to other Orbit
users as a seeded default.

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

For the browser leg, pass all three prepared capability inputs from the
worker's own disposable namespace. The harness forwards only these browser
paths to the dashboard process; it does not inherit the worker's Orbit or
general host environment. The report records the launched Chromium version and
retains screenshots plus `result.json` in the output-side evidence directory.

```bash
./scripts/qa-full-sweep.sh --build-candidate --run-commands \
  --output .orbit/tmp/qa-full-sweep/qa-full-sweep-report.json \
  --playwright-module .orbit/tmp/orbit-browser-check/node_modules/playwright/index.mjs \
  --playwright-browsers-path .orbit/tmp/orbit-browser-check/browsers \
  --browser-ld-library-path .orbit/tmp/orbit-browser-check/sysroot/usr/lib/x86_64-linux-gnu \
  --browser-evidence-dir .orbit/tmp/qa-full-sweep/browser
```

Attach both the JSON report and its retained evidence directory (or its file
hash manifest in `results[].retained_evidence`) to the task. If any prepared
path is absent or Chromium cannot launch, the browser scenario remains
`NOT_RUN` rather than passing; retain its capability probe in the report for
operator-owned post-merge verification.

Add `--website-build` only after preparing the website dependencies through
the isolated procedure in [website validation](website-validation.md). It
authorizes the harness's build check, never deployment.

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
report. This permits the hosted runner to contribute its actual macOS check
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
filters, but on Linux it is not macOS execution evidence. It lists each filter
with `cargo test -p <crate>`, which reuses the macOS job's per-crate builds;
`ci-guardrails.sh` passes `--workspace-build` instead so the Linux `ci` job
lists from the workspace test build its nextest pass already made rather than
compiling the crates a second time under per-crate feature resolution.

## Capability-bound legs

The inventory is the checklist. Local commands may run automatically; browser,
provider, macOS, and website-build legs require explicit operator capability.
Each must retain its exact command, output/log reference, and PASS, FAIL,
BLOCKED, or NOT_RUN outcome in the attached report. Provider execution must be
bounded in advance and may not recursively dispatch agents or jobs from the
sweep. Browser setup may use the documented disposable Playwright recipe; an
unavailable browser is NOT_RUN, not a clean dashboard result.

The local `npm-package` row runs
`./scripts/smoke-npm-install.sh --local-package-check` from the candidate root.
It checks the Cargo workspace, npm, server and registry-package versions and
identities, then runs `npm pack ./npm --ignore-scripts --offline --json
--pack-destination <candidate>/.orbit/tmp/npm-package-<unique>`.
Lifecycle scripts are disabled, so this builds the proxy archive without
downloading a release binary or publishing. It inspects the actual tarball for
`package.json`, `bin/orbit.js`, `scripts/install-binary.js`,
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
