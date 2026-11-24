# Automation: built-in recovery routines and GitHub evidence sweeps

## Contents

- [Built in: final recovery of blocked tasks](#built-in-final-recovery-of-blocked-tasks)
  - [What a block gets: mechanical steps, a recheck, then a decision card](#what-a-block-gets-mechanical-steps-a-recheck-then-a-decision-card)
- [Built in: re-checking tasks held for a red base](#built-in-re-checking-tasks-held-for-a-red-base)
- [Built in: Linux CodeQL evidence for held reviews](#built-in-linux-codeql-evidence-for-held-reviews)
- [GitHub evidence sweeps](#github-evidence-sweeps)

## Built in: final recovery of blocked tasks

This one is not a routine and needs no enablement. On the workspace owner,
every clock sweep looks at tasks that a failed, timed-out or interrupted run,
a failed gate or auto run, or a failed claim settlement left `blocked`. It
dispatches one `blocked_task_recovery_pipeline` run per block, with at most
two at a time. The `final_recovery` agent proposes `complete_no_diff`,
`reject`, `archive`, `requeue` or `escalate`, and Orbit's deterministic
applier acts on it.

Each decision is a task comment with the run id. An
escalation names the failed run's worktree and its changed paths when it
still exists, since that is where unfinished work stays.
Every task show prints the last one: local and routed `orbit task show` text
(`Final Recovery:`), `orbit.task.show`, and the dashboard task detail
(`final_recovery`, with `run_id`, `decision`, `outcome` and `at`). A task list
does not.

`orbit doctor` lists tasks still
blocked after one in its `blocked-task-recovery` row.

It leaves a block alone when someone other than Orbit's automation has
commented on the task, changed its status or attached an artifact since the
block, when the block already has a decision, and when the block is older
than 72 hours. A field-only edit (description, plan, selectors) records no
actor, so any such edit after the block also leaves it to a human; `orbit
doctor` names those tasks. It never runs on a follower or inside a claimed
worker.

The crew comes from `workflow.purpose_crews.final_recovery`. To opt out, set
it to `[]`; this also turns off final recovery inside the delivery pipelines.

### What a block gets: mechanical steps, a recheck, then a decision card

Final recovery is the single decider for a blocked task; there is no separate
triage pass. Before it escalates it runs bounded mechanical steps, each at
most once per head or run and each recorded in the task history:

- `resume_run` resumes the failed run from its checkpoints (once per run);
- `rerun_ci` re-requests the failed required checks of the task's pull request
  (once per head);
- `requeue` moves the task to `backlog`; it counts against the bound of 2
  requeues per 24 hours that every requeue, resume and CI rerun shares. Past
  the bound, or when the count cannot be read, it is refused and the task
  stays blocked;
- `complete_no_diff` closes a task whose work is already on the base;
- `friction_reported` links or files the friction record for an Orbit defect
  with no supported recovery and leaves the task blocked.

A refused step escalates and the comment reads `<step> refused: <why>`.

The backstop tick also runs a deterministic recheck with no agent. It settles
a blocked task whose pull request merged outside Orbit (the task follows the
merge to `done`), whose failing required checks were recorded as inherited from
the base and now pass (back to `backlog`), or whose infrastructure block
cleared, and records why in a `blocked_task_rechecked` history entry.

What none of that settles, final recovery either escalates or decides:

- **`escalate`** names the diagnosis and the one human action. The task stays
  blocked and its dashboard card shows the summary and that action.
- **`decide`** opens a decision request for a scope or requirements choice: a
  short summary, two or three options and a recommendation. Each option has a
  summary, a consequence, an optional field rewrite (`description`,
  `acceptance_criteria`) and an optional action (`requeue`, `re_pilot`,
  `change_crew`, `split`, `reject`, `archive`). An option with neither a
  rewrite nor an action, or with an unknown action, is refused.

A human answers a card from the dashboard, `orbit decision answer` or the
decision channel. The answer records a comment naming the option and note,
applies the rewrite, runs the action, moves the task out of `blocked` and closes
the request in one operation. An answer attributed to an agent or other
automation is refused.

A proposed task held by pilot warnings gets the same
card from pilot-warning triage (approve, re-pilot or rewrite), started from the
dashboard's Triage action; that triager only proposes and never approves, promotes or edits the task itself.

The seeded `blocked-task-triage` routine and its `block_escalated` trigger are
retired. `orbit workspace sync` removes an unedited copy; an existing
`blocked-task-triage-*` routine file loads inert and is reported as retired.
`orbit.task.block_triage` (`orbit tool run orbit.task.block_triage`, or MCP
with `--toolset task-triage`) now serves pilot-warning triage only, and
`subject="block"` is refused with a message naming final recovery. Old
`blocked_task_triaged` history entries still render.

## Built in: re-checking tasks held for a red base

This needs no enablement either. A task whose required command fails on its
base exactly as on the candidate waits in the backlog under a red-base hold,
unless it fails only tests that also fail on the base: that candidate
proceeds, and the run, the task and the PR record the known-red tests. A
before-PR or before-landing review settles the same way for a reviewer's
red-base claim the host confirms.
Every clock sweep on the owner judges those holds from cached base results
only.

When a held base ref has moved to a tip nobody has checked, the sweep
dispatches one `baseline_hold_refresh_pipeline` run, at most one per workspace
at a time. That run executes the held command on the new tip and records the
verdict; the hold lifts only when the command passes. The sweep never runs the
command itself, so a long check never makes later ticks report `lock_busy`.

The same sweep files one `baseline-red-repair` task per red base failure (a
command and the tests it fails on the base) at complexity `medium` and links each
later task held for the same failures to it, so the red base has an owner
without anyone reading worker logs.
Its crew comes from `workflow.purpose_crews.baseline_red_repair` when that is
set, else the `medium` complexity pool.
It never runs on a follower or inside a claimed worker.

## Built in: Linux CodeQL evidence for held reviews

This also needs no enablement. On a Linux owner, every clock sweep looks for
in-progress tasks whose before-PR review is held only for a `codeql` result. A
macOS reviewer, local or claimed, cannot complete that check, so it holds
instead. The built-in fulfilment runs only `scripts/codeql-rust-local.sh`
with optional `--ram <mib>` and `--toolchain <version>` and exactly one query
selector.

Download the script from
<https://raw.githubusercontent.com/constellation-works/orbit/v0.54.0/scripts/codeql-rust-local.sh>
and save it at `scripts/codeql-rust-local.sh` in the workspace, then make it
executable (`chmod +x scripts/codeql-rust-local.sh`, committed as mode 100755):
the fulfilment executes that path directly, so a copy saved without the
executable bit cannot run. Its default extractor toolchain is Rust 1.99.0 plus
rust-src, confirmed for CodeQL 2.27.5.

The script also exits 1 with `missing CodeQL configuration` unless the checkout
holds `.github/codeql/codeql-config.yml`, and the fulfilment runs the held
commit's checkout, so commit that file with the script; one that exists only on
the owner machine does not count. A minimal compatible file:

```yaml
name: orbit-codeql
paths-ignore:
  - "**/tests/**"
  - ".orbit/**"
  - "**/target/**"
```

Keep `paths-ignore` one top-level block list (a flow list is refused) with
`.orbit/**` and `**/target/**` in it.

A custom
command is not fulfilled by the built-in path: the run attaches its log with
that refusal and the hold stays.

For each such task the sweep dispatches one
`review_evidence_fulfilment_pipeline` run, one at a time. The run executes an
admitted command at the held commit and attaches the result, which queues
a fresh review. A failed or incomplete run attaches only its log, with a typed
reason, and the hold stays. Each run needs `min_free_mib` free
(30 GiB by default; override it in a workspace copy of the job) and is audited
as `review.evidence_fulfilment`.

It never runs on a follower, inside a claimed
worker, or on another platform. See
[owner fulfilment](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/codeql-local.md#owner-fulfilment).

## GitHub evidence sweeps

These two routines are optional alternatives to agent-authored recurring reviews.
They collect bounded evidence on the host and deterministically file tasks;
they do not launch repair agents or ship the tasks they create.

```bash
orbit job show ci_failure_sweep_pipeline
orbit run job ci_failure_sweep_pipeline --input integration_branch=<branch> --input max_tasks=5
orbit job show dependabot_alert_sweep_pipeline
orbit run job dependabot_alert_sweep_pipeline --input max_tasks=10
# Override the configured floor for one run:
orbit run job dependabot_alert_sweep_pipeline --input min_severity=high --input max_tasks=10
```

These commands can create and pilot proposed tasks; they are not dry runs.
The CI job promotes only tasks whose pilot applied non-empty canonical
selectors and reported no duplicate, already-landed, conflict, or warning
finding. Pilot failure and stale/no-diff findings remain proposed, so an active
auto-drain cannot claim them.

Invoking this named job, or enabling its shipped
inert routine, is explicit promotion authorization; the filing activity and a
standalone `task_pilot_pipeline` run carry no such authority. Set the CI
integration branch explicitly when it differs from GitHub's default branch.
CI defaults bound investigation to six runs and filing to five tasks. The
security job defaults to `moderate` and above for Dependabot/code-scanning
findings; the floor is fixed.

A run's `--input min_severity=high` (`low`,
`moderate`, `high` or `critical`) overrides it for that run only.
`security_alert_sweep.min_severity` loads with a deprecation warning, is
ignored, and stops being recognised after 2026-12-08. Secret-scanning findings are always considered regardless of this floor;
dependency alerts with an open Dependabot PR are skipped by default. Its catalog
exposes per-source collection caps.

The file step records the effective
`min_severity`, `min_severity_source` (`input` or `built-in`; runs recorded
before the fixed floor may name `workspace` or `global`), and `excluded_below_min_severity`. `orbit run show <RUN_ID>`
summarizes the filed count, floor and source, and excluded alert count and
numbers, including for a successful sweep.

A CodeQL alert whose rule the owner host's installed CodeQL bundle does not
offer (or any CodeQL alert when no `codeql` answers on the validation PATH) is
filed depending on one proposed `CodeQL bundle <version> needed on <host>`
prerequisite, with an open drift friction record that attention lists; install
the bundle on that host and mark the prerequisite done to release the repairs.
Secret values must not be copied into task prose or logs.

A missing GitHub client, authentication, or API permission is a capability gap,
not evidence of a clean repository. Read the collect/file step outcomes. CI
filing honors operator `covered_by` relations and timed archive/reject
suppression of the same evidenced cause
(a fixed 6 hours; see
[recovery](../../orbit-orchestrate/references/recovery-ci-findings.md)). Equal normalized
failing-test signatures consolidate across jobs with every job retained.

A job whose log has a line starting with an entry of
`workflow.ci_infrastructure_signatures` is listed under `infrastructure`, not
filed, and on a pull request it gets a CI rerun rather than a repair agent. The
sweep itself reruns nothing, so a failed landing-branch run stays red until a
rerun or a newer push.

That rerun is bounded by
[CI infrastructure reruns](configuration-recovery.md#ci-infrastructure-reruns):
`workflow.ci_infrastructure_retry.max_attempts` (default `3`, `0`..`10`;
`0` disables the recovery) caps reruns per pull-request head, and
`workflow.ci_infrastructure_retry.backoff_seconds` (default `60`,
`1`..`3600` seconds) sets the initial delay before doubling waits. Reading
the sweep output or this policy grants no repair or merge authority.

Otherwise filing first reuses a still-open `ci-failure:<key>` owner, then follows
a rejected exact-key task's inbound `Supersedes` relation to its covering owner,
preferring a still-open owner and then the lowest task ID if several supersede
the source. Any inbound relation suppresses the legacy comment reader, even
when its superseder is terminal; a terminal superseder does not fall through to
a legacy comment. Covering-task comments are read only when no inbound
`Supersedes` relation exists.

Filing then tries a high-confidence material
match (generated workflow/job/step labels, or a specific error together with
the failing command or the same run and job).
That is not fuzzy title similarity: a shared workflow, generic npx/cargo
exit, or the same affected file is not enough, and completion alone
does not suppress a later recurrence.

Completed owners can cover an exact pre-fix
observation only after the bounded structured reassessment described in
[workflows-ci-reassessment.md](../../orbit-orchestrate/references/workflows-ci-reassessment.md#completed-ci-repair-reassessment). Coverage retains
source and validation references on the existing owner and creates no new pilot
candidate. Missing or contradictory proof is explicitly unresolved. Discovery,
filing, and dedupe-lookup errors must remain visible and retryable.

A previous done repair is evidence
to inspect, not blanket dedupe: an old failed release already fixed on the
current integration branch stays proposed as already-landed, while a distinct
defect that still reproduces at the current revision remains eligible. Choose
these routines independently of how you dispatch; admission to backlog does
not authorize implementation or completion.
