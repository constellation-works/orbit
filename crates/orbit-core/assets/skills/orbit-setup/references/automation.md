# Automation: the host clock, routines, and auto-tasks

How scheduled work happens, and what to turn on. A fresh workspace has the whole
automation layer installed and **switched off** — this is the reference for
opting in deliberately.

## The chain

```text
OS clock unit (every minute)
  └── orbit clock tick                  stateless: what is due on this host?
        ├── routine (.orbit/routines/*.yaml) → job:<name> → normal run
        └── auto-task (.orbit/auto_tasks/*.yaml) → normal task
```

Three things must all be true for a routine to fire:

1. This host has a clock unit installed (or something invokes `orbit clock tick`).
2. The routine's `enabled:` is true.
3. The routine is not paused on this host.

The workspace itself opts in by being registered: the tick loads definitions
from every registered, active checkout on the host, and there is no config key
to set. A **replica** checkout fires only its host-local `worktree-gc` and
`store-gc` routines, and only on a cron trigger. Worktree GC reclaims that
host's claimed-leaf worktrees after asking the owner whether each task
settled. Store GC prunes this host's own store for that workspace (command
audit and resource history are host-wide; run audit and terminal pipeline
state are this workspace's) and does not call the owner. Every other routine,
and a delivery- or state-triggered copy of either job, is listed as
`owner-only` with the owner machine named and never fires there. Auto-tasks
are not evaluated there, and the auto-task panel disables toggle and manual
mint with the owner machine named.

## Turning the scheduler on

**1. Install the host clock.**

```bash
orbit routine init --install-clock
```

Reads the machine identity written by `orbit init` and installs the per-user OS
clock unit that runs `orbit clock tick` every minute — launchd on macOS, a systemd
user timer on Linux. It never creates or rewrites machine identity; `orbit init`
owns that.

```bash
orbit clock status                  # cadence and native manager state
orbit clock pause|enable            # host-wide, without touching definition state
orbit clock set --cadence-seconds 300  # whole-minute cadence, reloads the unit; a paused clock stays paused
```

The clock starts with no login environment. Credentials its runs need (on a Mac,
`CLAUDE_CODE_OAUTH_TOKEN`) come from the owner-only `~/.orbit/clock.env`, which
the install creates; see [macOS](macos.md).

`clock pause` stops scheduled invocation; a manual `orbit clock tick` still works.
`orbit sweep` is a compatibility alias for the same tick and produces the same output.

A tick has a five-minute cooperative deadline shared by discovery, routine
evaluation, auto-tasks, and recovery. An operation already running finishes
before the next boundary checks the budget; remaining workspaces are reported
in `skipped_workspaces`, `deadline_exceeded` is true, and the tick exits nonzero.
Skipped schedules keep their cursors unchanged, so the next tick retries
according to each definition's missed-run policy. Source git and GitHub commands have a shared
30-second pass budget including fetch-lock waits; timeout cleanup kills their
process group, including fetch helpers.

The Linux service adds a ten-minute `TimeoutStartSec` with `KillMode=mixed`
as the hard backstop for an operation that never returns. Keep worker
containment enabled so pipeline runs have their own scopes; workers opting
out remain in the service cgroup and can be killed when that service stops.
`orbit clock status` and `orbit doctor` flag overdue or failed ticks and unsafe
recovery settings. Run `orbit clock repair` to update an older service, even
when it already names the current binary. A paused clock stays paused.

**2. Enable routines, one at a time.** Each is a YAML file in this checkout's
`.orbit/routines/`. `.orbit/` is per-user state that git ignores, so flipping
`enabled: true` is a local edit to that file, not a commit; `orbit routine
pause` is the separate host-local toggle. Registering the
checkout already made the workspace a routine source; an older `.orbit/config.toml`
may still carry a `[routines]` section, which is ignored with a warning and can
be deleted.

<a id="the-nine-seeded-routines"></a>
<a id="the-ten-seeded-routines"></a>
## The eleven seeded routines

`orbit workspace init` seeds all eleven with a workspace-unique name
(`<base>-<workspace>`) resolved at seed time. Ten are seeded disabled;
`crew-performance-refresh` is the one exception, seeded enabled because it
no-ops while `workflow.crew_performance` is `off`. The seven cron routines
resolve nothing else per machine, so two hosts seed identical bytes.
`task-pilot`, `finding-verification`, `pilot-warning-triage` and
`blocked-task-triage` are state-triggered and additionally resolve this host's
machine id as `trigger.state.owner_machine` and the registered base branch as
`trigger.state.branch`; a second host registering the same workspace seeds
itself as owner of its own copy. Run `orbit routine list` to see their names
on this host.

| Base name | Cadence | Target | What it does |
|---|---|---|---|
| `worktree-gc` | hourly | `worktree_gc_pipeline` | Reclaims worktrees whose task settled to done, rejected, or archived, or has been parked in someday for `worktree.parked_reclaim_after_hours` (default 336), and prunes `.orbit/tmp` entries untouched for 24 hours (`scratch_older_than_hours`). |
| `task-pilot` | state trigger (`preparation_eligible`) | `task_pilot_pipeline` | Fingerprints eligible proposed/backlog tasks each tick and preflights a batch whose material has no fresh assessment (seeded `batch_size` 5, mixed crews included); quiet while the backlog is unchanged. |
| `finding-verification` | state trigger (`finding_unverified`) | `finding_verification_pipeline` | Disabled by default; verifies review-filed bug findings still owed a verdict in proposed, someday, or backlog, recording confirmation or rejection with evidence. |
| `pilot-warning-triage` | state trigger (`pilot_warnings_held`) | `escalation_triage_pipeline` | Disabled by default; once per assessment, a hard-tier triager reads each proposed task whose task-pilot assessment holds promotion on warnings and records a decision proposal or the human action that settles the hold. It only proposes: it never approves, promotes, clears a hold or edits the task. Approve with `orbit task update <task-id> --approve --note "<evidence>"`. |
| `blocked-task-triage` | state trigger (`block_escalated`) | `escalation_triage_pipeline` | Disabled by default; once per block episode, a hard-tier triager reads each blocked task whose block final recovery escalated (or that stayed undecided past 72 hours) and requeues, proposes or reports it — [Blocked-task triage](#blocked-task-triage). Replaces the retired `blocked-task-triage` auto-task. |
| `ci-failure-sweep` | hourly at :05 | `ci_failure_sweep_pipeline` | Files deduped proposed CI findings, pilots them, and admits only current warning-free repairs to backlog. |
| `crew-performance-refresh` | hourly at :23 | `crew_performance_refresh_pipeline` | Rebuilds this workspace's crew-performance snapshot (the implementer by-type cell matrix) that performance-weighted crew draws read. The one routine seeded enabled: while `workflow.crew_performance` is `off` (the default) it skips the scan, records `skipped: crew_performance_off` and writes no snapshot; once the feature is on, keeps the snapshot fresh — [Crew performance snapshot](#crew-performance-snapshot). |
| `dependabot-alert-sweep` | daily at 03:25 host-local time | `dependabot_alert_sweep_pipeline` | Collects Dependabot, code-scanning, and secret-scanning findings and files remediation tasks. |
| `store-gc` | daily at 04:17 | `store_gc_pipeline` | Applies `orbit gc audit` and `orbit gc runs`: prunes audit rows older than `retention.audit_days` (this workspace and workspace ids that are no longer registered) and the audit blobs nothing names, and drops the pipeline state of terminal runs older than `retention.runs_days` and the per-minute resource history older than 30 days. Another registered workspace keeps its own window. Review both plans before enabling it — [maintenance.md](maintenance.md#store-retention). On a large store, consider `retention.runs_days = 30` first; smaller windows are refused to preserve 30-day reliability and forecast-accuracy evidence. |
| `task-publication` | weekly Sunday at 04:47 host-local time (`47 4 * * 0`) | `task_publication_pipeline` | Disabled by default because publishing task prose to a remote is an operator decision. Publishes only this owned workspace; unbound workspaces record `skipped: publication_unbound`. Unchanged content is a recorded no-op. Attachments default to `omit` under `publication.routine_attachments`; `include` fails closed without the explicit unscanned opt-in. Replicas report it owner-only and never fire it. Missed runs skip; overlaps are forbidden — [publication.md](publication.md#weekly-publication-routine). |
| `ship-sweep` | every 30m | `workspace_ship_pipeline` | Ships this workspace's ready backlog through the gated pipeline, unattended. The 30-minute cadence leaves 10 minutes of slack past the job's 20-minute drain, so `overlap: forbid` does not skip the next fire. |

### Crew performance snapshot

A crew draw must never run the scorecard scan, so the by-type cell matrix is
persisted as a per-workspace snapshot: one row per `(crew, model, tier, type)`
cell with its raw score, sample count, crew mean and shrunk score, plus the
evidence window, a hash of the type list, and `computed_at`. The seeded
`crew-performance-refresh` routine runs hourly (`cron: "23 * * * *"`, target
`job:crew_performance_refresh_pipeline`) to rebuild it atomically. Run it on
demand with:

```bash
orbit run job crew_performance_refresh_pipeline
```

While `workflow.crew_performance` is `off` (the default) the refresh writes no
snapshot, records `skipped: crew_performance_off`, and skips the scan (the hourly
routine still records a job run). `off` is the only mode that skips the scan;
`absolute`, `comparative`, `shadow_absolute` and `shadow_comparative` all build
snapshots over `workflow.crew_performance_window_days` (default `30`, `1`–`90`).

A stored snapshot is usable only while fresh: a reader gets the typed reason
`missing`, `stale` (older than 24 hours) or `type_list_changed` (built from a
different `workflow.crew_performance_types` list) otherwise; performance-weighted
draws treat any unusable reason as factor 1.

Verify the snapshot with `orbit run readiness`: the output prints one line for
the snapshot (its computed time, window, type list or the unusable reason), and
`orbit run readiness --json` carries the same details under `crew_performance`.

## Built in: final recovery of blocked tasks

This one is not a routine and needs no enablement. On the workspace owner,
every clock sweep looks at tasks that a failed, timed-out or interrupted run,
a failed gate or auto run, or a failed claim settlement left `blocked`. It
dispatches one `blocked_task_recovery_pipeline` run per block, with at most
two at a time. The `final_recovery` agent proposes `complete_no_diff`,
`reject`, `archive`, `requeue` or `escalate`, and Orbit's deterministic
applier acts on it. Each decision is a task comment with the run id. An
escalation names the failed run's worktree and its changed paths when it
still exists, since that is where unfinished work stays.
Every task show prints the last one: local and routed `orbit task show` text
(`Final Recovery:`), `orbit.task.show`, and the dashboard task detail
(`final_recovery`, with `run_id`, `decision`, `outcome` and `at`). A task list
does not. `orbit doctor` lists tasks still
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

### Blocked-task triage

What final recovery escalates stays blocked for a human. The seeded
`blocked-task-triage` routine (disabled until you set `enabled: true` in
`.orbit/routines/blocked_task_triage.yaml`) reads those blocks once each,
shortly after they appear. A block episode — the task's latest move to
`blocked` — awaits triage when its final-recovery decision was an escalation,
or when it has none and is older than the 72 hours after which the backstop
above never decides it. An episode is skipped when a human set the block or
commented, changed the status, attached an artifact or made an unattributed
edit after it, and once triaged; a later block is a new episode. Its
`block_escalated` trigger waits two minutes for an episode to settle, then
submits up to five episodes that share one stored crew into one
`escalation_triage_pipeline` run, whose triager is drawn from
`workflow.hard_complexity_crews`. With nothing waiting it stays quiet.

It replaces the `blocked-task-triage` auto-task, which Orbit no longer seeds.
`orbit workspace sync` removes an unedited copy of that auto-task (with its
settings), keeps a copy you edited, and, when the copy was enabled, tells you
the routine replaces it. It never enables the routine for you.

The triager records exactly one disposition per episode through
`orbit.task.block_triage`, whose `candidates` action lists what qualifies:

- **Mechanical** — a cause with a supported recovery. `requeue` moves the task
  to `backlog` as a final-recovery requeue: it counts against the same bound
  (2 requeues per 24 hours, one count shared with final recovery), and the
  comment records `requeue n of 2 per 24h`. Past the bound it is refused and
  the task stays blocked with the bound named; when the count cannot be read
  nothing is written. An Orbit defect with no supported recovery gets a
  friction record and a report, never a requeue.
- **Decision** — a scope or requirements choice. `propose` writes one comment
  quoting the blocker, with two or three options, their consequences, a
  `target` field (`acceptance_criteria` or `description`) and the ready-to-apply
  replacement text for it, and a recommendation; nothing else on the task
  changes. Answering an option through the decision channel writes that
  replacement as the field's whole new value and returns the task to
  `backlog` in one human update; an option with no target, or one that would
  change nothing, is refused and the task stays blocked.
- **Human-only** — credentials or login, a named owner sign-off, a `security`
  or `security-review` tag, a run another machine executed, or an OS or
  browser-kit requirement. `report` names the human action and changes
  nothing else. The tool itself refuses to requeue a security-tagged task or
  one whose run executed on another machine.

The triager holds no task-update, requeue or run-resume tool and no `--force`
path: it never completes, rejects or archives a task, resumes a run, or merges
or closes a pull request. A recovery that needs one of those is reported with
the exact operator command. To triage one blocked task now, run
`orbit run job escalation_triage_pipeline --input 'subject="block"' --input
'task_ids=["<task-id>"]'`. The dashboard's task details panel offers the
same explicit submission as a **Triage** action — on a blocked task with
`subject="block"`, and on a proposed task held by pilot warnings with the
default `pilot_warning` subject (`orbit run job escalation_triage_pipeline
--input 'task_ids=["<task-id>"]'`). The action is enabled only while the
task currently qualifies and is otherwise disabled with the reason; on
success it links the submitted run, whose triage comment appears on the
refreshed task.

## Built in: re-checking tasks held for a red base

This needs no enablement either. A task whose required command fails on its
base exactly as on the candidate waits in the backlog under a red-base hold,
unless it fails only tests that also fail on the base: that candidate
proceeds, and the run, the task and the PR record the known-red tests. A
before-PR or before-landing review settles the same way for a reviewer's
red-base claim the host confirms.
Every clock sweep on the owner judges those holds from cached base results
only. When a held base ref has moved to a tip nobody has checked, the sweep
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
macOS reviewer, local or claimed, cannot complete
`scripts/codeql-rust-local.sh`, so it holds instead. For each such task the
sweep dispatches one `review_evidence_fulfilment_pipeline` run, one at a time.
The run executes the named command at the held commit and attaches the result,
which queues a fresh review. A failed or incomplete run attaches only its log,
with a typed reason, and the hold stays. Each run needs `min_free_mib` free
(30 GiB by default; override it in a workspace copy of the job) and is audited
as `review.evidence_fulfilment`. It never runs on a follower, inside a claimed
worker, or on another platform. See
[owner fulfilment](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/codeql-local.md#owner-fulfilment).

## Recommended enablement order

Enable in this order and stop wherever the value runs out. Each step is safe
without the ones after it; **the reverse is not true.**

1. **`worktree-gc` first.** It is the only one that reclaims disk, and every
   implementation workflows create runs that can leave worktrees behind.
   Deterministic evidence-filing sweeps do not create worktrees. Turning on
   scheduled shipping without GC is how a workspace fills a disk. Watch one
   cycle with `orbit gc worktrees --dry-run` before enabling. →
   [maintenance.md](maintenance.md)
2. **`task-pilot`.** Its agent inspection is read-only; its apply step writes validated task selectors, and it makes everything downstream safer:
   populated `context_files` are what conflict detection and file reservation
   use to keep parallel runs off each other's files. It fires on task creation
   or a material edit (debounced two minutes, at most ten) rather than on a
   clock, so an unchanged backlog costs nothing. See
   [Tuning which tasks task-pilot prepares](#tuning-which-tasks-task-pilot-prepares).
3. **`ship-sweep` last, and only deliberately.** This is the one that commits,
   pushes, and opens PRs without a human present. Its `enabled: true` is the
   only switch: the routine does not read `workflow.auto_ship`, which gates only
   the cross-workspace `orbit run ship-sweep` command. It never grants
   `--complete`, so shipped work stops in `review`. Do not enable it in the same
   change as anything above; let the earlier ones prove themselves against real
   traffic first.

## Authoring a routine

Only a `job:` target is accepted — an `activity:` target is rejected, so wrap a
single activity in a one-step job.

```yaml
schemaVersion: 1
name: <routine-name>              # unique across every routine source on the host
enabled: true                      # kill-switch in the definition file
trigger:
  cron: "0 22 * * *"              # 5-field, host-local time
  missed_run: skip                 # skip | catch_up_once
target: job:<job-name>
policy:
  timeout_minutes: 10
  retries: { max: 2, backoff_minutes: 2 }
  overlap: forbid                  # forbid | allow
```

Parsing is fail-closed: an invalid file — bad schema version, unknown field,
unresolvable target, unparsable cron — makes *that routine* absent and reports a
load error. It never fires with defaults.

Use `orbit routine show <name>` for a complete installed example and effective
state; `orbit routine --help` lists the management commands. Keep the schema
version and field names from the installed definition when authoring one.

### Changing a routine's trigger

An operator can replace a routine's `trigger:` block without opening the file:
the dashboard's **Automation › Routines** row has an **edit** link beside the
cadence, and `orbit.routine.control` takes `action: "set_trigger"` with the
observed `target` and `expected_trigger` and the replacement `trigger`. Both run
the same write: it refuses a target or trigger that changed since you read it,
checks the new trigger as the loader would (exactly one of `cron`, `state` or
`deliveries_landed`; a cron that parses; a state kind whose job is the routine's
target; `overlap: forbid` for state and delivery triggers), and rewrites only
the `trigger:` block. Comments inside that block are not kept; the rest of the
file is. Name, target, policy and `enabled` are not editable this way. Editing a
seeded routine makes it a local edit: `orbit workspace sync` keeps your file and
stops refreshing it from Orbit's template.

## Routines a plugin seeds

An installed plugin may ship routines. `orbit plugin enable <ns>` seeds each
one as `.orbit/routines/<ns>-<name>.yaml` with `enabled: false` and a
`# provenance: plugin:<ns>@<version>` header — the plugin never ships a
schedule that is already on, and a routine it ships may target only a job the
same plugin ships or a shipped default. Turning one on is the ordinary
reviewed edit described above.

An upgrade re-seeds a seeded file only while it still matches what the plugin
wrote; a file you edited is preserved with a warning until
`orbit plugin enable <ns> --force` overwrites it. Disabling or removing the
plugin — on the host, or in one workspace with `--scope workspace` — leaves the
file where it is: the clock tick skips it, never as a load error, and
`orbit routine list` and the dashboard hide it. `orbit routine list --all`
lists it as `inactive` with a reason naming the plugin, and `orbit routine show
<name>` still resolves it. Re-enabling brings it back with no re-seed.

## Tuning which tasks task-pilot prepares

The seeded `task_pilot.yaml` carries an optional `eligibility` block under
`trigger.state`. Every key is optional; the seeded values are the defaults,
and a definition without the block behaves identically:

```yaml
trigger:
  state:
    kind: preparation_eligible
    owner_machine: hm_...            # resolved at seed time: this host
    branch: main                     # resolved at seed time: the registered base branch
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    batch_size: 5                    # due tasks admitted per run (1..=min(50, max_items))
    retries: 1
    deadline_minutes: 90
    eligibility:
      statuses: [proposed, backlog]  # non-empty subset of these two
      exclude_tags: [no-diff-expected, no-diff-needed]
      require_tags: []               # every listed tag must be present
      task_types: []                 # empty admits every type
```

`batch_size` is how many due tasks one tick admits into a single
`task_pilot_pipeline` run. Tasks whose stored `task.crew` differs, including
set versus unset, share that run; prepare gives each crew its own partitions
of at most five, so one pass assesses every crew instead of refusing a mixed
bundle. A hand-assembled `orbit run task-pilot` list that mixes crews is still
refused at run start. A burst of same-crew filed tasks is prepared as one run
rather than one run per task, and
`orbit clock tick --dry-run` lists the batch it would admit. Narrow the
predicate to, say, `statuses: [backlog]` or `require_tags: [pilot]` to keep the
scheduled pilot off proposals a human has not looked at yet. Unknown keys and
an empty `statuses` fail the definition closed. The resolved predicate is part
of each task's material fingerprint, so changing it re-prepares tasks assessed
under the old one; the default predicate keeps the fingerprints a workspace
accepted before the block existed. Explicit runs (`orbit run task-pilot <id>`)
ignore the block.

An active pilot temporarily holds its tasks across routines, drains, manual
runs and targeted CI-sweep pilots, from the reservation its prepare step writes
until the run ends; of two pilots started together for one task, only one does
the agent work. The state trigger withholds held tasks as
`already_preparing: <run id>`; editing a held task does not start another
pilot. An explicit run skips held tasks, records `already_preparing`
exclusions with `prepared_by_run_ids`, settles them as `piloted_elsewhere`,
and prepares the free tasks. An all-held run succeeds without agent work. A
claimed task edited before its batch is prepared is claimed afresh on its own;
the rest of the batch is piloted. Inspect
`orbit run show <RUN_ID> -s prepare --json` to find the holder.

Which edits make an assessed task due again is `[workflow.task_pilot_freshness]`
in `config.toml`. By default only a change to the title, description,
acceptance criteria, plan or selectors does; retagging, reassigning crew, and
merges to the branch do not. Add fields to `material_fields` (`tags`, `crew`,
`tools`, `type`, `complexity`, `relations`, `dependencies`, `instructions`) or
set `source_sensitivity` to `context_files` (a merge that touches a task's
selector paths) or `any` (every merge). A `freshness:` block under the
routine's `trigger.state` overrides either key for that routine. Changing the
resolved value re-prepares tasks assessed under the old one.

**Upgrading from the cron form.** A workspace seeded before the state form
holds a cron `task_pilot.yaml` (`*/40 * * * *`). `orbit workspace sync` refreshes
an unmodified one — or one whose only edit is `enabled: true` — onto the state
form owned by this host, keeping the opt-in (`orbit workspace sync --check`
previews it). A cron file whose cadence or policy was hand-edited is left in
place and reported as `preserved`; either restore the shipped bytes and rerun
the sync, or edit it to the state form above by hand.

## Verify without firing

```bash
orbit routine list                 # every routine: enabled / paused, next due
orbit routine show <name>          # definition, effective state, recent fires
orbit clock tick --dry-run         # what would fire; writes nothing
orbit clock tick --dry-run --verbose  # include not-due rows
orbit --workspace <name> clock tick --dry-run  # restrict to one workspace
```

The global `--workspace` selector narrows a sweep — dry-run or live — to one
registered workspace's routines; without it the pass covers every registered
checkout on the host. A replica checkout contributes only its cron worktree-GC
and store-GC routines.

## Observe and control

Fires are ordinary runs — they appear in `orbit run history` under the actor
`routine/<name>`.

```bash
orbit routine pause <name>         # this host only; survives reboots
orbit routine resume <name>
```

## "Why didn't it fire?"

Resolve the toggles in this order — `orbit routine list` shows both at once:

1. `enabled: false` in this checkout's definition (per-user, gitignored).
2. A local pause (this host only, unversioned, durable across reboots).

If neither explains it, check further out: is this checkout registered as an
owner (`orbit workspace list`), is the clock unit running
(`orbit clock status`), and did the tick itself error
(`orbit log tail --level warn --since 1h`)? For a fire
that started and then failed, the run is the evidence —
[run-debugging.md](../../orbit-orchestrate/references/run-debugging.md).

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
auto-drain cannot claim them. Invoking this named job, or enabling its shipped
inert routine, is explicit promotion authorization; the filing activity and a
standalone `task_pilot_pipeline` run carry no such authority. Set the CI
integration branch explicitly when it differs from GitHub's default branch.
CI defaults bound investigation to six runs and filing to five tasks. The
security job defaults to `moderate` and above for Dependabot/code-scanning
findings. Set `[security_alert_sweep] min_severity = "high"` in the workspace
`.orbit/config.toml` or global `~/.orbit/config.toml` to change the floor for
scheduled routines and manual runs. Allowed values are `low`, `moderate`,
`high` and `critical`; precedence is explicit run/job `min_severity` input >
workspace config > global config > built-in `moderate`. The `--input
min_severity=high` example overrides config for that run only. Invalid config
values fail validation with `security_alert_sweep.min_severity` named in the
error. Secret-scanning findings are always considered regardless of this floor;
dependency alerts with an open Dependabot PR are skipped by default. Its catalog
exposes per-source collection caps. The file step records the effective
`min_severity`, `min_severity_source` (`input`, `workspace`, `global` or
`built-in`), and `excluded_below_min_severity`. `orbit run show <RUN_ID>`
summarizes the filed count, floor and source, and excluded alert count and
numbers, including for a successful sweep.
Secret values must not be copied into task prose or logs.

A missing GitHub client, authentication, or API permission is a capability gap,
not evidence of a clean repository. Read the collect/file step outcomes. CI
filing honors exact-key operator `covered_by` relations and timed archive/reject
suppression (`ci_failure.operator_suppression_hours`, default 6; see
[recovery](../../orbit-orchestrate/references/recovery.md)). Equal normalized
failing-test signatures consolidate across jobs with every job retained.
Otherwise filing first reuses a still-open `ci-failure:<key>` owner, then a rejected
exact-key task whose comment names one still-open covering owner, then a
high-confidence material match (generated workflow/job/step labels, or a
specific error together with the failing command or the same run and job).
That is not fuzzy title similarity: a shared workflow, generic npx/cargo
exit, or the same affected file is not enough, and completion alone
does not suppress a later recurrence. Completed owners can cover an exact pre-fix
observation only after the bounded structured reassessment described in
[workflows.md](../../orbit-orchestrate/references/workflows.md#completed-ci-repair-reassessment). Coverage retains
source and validation references on the existing owner and creates no new pilot
candidate. Missing or contradictory proof is explicitly unresolved. Discovery,
filing, and dedupe-lookup errors must remain visible and retryable. A previous done repair is evidence
to inspect, not blanket dedupe: an old failed release already fixed on the
current integration branch stays proposed as already-landed, while a distinct
defect that still reproduces at the current revision remains eligible. Choose
these routines independently of `ship-sweep`; admission to backlog does not
authorize implementation or completion.
