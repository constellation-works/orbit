---
type: runbook
summary: Mint, execute, and judge the workspace-local complete pre-release Orbit QA sign-off.
tags: [operations, qa, release, automation]
paths: [".orbit/auto_tasks/qa-full-sweep.yaml", "scripts/qa-full-sweep*"]
related_features: [auto-tasks, activity-job, dashboard, task-publication]
related_artifacts: [ORB-12010]
last_validated: 2026-09-10
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
  --output qa-full-sweep-report.json --run-commands
orbit task artifact put <TASK_ID> qa-full-sweep-report.json \
  --path qa-full-sweep-report.json --model claude --json
```

`--build-candidate` compiles the executable from the checkout into a disposable
target directory. The harness snapshots HEAD, the tracked diff, and untracked
inputs before the build, verifies that identity is unchanged after every local
scenario, and binds every result to the resulting candidate ID. Supplying an
installed binary without that build attestation intentionally makes provenance
FAIL, even when `--version` exits zero. The harness constructs a clean child
environment and disposable Orbit root and Git repositories for mutations.

For the browser leg, pass all three prepared capability inputs from the
worker's own disposable namespace. The harness forwards only these browser
paths to the dashboard process; it does not inherit the worker's Orbit or
general host environment. The report records the launched Chromium version and
retains screenshots plus `result.json` in the output-side evidence directory.

```bash
./scripts/qa-full-sweep.sh --build-candidate --run-commands \
  --playwright-module /tmp/orbit-browser-check/node_modules/playwright/index.mjs \
  --playwright-browsers-path /tmp/orbit-browser-check/browsers \
  --browser-ld-library-path /tmp/orbit-browser-check/sysroot/usr/lib/x86_64-linux-gnu \
  --browser-evidence-dir /tmp/orbit-qa-evidence/browser
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
  --output qa-full-sweep-report.json
```

The workflow creates the bounded report with
`./scripts/check-ci-macos.sh --evidence-output macos-platform-evidence.json`.
That emission mode deliberately refuses non-Darwin and non-GitHub-Actions
environments. The ordinary `./scripts/check-ci-macos.sh` command remains the
inventory contract and can be run locally to validate workflow paths and test
filters, but on Linux it is not macOS execution evidence.

## Capability-bound legs

The inventory is the checklist. Local commands may run automatically; browser,
provider, macOS, and website-build legs require explicit operator capability.
Each must retain its exact command, output/log reference, and PASS, FAIL,
BLOCKED, or NOT_RUN outcome in the attached report. Provider execution must be
bounded in advance and may not recursively dispatch agents or jobs from the
sweep. Browser setup may use the documented disposable Playwright recipe; an
unavailable browser is NOT_RUN, not a clean dashboard result.

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
makes the decision `INCOMPLETE`. `python3 scripts/test-qa-full-sweep.py
--self-test` exercises those fail-closed rules. Logs and the JSON report are
task artifacts, not a parallel results store.

For a failure, search open and closed `ws_orbit` tasks using the scenario ID,
exact error, and boundary. Reuse an open task only when its reproducer covers
the same defect. Reassess closed repairs against the current revision. File a
new `qa-full-sweep` task only for a non-duplicate, including the command,
environment, expected and observed result, source revision, and evidence
artifact. Map every finding to its owning task in the report. The sweep stops
there: repairs, nested dispatch, tags, releases, npm publication, and website
deployment are outside its leaf mandate.
