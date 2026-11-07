# Release-readiness examples

Disabled examples of how this workspace can prove a release is ready through the
generic `completion` contract of an auto-task job step. Nothing here runs by being
in the repository: Orbit does not seed these files, and they are not in either
embedded registry. Installing them is the workspace owner's decision.

| File | What it is |
| --- | --- |
| `auto_tasks/security-review.yaml` | The current security review, disabled, plus its evidence contract. |
| `auto_tasks/release-prep.yaml` | The current report-first preparation probe, disabled, plus its evidence contract. |
| `examples/release-readiness/scripts/release-readiness.yaml` | Script manifest that registers the adapter. |
| [`scripts/release-readiness-adapter.py`](../../scripts/release-readiness-adapter.py) | The workspace's QA, security and preparation policy. Stays at the repository's `scripts/`. |

There is deliberately no `full-qa-sweep.yaml`. Full QA is this workspace's existing
`.orbit/auto_tasks/qa-full-sweep.yaml`, whose harness, inventory, supplemental
forecast scenarios and platform requirements stay as they are. Adding a second
definition would split that sign-off, so the owner adds one block to the native file
(step 3). The job and routine that compose these steps are separate examples
(ORB-20166).

## Installing, when the owner decides to

1. Copy `examples/release-readiness/scripts/release-readiness.yaml` to
   `.orbit/resources/scripts/release-readiness.yaml` and grant it:
   `orbit script grant release-readiness`. The grant binds the manifest and adapter
   digests, so changing either makes the grant stale and the step holds until the owner
   grants it again. Orbit never widens a grant by itself.
2. Copy the two definitions to `.orbit/auto_tasks/`. The names replace the installed
   `security-review` and `release-prep` definitions, so compare them first and keep any
   owner edits. `orbit auto-task show` then reports each as forked from the bundled
   default, which is expected. Enable each one deliberately with `orbit auto-task toggle`.
3. Add the block below to the native `qa-full-sweep.yaml`, leaving everything else as it
   is. A definition's digest changes when its body does, so a running job keeps the
   frozen definition it started with:

   ```yaml
   completion:
     contract: release-readiness/v1
     adapter: release-readiness
     form: qa-full-sweep
     evidence_artifact: readiness-evidence.json
     max_age_minutes: 1440
     evidence_instructions: |
       List these in the evidence manifest, with every file they cite:

       - `qa-full-sweep-report.json`: the harness report for the clean pinned candidate.
       - `forecast-history-qa.json`: `candidate_id`, `source_revision` and `scenarios`, each
         with `id`, `outcome` and `evidence`, covering `legacy-history`, `forecast-contract`,
         `rendered-behavior` and `historical-evidence`.
       - `capability-gap-evidence.json`: `candidate_id`, `source_revision` and `surfaces`,
         giving each required capability gap in the inventory an `outcome` and `evidence`.

       macOS evidence comes from the native macOS check of this revision. Linux evidence
       never stands in for it. Record what is missing rather than filling it in: a gap
       leaves the step unavailable.
   ```

4. Bind each step in a job with `require_result: release-readiness/v1`, for example
   `target: auto-task:qa-full-sweep`. The step ID (such as `full-qa-sweep`) is a display
   name and may differ from the definition name. The target must name the real definition.

## What counts

The owner, not the run, supplies the task, run, definition digest, source revision and
completion time. A run's `readiness-evidence.json` must match them, and may not state a
time or status of its own. The adapter then judges the evidence against this workspace's
policy:

| Form | Passed | Blocked | Unavailable |
| --- | --- | --- | --- |
| `qa-full-sweep` | Every required scenario, forecast scenario and capability gap has passing evidence for this clean candidate. | | Anything missing, stale, dirty, failed or run against another inventory. |
| `security-review` | All four areas have cited evidence, and each spawned finding is reported. | An open high or critical finding. | A missing scope, an omitted finding, or a finding that is unlinked or does not exist. |
| `release-prep` | The actual open canonical `Prepare v<X.Y.Z> release` task, selected deterministically and read back with the commit survey. | A busy-board or nothing-to-release probe. | No canonical task, or one that does not match the survey. |

A probe that found a busy board or nothing to release is still a successful task. It is
not accepted preparation: it holds the step and adds nothing to accepted-result history.
The probe keeps its `auto-task:release-prep` exemption and single canonical task, and it
sets no version, tag or release.

## When a step stays unavailable

A step holds, and times out at its expiry, until each of these is in place:

- the definition exists, is enabled and declares the contract the step requires;
- the adapter manifest is installed with a current grant;
- this host can start a script sandbox. A process already inside a sandbox cannot, so an
  agent run does not exercise the adapter launch;
- `python3` and `scripts/qa-full-sweep-inventory.json` are in the checkout;
- the required Mac, Linux, browser and other capability evidence exists for this revision;
- accepted-result history is readable. Orbit reports a corrupt file rather than treating
  it as empty, and leaves it in place for inspection.

Check the state, without changing it, with `orbit automation explain job:<name> --json`.
