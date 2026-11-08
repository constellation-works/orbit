# Automation: the host clock, routines, and auto-tasks

How scheduled work happens, and what to turn on. A fresh workspace carries the
defaults selected by its [seeding policy](configuration.md#seeding-policy).
Scheduled work requires a host clock and enabled definitions.
This is the reference for opting in deliberately.

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

**2. Enable routines, one at a time, as an operator.** Each routine is a YAML
file in this checkout's `.orbit/routines/`. Set its `enabled` field with the
audited verbs rather than by editing the file:

```bash
orbit routine list                          # rendered names, enabled and paused state
orbit routine enable <routine-name>         # operator only: sets enabled: true
orbit routine show <routine-name>           # confirm enabled and the effective state
orbit clock tick --dry-run                  # what would fire on this host; writes nothing
```

`enable` and `disable` (`orbit routine disable <routine-name>`) need operator
authority; an agent session is refused. They act on the selected workspace:
`--workspace <name>` when given (`orbit --workspace <name> routine enable
<routine-name>`), otherwise the current checkout. A routine that belongs to
another registered workspace is refused, and the error names the workspace to
rerun with. The write re-checks the state it read and refuses if the routine
changed in the meantime; the command is audited. Copy the name from `orbit
routine list`: seeded names carry the workspace suffix (`<base>-<workspace>`),
which the base names in the table below omit.

`.orbit/` is per-user state that git ignores, so the enabled field is a local
setting, not a commit. The host-local pause (`orbit routine pause`) and the host
clock (`orbit clock`) are separate controls; see
[Observe and control](#observe-and-control). Registering the checkout already
made the workspace a routine source; an older `.orbit/config.toml` may still
carry a `[routines]` section, which is ignored with a warning and can be
deleted.

<a id="the-nine-seeded-routines"></a>
<a id="the-ten-seeded-routines"></a>
<a id="the-eleven-seeded-routines"></a>
## The seeded routines

`orbit workspace init` seeds the routines selected by `seeding.routines`
(`"all"` by default) from this catalog, with a workspace-unique name
(`<base>-<workspace>`) resolved at seed time. See [Seeding policy](configuration.md#seeding-policy)
to change that selection and reconcile an existing workspace. Every routine default starts disabled
except `worktree-gc` and `task-pilot`. A workspace seeded while `worktree-gc` or `task-pilot`
shipped disabled keeps its `enabled` value. The `host-health` auto-task is the one shipped
auto-task seeded enabled ([Auto-tasks](auto-tasks.md)). Each of the two starts enabled only when
the seeding policy selects it. The cron routines
resolve nothing else per machine, so two hosts seed identical bytes.
`task-pilot` and `resident-cycle`
are state-triggered and additionally resolve this host's
machine id as `trigger.state.owner_machine` and the registered base branch as
`trigger.state.branch`; a second host registering the same workspace seeds
itself as owner of its own copy. Run `orbit routine list` to see their names
on this host.

| Base name | Cadence | Target | What it does |
|---|---|---|---|
| `worktree-gc` | hourly | `worktree_gc_pipeline` | Reclaims worktrees whose task settled to done, rejected, or archived, or has been parked in someday for `worktree.parked_reclaim_after_hours` (default 336), and prunes `.orbit/tmp` entries untouched for 24 hours (`scratch_older_than_hours`). |
| `task-pilot` | state trigger (`preparation_eligible`) | `task_pilot_pipeline` | Fingerprints eligible proposed/backlog tasks each tick and preflights a batch whose material has no fresh assessment (seeded `batch_size` 5, mixed crews included); quiet while the backlog is unchanged. |
| `resident-cycle` | state trigger (`resident_due`) | `resident_cycle_pipeline` | Disabled by default; starts one bounded cycle for each active resident mandate whose wake digest changed (a watched task, an answered decision request, an operator comment, a notice) or whose heartbeat expired, never inside its cadence floor and never beside a live cycle. Core scopes the cycle's tool calls to the mandate's granted rights and workspaces, and an open maintenance notice makes it observe-only. Lint without restoration only reads; `orbit task lint --restore-pruned` (one task or a sweep) is refused inside a resident cycle because no right grants context maintenance. Use an operator outside the cycle for restoration. |
| `ci-failure-sweep` | hourly at :05 | `ci_failure_sweep_pipeline` | Files deduped proposed CI findings, pilots them, and admits only current warning-free repairs to backlog. |
| `dependabot-alert-sweep` | daily at 03:25 host-local time | `dependabot_alert_sweep_pipeline` | Collects Dependabot, code-scanning, and secret-scanning findings and files remediation tasks. |
| `store-gc` | daily at 04:17 | `store_gc_pipeline` | Applies `orbit gc audit` and `orbit gc runs`: prunes audit rows older than `retention.audit_days` (this workspace and workspace ids that are no longer registered) and the audit blobs nothing names, and drops the pipeline state of terminal runs older than `retention.runs_days` and the per-minute resource history and admission forecasts older than 30 days, except the newest counted rows that stay at any age. Another registered workspace keeps its own window. Review both plans before enabling it — [maintenance.md](maintenance.md#store-retention). On a large store, consider `retention.runs_days = 30` first; smaller windows are refused to preserve 30-day reliability and forecast-accuracy evidence. |
| `task-publication` | weekly Sunday at 04:47 host-local time (`47 4 * * 0`) | `task_publication_pipeline` | Disabled by default because publishing task prose to a remote is an operator decision. Publishes only this owned workspace; unbound workspaces record `skipped: publication_unbound`. Unchanged content is a recorded no-op. Attachments default to `omit` under `publication.routine_attachments`; `include` fails closed without the explicit unscanned opt-in. Replicas report it owner-only and never fire it. Missed runs skip; overlaps are forbidden — [publication.md](publication.md#weekly-publication-routine). |

The `finding-verification`, `evolution-pass`, `ux-rules` and `pilot-warning-triage`
routines were removed. `orbit workspace sync` retires a workspace's seeded copies,
and a definition that still targets one of their jobs, or the `pilot_warnings_held` trigger,
loads as retired and never fires. Triage a task held on pilot warnings from the dashboard's
Triage action, which still runs `escalation_triage_pipeline`. No routine or installed command
runs the UX rules now; Orbit runs no UI rule checks and files no tasks from them. The leftover keys of these removals are listed in
[Maintenance — Retired features](maintenance.md#retired-features-and-their-keys).

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
automation is refused. A proposed task held by pilot warnings gets the same
card from pilot-warning triage (approve, re-pilot or rewrite), started from the
dashboard's Triage action; that triager only proposes and never approves, promotes or edits the task itself.

The seeded `blocked-task-triage` routine and its `block_escalated` trigger are
retired. `orbit workspace sync` removes an unedited copy; an existing
`blocked-task-triage-*` routine file loads inert and is reported as retired.
`orbit.task.block_triage` now serves pilot-warning triage only, and
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

A newly initialized workspace starts with steps 1 and 2 enabled. Otherwise,
enable in this order and stop wherever the value runs out. Each step is safe
without the ones after it; **the reverse is not true.**

1. **`worktree-gc` first.** It is the only one that reclaims disk, and every
   implementation workflows create runs that can leave worktrees behind.
   Deterministic evidence-filing sweeps do not create worktrees. Running
   unattended drains without GC is how a workspace fills a disk. Watch one
   cycle with `orbit gc worktrees --dry-run` before enabling. →
   [maintenance.md](maintenance.md)
2. **`task-pilot`.** Its agent inspection is read-only; its apply step writes validated task selectors, and it makes everything downstream safer:
   populated `context_files` are what conflict detection and file reservation
   use to keep parallel runs off each other's files. It fires on task creation
   or a material edit (debounced two minutes, at most ten) rather than on a
   clock, so an unchanged backlog costs nothing. See
   [Tuning which tasks task-pilot prepares](#tuning-which-tasks-task-pilot-prepares).

No routine ships work. Dispatch the backlog with `orbit run auto` drains or
`orbit run ship` once the steps above have proved themselves against real
traffic. A workspace that still holds the retired `ship-sweep` routine loads it,
warns, and never runs it; `orbit workspace sync` retires the seeded copy.

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
checks the new trigger as the loader would (exactly one of `cron`, `state`,
`deliveries_landed` or `condition`; a cron that parses; a state kind whose job
is the routine's target; `overlap: forbid` for state and delivery triggers; a
condition or `when` guard that type-checks), and rewrites only
the `trigger:` block. Comments inside that block are not kept; the rest of the
file is. Name, target, policy and `enabled` are not editable this way. Editing a
seeded routine makes it a local edit: `orbit workspace sync` keeps your file and
stops refreshing it from Orbit's template.

### Condition triggers

A routine's `trigger.condition` or an auto-task's `schedule.condition` is a CEL
predicate over task and workspace state. Orbit type-checks it when the file
loads and refuses, per file, an unknown field, a non-`bool` result, a computed
`tasks[...]` or `board.by_tag[...]` index, a task scope without a top-level
status or literal-tag conjunct, an optional field (`task.origin`, `task.crew`,
`self.last_fired`, ...) read without `has()`, `matches()`, and an expression
over 4 KiB, 200 nodes or two nested comprehensions. The error names the file,
the clause and the line:column in the expression.

Only the workspace owner fires a condition; followers and replica checkouts
write and admit nothing. A `task` or `workspace` subject fires once per edge
(true for `for_minutes`, then held fired until `rearm`), under a durable fire
ID that a crash, restart or second clock cannot repeat. An auto-task files one
task, a routine submits one job run with the subject and fire in its input.
Unknown values never fire. Enabling baselines subjects already true unless
`on_enable: fire_existing`; `orbit auto-task toggle <name> on --dry-run`
previews the count, and an enable over `max_matches` is refused (a routine
stalls as `too_many_matches`). Fire caps and cooldown defer fires; a match
storm, a chain deeper than 5 (`loop_suspected`) or a subject oscillating more
than three times an hour stalls the definition until
`orbit automation clear-stall <name> --reason <text>` (operator, audited).
`host` and `file` scopes are refused.

`orbit automation explain <name>` (MCP `orbit.automation.explain`) shows
matches with arm state and holds, closest misses, one subject's clause tree,
and the durable tracking (stall, last pass and skip, recent fires with their
chain, `self`), read-only. `orbit auto-task list`, `orbit routine list` and the
dashboard show the condition state and any stall.

A cron, interval, delivery or state trigger also takes a `when` guard (a
workspace predicate without `self`). A false or unknown guard holds the fire:
a cron or interval slot waits (`guard_miss: wait`, default) or is skipped
(`guard_miss: skip`), delivery debt and state members are kept, and the skip
records `guard_false` or `guard_unknown`. The keys and the readable schema are
in the website's Scheduling YAML reference, Condition triggers.

A job may also name an auto-task as a step (`target: auto-task:<name>`) with
CEL `wait_until`, `expire_when` and `reuse_when` fields. These steps are
checked when the job loads and explained by `orbit automation explain
job:<name>`. An enabled job declares `schemaVersion: 3` and runs detached;
when a step waits, the run parks durably and the owner clock revisits it. A
step with `require_result` also needs the definition's matching `completion`
block ([auto-tasks](auto-tasks.md#proving-a-job-step-with-a-completion-contract))
and otherwise holds. The same fields under
`schemaVersion: 2` fail to load, even when null, so an older Orbit refuses the
file instead of dropping its waits. The keys are in the website's Activity and job
YAML reference, Auto-task steps.

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

## Workspace scripts

A workspace script is a program that a job step runs as `target: script:<name>`.
Its manifest lives at `.orbit/resources/scripts/<name>.yaml`, and the manifest's
`entry` names the program file in the workspace. A script step runs only while
the script holds an operator grant on the host that runs it. Granting runs
nothing; the step runs when a job reaches it.

### Grants

A grant is the operator's host-local consent to run one script at one set of
content. It binds to the digests of both the manifest and the entry file.
Editing either file makes the grant `stale`: the script does not run until an
operator grants it again at the new digests.

Only operators grant and revoke. An agent session can list, show, and read
history, but must not grant, even when it can edit the manifest or the program.

```bash
orbit script grant <name>                          # grant at the current digests
orbit script grant <name> --read-root /srv/reports --write-root /srv/out
orbit script grant <name> --follower               # grant on a pull follower, not the owner
orbit script revoke <name>                         # revoke this host's grant
```

| Grant flag | Meaning |
|---|---|
| `--network` | Network the grant allows: `none`, `https` or `any`. Default: what the manifest requests. |
| `--read-root <path>` | Extra absolute read root to grant. Repeatable. |
| `--write-root <path>` | Extra absolute write root to grant. Repeatable. |
| `--env-pass <NAME>` | Extra environment variable name to pass to the script. Repeatable. |
| `--follower` | Grant for a pull follower host instead of the workspace owner. |
| `--output-max-files <n>` | Most declared output files one step may keep, 1 to 64. Default 8. |
| `--output-max-file-bytes <b>` | Largest output file one step may keep, up to 64 MiB. Default 1 MiB. |

Every read and write root, from the flags or the manifest's `requests`, must
already exist, sit outside the credential paths (`~/.ssh`, `~/.config/gh`, the
keychains, …), and pass a probe by the granting process: it lists the directory
(or opens a file root), and for a write root creates and removes a file. A root
that fails refuses the grant, names the path, and nothing is granted. Before
every run, the process that runs the job repeats the probe; a root that is
missing or denied then refuses the step with `script_root_unavailable`, naming
the path, and nothing runs. Orbit never creates a granted root.

A script that declares `files.outputs` in its manifest keeps the regular files it
writes at the top level of `out/`, and later steps read them as
`steps.<id>.files` (`name`, `path`, `bytes`, `media_type`, `sha256`). The
manifest's caps must fit the grant: asking for more than the grant allows
refuses the step with `grant_insufficient` and names the flag to grant again
with. Each grant states its output limits again, so granting without the flags
returns to the defaults. A step's whole set never exceeds 256 MiB. Output over
the caps, a symlink, a subdirectory or an unsafe file name fails the step and
exposes none of its files.

To file a task that carries those files, a later step of the same job runs the
shipped `attach_script_files` activity with `task_id` and `step` (the script
step's id), and optionally `files` (names to attach; all by default) and
`path_prefix` (prepended to each artifact path). It attaches each file as a
task artifact from bytes that still match the recorded SHA-256. A changed file,
a name outside the set, a file over the 1 MiB task-artifact limit or a script
step that is not a direct, earlier step of the job refuses the step and
attaches nothing.

A manifest's `files.inputs` lists `{from, as}` entries. `from` may read the
step input (`{{ input.<field> }}`) and resolves, symlinks followed, to a file
or directory inside the step's cwd tree or a granted read root (`--read-root`).
Each one appears read-only at `in/<as>` (`$ORBIT_SCRIPT_IN/<as>`). An input
that is missing, resolves anywhere else, or overlaps the workspace's `.orbit/`
or a credential path refuses the step before it runs with
`script_file_input_unavailable`, naming the input. Each attempt records every
input's name, source path and SHA-256 (for a directory, a digest over its whole
tree) as `input_files` in the `--json` output of `orbit run show` and
`orbit script history`.

A granted script step runs on Linux under `linux-bwrap-script-v1` and on macOS
under `macos-sandbox-script-v1`; other platforms refuse the step with
`script_sandbox_unavailable`, and so does a step started inside another sandbox
(a worker's own Seatbelt or Bubblewrap sandbox), which never runs nested. The
`RLIMIT_CPU` soft limit is the timeout rounded up to whole CPU seconds per
process, with a minimum of 1 CPU second, and its hard limit is 10 CPU seconds
later, capped at the hard limit inherited by the wrapper. The script can raise
its CPU soft limit up to that hard limit. `RLIMIT_NOFILE` is 1024 for both its
soft and hard limits, capped at the wrapper's inherited hard limit, so the
script cannot raise it. On Linux, network access is off unless allowed by the
grant. `--network https` shares the host network but admits outbound TCP to
port 443 only (it needs Landlock ABI 4, so an older kernel refuses the step),
and `--network any` shares it with no rule.
Only the variables named with `--env-pass` (or in the manifest's
`requests.env_pass`) reach the script, copied from the environment of the
process that runs the job; `ORBIT_*` names and the profile's own `PATH`,
`HOME`, `TMPDIR`, `LANG` and `TZ` are never passed. A passed value is redacted
from the step's log, its output and `orbit run show` / `orbit script history`.
A manifest asking for more network or another variable than its grant refuses
with `grant_insufficient`. If the sandbox itself fails, see
[Linux sandbox](linux-sandbox.md).

On macOS a script step never has network: a grant with `--network https` or
`any` refuses the step with `script_sandbox_unavailable`. The macOS profile
allows the same paths as the Linux one, but the script sees its scratch
directory at its host path (use `$ORBIT_SCRIPT_IN`, `$ORBIT_SCRIPT_OUT`,
`$HOME` and `$TMPDIR`, never `/orbit-script`), and declared inputs are copied
into `in/`, so they count against the scratch size limit. It has no memory or
task limits, a timed-out descendant that calls `setsid` can outlive the step
(still confined), and file metadata outside the grant stays visible; `orbit
doctor` lists these gaps in its `script-sandbox` row. With
`machine.worker_containment = "strict"`, every script step on macOS is refused.
macOS privacy permissions (TCC) apply on top of the profile and belong to the
process that runs the job (the Orbit clock's launch agent, or the `orbit`
binary it runs), not to the terminal that granted. A root they deny refuses the
step with `script_root_unavailable`, naming the path and the System Settings >
Privacy & Security pane to allow it in.

### Grant states

`orbit script list` and `orbit script show` report one of these states:

| State | Meaning |
|---|---|
| `granted` | The grant's digests match the current manifest and entry file. |
| `stale` | A verified grant whose digests no longer match: one of the files changed after granting. |
| `ungranted` | A manifest with no grant. |
| `orphaned` | A grant whose manifest no longer exists. |
| `witness_refused` | The stored grant is not trusted because its witness is missing or invalid, its row does not match the witness, or its timeout ceiling is outside `1..=4294967295` seconds. Review it and run `orbit script grant <name>` to re-authorize it. |

### Grant requests

When a script step is refused with `grant_missing`, `grant_stale` or
`grant_insufficient`, Orbit also raises a `script_grant` decision request, and the
refusal message names it. The request carries the script, the digest it would
grant, the refused grant's digest, the diff against the last granted bytes, and
the requested against granted capabilities. Another refusal at the same digest and
capability set reuses the open request. After an edit, the next refusal raises a new
request and supersedes the old one. Requests live on a `someday` task per script,
titled ``Script grant requests: `<name>` ``, which agents never pick up. A witness
failure, a missing sandbox or a changed interpreter raises no request, because a
grant would not clear it.

```bash
orbit decision list --gate script_grant     # open grant requests
orbit decision show <id>                     # script, digests, capabilities and the diff to review
orbit decision answer <id> grant             # write the grant at that digest (operator only)
orbit decision answer <id> reject            # leave the script ungranted
```

A `grant` answer writes the grant exactly as requested, with the answering human
as grantor. It is refused, and writes nothing, when the script's bytes or requested
capabilities have changed since the request was raised. The same answer also comes
from a registered [decision channel](plugins.md#decision-channels) (provenance
`decision-channel <name>`) or from the dashboard. An agent session or plugin
callback can neither answer the request nor write a grant.

The dashboard's **Automation › Scripts** view lists every script with its state
(adding `revoked` for a script whose grant was withdrawn after it ran), digests,
the commit that made it stale, requested against granted capabilities, its open
request and the diff. An operator session can grant from there; the action is
bound to the digest shown, so bytes that change after the page loaded are refused.
A grant answer binds the catalog bytes in the workspace's primary checkout. A step
that ran from a worktree with different bytes is refused as superseded until those
bytes land.

### Verify a grant

```bash
orbit script list                  # NAME, ENTRY, GRANT, DIGEST for every script
orbit script show <name>           # manifest and entry paths, current and granted digests, network, grantor, detail
orbit script history <name>        # recent attempts, newest first, refusals included (--limit, default 20)
```

1. `orbit script list` should show `granted` in the GRANT column for each script
   you expect to run. `stale`, `ungranted` or `orphaned` means the step will
   not run.
2. `orbit script show <name>` should report `grant state: granted`, with the
   current manifest and entry digests equal to the granted ones. A difference
   after an edit is the staleness above; grant again to accept the edited files.
3. After a run, `orbit script history <name>` lists each attempt, including the
   refused ones.

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
      exclude_tags: [no-diff-expected, no-diff-needed, no-pilot]
      require_tags: []               # every listed tag must be present
      task_types: []                 # empty admits every type
```

`batch_size` is how many due tasks one tick admits into a single
`task_pilot_pipeline` run. Tasks whose stored `task.crew` differs, including
set versus unset, share that run; prepare gives each crew its own partitions
of at most five, so one pass assesses every crew instead of refusing a mixed
bundle. The supported `orbit run task-pilot` command groups explicit tasks by
stored crew and submits one homogeneous run per crew. A hand-assembled direct
`orbit run job task_pilot_pipeline` with mixed task crews is still refused at
run start. A burst of same-crew filed tasks is prepared as one run rather than
one run per task, and
`orbit clock tick --dry-run` lists the batch it would admit. Narrow the
predicate to, say, `statuses: [backlog]` or `require_tags: [pilot]` to keep the
scheduled pilot off proposals a human has not looked at yet. Unknown keys and
an empty `statuses` fail the definition closed. The predicate selects eligible
never-assessed tasks for their first automatic assessment. It is also part of
the material fingerprint checked during preparation and apply, but changing it
does not queue another assessment for a task with an applied assessment. The
default predicate keeps fingerprints accepted before the block existed. Explicit
runs (`orbit run task-pilot <id>`) ignore the block.

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

`[workflow.task_pilot_freshness]` controls material drift checks, not automatic
re-piloting. By default `material_fields` covers title, description, acceptance
criteria, plan and selectors. It can also include `tags`, `crew`, `tools`,
`type`, `complexity`, `relations`, `dependencies` and `instructions`.
`source_sensitivity` defaults to `ignore`; choose `context_files` to check
branch changes at selector paths or `any` to check every branch-head move.
These settings guard prepare/apply and assessment admission. A `freshness:`
block under the routine's `trigger.state` overrides either key for that routine:
routine value, then workspace config, then default. Changing material or the
resolved policy does not queue another pilot for an assessed task.

For example, after choosing a policy, set and inspect the workspace values:

```bash
orbit config set workflow.task_pilot_freshness.source_sensitivity context_files
orbit config get workflow.task_pilot_freshness.source_sensitivity
orbit config get workflow.task_pilot_freshness.material_fields
orbit config show --all                    # effective config and provenance
orbit routine show task_pilot              # inspect routine overrides
orbit run readiness <id> --json             # inspect task holds
```

Description or criteria edits hold dispatch as `pilot_assessment_stale` until
a new assessment completes (the drain queues one automatically for an
unclaimed task), unless an authorized operator skips pilot
admission with `no-pilot`. Other readiness, dependency, lock and delivery gates
still apply. For the operator's explicit `orbit task repilot <id>` procedure,
no-pilot authority and bounded re-check/full behavior, follow
[task-pilot orchestration](../../orbit-orchestrate/references/orchestration.md).

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
`routine/<name>`. Three controls act at different scopes, and an enabled
routine can still be held on one host:

| Control | Scope | Commands |
|---|---|---|
| Enabled (the definition) | The routine's versioned `enabled` field in this checkout | `orbit routine enable <name>`, `orbit routine disable <name>` (operator only, audited) |
| Host-local pause | This host only; unversioned; survives reboots | `orbit routine pause <name>`, `orbit routine resume <name>` |
| Host clock | Every routine and auto-task this host runs | `orbit clock pause`, `orbit clock enable` (definition state is untouched) |

```bash
orbit routine enable <routine-name>   # set enabled: true (operator only)
orbit routine disable <routine-name>  # set enabled: false (operator only)
orbit routine pause <name>            # hold it on this host only
orbit routine resume <name>           # clear the host-local pause
orbit clock status                    # clock cadence and whether it is paused
```

Verify an enablement, a pause and the clock separately. `orbit routine list`
shows each routine's enabled and paused state and its next due time, and
`orbit routine show <name>` shows its definition and effective state. An
auto-task uses its own kill-switch, `orbit auto-task toggle <name> on|off`
([Auto-tasks](auto-tasks.md)).

## "Why didn't it fire?"

Resolve the toggles in this order — `orbit routine list` shows both at once:

1. `enabled: false` in this checkout's definition (`orbit routine enable` sets
   it; per-user, gitignored).
2. A local pause (`orbit routine pause`: this host only, unversioned, durable
   across reboots).

If neither explains it, check further out: is this checkout registered as an
owner (`orbit workspace list`), is the clock unit running and not paused
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
filing honors operator `covered_by` relations and timed archive/reject
suppression of the same evidenced cause
(`ci_failure.operator_suppression_hours`, default 6; see
[recovery](../../orbit-orchestrate/references/recovery.md)). Equal normalized
failing-test signatures consolidate across jobs with every job retained.
A job whose log has a line starting with an entry of
`workflow.ci_infrastructure_signatures` is listed under `infrastructure`, not
filed, and on a pull request it gets a CI rerun rather than a repair agent. The
sweep itself reruns nothing, so a failed landing-branch run stays red until a
rerun or a newer push.
Otherwise filing first reuses a still-open `ci-failure:<key>` owner, then follows
a rejected exact-key task's inbound `Supersedes` relation to its covering owner,
preferring a still-open owner and then the lowest task ID if several supersede
the source. Any inbound relation suppresses the legacy comment reader, even
when its superseder is terminal; a terminal superseder does not fall through to
a legacy comment. Covering-task comments are read only when no inbound
`Supersedes` relation exists. Filing then tries a high-confidence material
match (generated workflow/job/step labels, or a specific error together with
the failing command or the same run and job).
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
these routines independently of how you dispatch; admission to backlog does
not authorize implementation or completion.
