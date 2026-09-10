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
./scripts/qa-full-sweep.sh --orbit-bin "$(command -v orbit)" \
  --output qa-full-sweep-report.json --run-commands
orbit task artifact put <TASK_ID> qa-full-sweep-report.json \
  --path qa-full-sweep-report.json --model claude --json
```

Use the exact release candidate binary. The report pins its resolved path and
SHA-256 separately from the source HEAD, so a stale installed binary cannot be
mistaken for the checkout under test. The harness constructs a clean child
environment and disposable Orbit root and Git repositories for mutations.

For the browser leg, pass the absolute Playwright module installed in the
worker's own disposable namespace:

```bash
./scripts/qa-full-sweep.sh --run-commands \
  --playwright-module /tmp/orbit-browser-check/node_modules/playwright/index.mjs
```

Add `--website-build` only after preparing the website dependencies through
the isolated procedure in [website validation](website-validation.md). It
authorizes the harness's build check, never deployment.

## Capability-bound legs

The inventory is the checklist. Local commands may run automatically; browser,
provider, macOS, and website-build legs require explicit operator capability.
Each must retain its exact command, output/log reference, and PASS, FAIL,
BLOCKED, or NOT_RUN outcome in the attached report. Provider execution must be
bounded in advance and may not recursively dispatch agents or jobs from the
sweep. Browser setup may use the documented disposable Playwright recipe; an
unavailable browser is NOT_RUN, not a clean dashboard result.

Website deployment and npm publication are user-owned. The sweep validates
source builds, packaging, installers, and dry-run/version contracts but never
deploys or publishes. Before publication, live website/npm freshness is
pending and full sign-off remains incomplete. Linux evidence does not verify
the required macOS leg.

## Sign-off decision and findings

`PASS` is allowed only when every required inventory scenario has current PASS
evidence for the same source revision and binary. Any required FAIL, BLOCKED,
or NOT_RUN result makes the decision `INCOMPLETE`. Logs and the JSON report are
task artifacts, not a parallel results store.

For a failure, search open and closed `ws_orbit` tasks using the scenario ID,
exact error, and boundary. Reuse an open task only when its reproducer covers
the same defect. Reassess closed repairs against the current revision. File a
new `qa-full-sweep` task only for a non-duplicate, including the command,
environment, expected and observed result, source revision, and evidence
artifact. Map every finding to its owning task in the report. The sweep stops
there: repairs, nested dispatch, tags, releases, npm publication, and website
deployment are outside its leaf mandate.
