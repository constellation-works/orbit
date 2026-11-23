# Automation: routines

## Contents

- [The seeded routines](#the-seeded-routines)
- [Authoring a routine](#authoring-a-routine)
  - [Changing a routine's trigger](#changing-a-routines-trigger)
  - [Condition triggers](#condition-triggers)
- [Routines a plugin seeds](#routines-a-plugin-seeds)

## The seeded routines

`orbit workspace init` seeds the routines selected by `seeding.routines`
(`"all"` by default) from this catalog, with a workspace-unique name
(`<base>-<workspace>`) resolved at seed time. See [Seeding policy](configuration-layers.md#seeding-policy)
to change that selection and reconcile an existing workspace. Every routine default starts disabled
except `worktree-gc` and `task-pilot`, plus one dormant experiment routine that
mints nothing unless a running experiment has a memory arm.

A workspace seeded while `worktree-gc` or `task-pilot`
shipped disabled keeps its `enabled` value. The `host-health` auto-task is seeded disabled,
like every other shipped auto-task ([Auto-tasks](auto-tasks-authoring.md)). Each of these starts enabled only when
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
| `worktree-gc` | hourly | `worktree_gc_pipeline` | Reclaims worktrees whose task settled to done, rejected, or archived, or has been parked in someday for 336 hours (14 days), and prunes `.orbit/tmp` entries untouched for 24 hours (`scratch_older_than_hours`). |
| `task-pilot` | state trigger (`preparation_eligible`) | `task_pilot_pipeline` | Fingerprints eligible proposed/backlog tasks each tick and preflights a batch whose material has no fresh assessment (seeded `batch_size` 5, mixed crews included); quiet while the backlog is unchanged. |
| `resident-cycle` | state trigger (`resident_due`) | `resident_cycle_pipeline` | Disabled by default; starts one bounded cycle for each active resident mandate when something it acts on changed (any change to a task in its `watch`, a state change of a task its cycles acted on, an answered decision request, an operator comment, a warn, incident or maintenance notice opening or clearing, except resident-keyed notices and notices authored by the mandate's assignment task), its checkpoint's planned next wake came or its heartbeat expired, never inside its cadence floor and never beside a live cycle. Core scopes the cycle's tool calls to the mandate's granted rights and workspaces, and an open maintenance notice makes it observe-only. Lint without restoration only reads; `orbit task lint --restore-pruned` (one task or a sweep) is refused inside a resident cycle because no right grants context maintenance. Use an operator outside the cycle for restoration. |
| `ci-failure-sweep` | hourly at :05 | `ci_failure_sweep_pipeline` | Files deduped proposed CI findings, pilots them, and admits only current warning-free repairs to backlog. |
| `dependabot-alert-sweep` | daily at 03:25 host-local time | `dependabot_alert_sweep_pipeline` | Collects Dependabot, code-scanning, and secret-scanning findings and files remediation tasks. |
| `store-gc` | daily at 04:17 | `store_gc_pipeline` | Applies `orbit gc audit` and `orbit gc runs`: prunes audit rows older than `retention.audit_days` (this workspace and workspace ids that are no longer registered) and the audit blobs nothing names, and drops the pipeline state of terminal runs older than `retention.runs_days` and the per-minute resource history and admission forecasts older than 30 days, except the newest counted rows that stay at any age. Another registered workspace keeps its own window. Review both plans before enabling it — [maintenance-storage.md](maintenance-storage.md#store-retention). On a large store, consider `retention.runs_days = 30` first; smaller windows are refused to preserve 30-day reliability and forecast-accuracy evidence. |
| `task-publication` | weekly Sunday at 04:47 host-local time (`47 4 * * 0`) | `task_publication_pipeline` | Disabled by default because publishing task prose to a remote is an operator decision. Publishes only this owned workspace; unbound workspaces record `skipped: publication_unbound`. Unchanged content is a recorded no-op. Attachments are always omitted. Replicas report it owner-only and never fire it. Missed runs skip; overlaps are forbidden — [publication-bind.md](publication-bind.md#weekly-publication-routine). |

The `finding-verification`, `evolution-pass`, and `ux-rules` routines were removed,
while `pilot-warning-triage` was retired via its `pilot_warnings_held` state
trigger rather than its job.

`orbit workspace sync` retires a workspace's seeded copies,
and a definition that still targets one of the three removed jobs
(`finding_verification_pipeline`, `evolution_pass_pipeline`,
`ux_rules_pipeline`) loads as retired and never fires unless the workspace
defines a job of the same name, which resolves first and stays active.

A
definition on the retired `pilot_warnings_held` trigger loads as retired even
though its `escalation_triage_pipeline` target still ships, so a cron definition
targeting that job without the retired trigger stays active. Triage a task held on pilot warnings from the dashboard's
Triage action, which still runs `escalation_triage_pipeline`. No routine or installed command
runs the UX rules now; Orbit runs no UI rule checks and files no tasks from them.

The leftover keys of these removals are listed in
[Maintenance — Retired features](maintenance-retired-features.md#retired-features-and-their-keys).

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
observed `target` and `expected_trigger` and the replacement `trigger`.

Both run
the same write: it refuses a target or trigger that changed since you read it,
checks the new trigger as the loader would (exactly one of `cron`, `state`,
`deliveries_landed` or `condition`; a cron that parses; a state kind whose job
is the routine's target; `overlap: forbid` for state and delivery triggers; a
condition or `when` guard that type-checks), and rewrites only
the `trigger:` block. Comments inside that block are not kept; the rest of the
file is.

Name, target, policy and `enabled` are not editable this way. Editing a
seeded routine makes it a local edit: `orbit workspace sync` keeps your file and
stops refreshing it from Orbit's template.

### Condition triggers

A routine's `trigger.condition` or an auto-task's `schedule.condition` is a CEL
predicate over task and workspace state.

Orbit type-checks it when the file
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
Unknown values never fire.

Enabling baselines subjects already true unless
`on_enable: fire_existing`; `orbit auto-task toggle <name> on --dry-run`
previews the count, and an enable over `max_matches` is refused (a routine
stalls as `too_many_matches`). Fire caps and cooldown defer fires; a match
storm, a chain deeper than 5 (`loop_suspected`) or a subject oscillating more
than three times an hour stalls the definition until
`orbit automation clear-stall <name> --reason <text>` (operator, audited).

`host` and `file` scopes are refused.

`orbit automation explain <name>` (or MCP `orbit.automation.explain` with
`--toolset automation`) shows matches with arm state and holds, closest misses,
one subject's clause tree,
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
when a step waits, the run parks durably and the owner clock revisits it.

A
step with `require_result` also needs the definition's matching `completion`
block ([auto-tasks](auto-tasks-authoring.md#proving-a-job-step-with-a-completion-contract))
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
`orbit routine list` and the dashboard hide it.

`orbit routine list --all`
lists it as `inactive` with a reason naming the plugin, and `orbit routine show
<name>` still resolves it. Re-enabling brings it back with no re-seed.
