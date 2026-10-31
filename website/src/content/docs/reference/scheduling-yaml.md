---
title: Scheduling YAML
description: "Hand-authored auto-task and routine YAML: schedules, templates, delivery, state and condition triggers, dedupe, and preconditions."
---

An **auto-task** is one YAML file that files a task on a schedule. A **routine** is one YAML file that starts a catalog job. The host clock evaluates both. A new chore is a new auto-task; a new pipeline fire is a new routine. [Routines and auto-tasks](../../concepts/scheduling/) is the model, and [Schedule recurring work](../../how-to/recurring-work/) is the clock, pause, and repair procedure.

Both files live under `.orbit/` in a checkout. Git ignores that directory, so a definition applies to this checkout on this machine. The clock reads every registered owner checkout. An invalid file is skipped and never fires with defaults. Unknown keys are rejected.

## Names and `enabled`

A name is lowercase letters and digits plus `-` or `_`, and it starts with a letter or digit.

| | Auto-task | Routine |
|---|---|---|
| Path | `.orbit/auto_tasks/<name>.yaml` | `.orbit/routines/<file>.yaml` (`.yml` is also read) |
| Name rule | The file stem equals `name` | `name` is unique across every routine source on this host. The file stem can differ. |
| `enabled` absent | The definition is enabled | The routine is enabled |

`enabled: false` keeps the file and its history. It is the kill switch. Delete is a separate command for auto-tasks; for a routine, `enabled: false` is how you retire it on this checkout. `orbit routine pause <name>` suppresses one routine on this machine only. A pause is not written into the file, is not synced, and survives a reboot. `orbit routine resume <name>` clears it.

`orbit workspace init` seeds the shipped definitions disabled, except the hourly crew-performance refresh, which is enabled and records a run even while crew performance is off. Seeded routine names are the file stem with underscores turned into hyphens, then a hyphen and the registered workspace name, so two workspaces on one host do not collide. `orbit routine list` prints the name. `orbit workspace sync` refreshes a seeded file and does not turn it on.

A replica checkout schedules only its worktree-GC and store-GC routines. Its other routines are listed as owner-only.

Every task an auto-task files carries the tag `auto-task:<name>` in addition to `template.tags`.

## Auto-task definitions

Create one with `orbit auto-task add`, or write the file yourself and read it back with `orbit auto-task show <name>` before you rely on it. Pass exactly one of `--cron`, `--every-minutes`, or `--deliveries-landed`. A cron or interval add is enabled immediately. A delivery add is written disabled.

This cron definition files one chore each Monday at 09:00 in the host's local time. It stays off until you set `enabled: true`:

```yaml
schemaVersion: 1
name: weekly-dep-audit
description: Weekly check for outdated dependencies.
enabled: false
schedule:
  cron: "0 9 * * 1"
template:
  title: Audit outdated dependencies
  description: Audit the lockfile and file follow-up tasks for known advisories.
  acceptance_criteria:
    - List outdated packages and their latest versions.
    - File follow-up tasks for known advisories.
  task_type: chore
  tags:
    - maintenance
  required_tools:
    - github.run.list
  context_files:
    - file:package.json
  priority: medium
  complexity: medium
  status: backlog
dedupe: skip_if_open
```

The same template on an interval replaces the schedule with one field:

```yaml
schedule:
  every_minutes: 60
```

`schemaVersion` is `1`.

### One schedule

`schedule` carries exactly one of `cron`, `every_minutes`, `deliveries_landed`, or `condition`. A file with none of them, or with more than one, is rejected and names which forms it found.

| Form | Meaning |
|---|---|
| `cron` | Five-field cron, evaluated in the host's local time. An empty value is rejected. |
| `every_minutes` | An integer from 1 through 525600 (365 days). The interval is anchored at the slot first observed for this definition. |
| `deliveries_landed` | Verified landings on a branch. There is no clock slot. See [Delivery triggers](#delivery-triggers). |
| `condition` | A typed predicate over workspace state. It fires once per subject edge on the workspace owner. See [Condition triggers](#condition-triggers). |

`cron`, `every_minutes` and `deliveries_landed` also take an optional `when` guard, and `cron` and `every_minutes` take `guard_miss`. See [Guard a schedule with `when`](#guard-a-schedule-with-when).

A cron or interval definition that was asleep through several slots files one make-up task for the missed window, then waits for the next slot. A delivery definition does not catch up on a clock.

### Template fields

| Field | Default | What it sets on each filed task |
|---|---|---|
| `title` | Required, non-empty | Title. |
| `description` | Empty | Body. `orbit auto-task add --body` writes this field. |
| `acceptance_criteria` | None | One string per criterion. `--criterion` repeats. |
| `task_type` | `chore` | `feature`, `bug`, `refactor`, or `chore`. `--type`. |
| `tags` | None | Extra tags. The `auto-task:<name>` tag is added for you. `--tag` repeats. |
| `required_tools` | None | Exact tool names copied onto the task. `--required-tools`. An unknown name fails when the task is dispatched. |
| `context_files` | None | Context selectors (`file:`, `dir:`, or `symbol:`), one per line in the dashboard. There is no CLI flag; put them in the file. Older definitions omit the key. |
| `priority` | `medium` | `low`, `medium`, `high`, or `critical`. |
| `complexity` | Omitted | `low`, `medium`, `hard`, or `xhard` when you want an assessment. Omit the key to leave the task unassessed. There is no `unassessed` value to write. |
| `crew` | Omitted | Crew override. `--crew`. |
| `status` | `backlog` | Status the task enters: `proposed`, `backlog`, `in-progress`, `review`, `done`, `blocked`, `archived`, `rejected`, or `someday`. Use `backlog`, or `proposed` when you want to approve each one. `--status`. |

Turn-based budget fields are not part of the template. An unknown template key rejects the file.

### Dedupe

`dedupe` is `skip_if_open` or `always`. The default is `skip_if_open`: a fire is skipped while a task this definition already filed is still open, so a stalled backlog does not collect copies. Open means any status other than `done`, `archived`, `rejected`, or `someday`. Parking a task in `someday` does not hold the next fire.

`always` files a task even when an earlier one is open. Use it when each instance stands alone.

In the file, and in `orbit auto-task show`, the tokens are `skip_if_open` and `always`. `--dedupe` accepts `skip-if-open` and `always`, and accepts `skip_if_open` as an alias; it stores the snake_case token.

`orbit auto-task mint <name>` files one task immediately. It ignores the schedule, dedupe, `enabled`, and the preconditions below, and it does not move the next scheduled fire. The filed task is still visible to later `skip_if_open` checks.

### Preconditions

Preconditions answer a different question from dedupe. Dedupe asks whether an earlier task is still open. A precondition asks whether there is anything to do. Both are file fields. `orbit auto-task add` does not set them. On a shipped definition, editing either one makes the file your own copy: `orbit workspace sync` then keeps your bytes and stops refreshing the shipped body. `enabled`, the schedule, dedupe, and a template crew, priority, complexity, or added tag stay in the settings layer and still receive body updates.

`skip_if_unchanged` skips a due fire while an integration branch tip is already covered by the cursor the last completed sweep recorded:

```yaml
skip_if_unchanged:
  ref: main
  cursor:
    tags:
      - code-review
      - no-diff-expected
      - auto-task:code-review
    legacy_tags:
      - code-review-sweep
      - no-diff-expected
```

`ref` is the branch to compare, and it must be non-empty. `cursor.tags` needs at least one non-empty tag. The scheduler takes the newest `done` chore that carries every tag (AND). `legacy_tags` is consulted only when `tags` selects nothing, so a rename does not reset the cursor. Include `auto-task:<name>` in `tags`: only tasks this definition filed carry it, so a hand-filed chore that shares the other tags cannot become the cursor.

That sweep attaches a `sweep-cursor.json` artifact:

```json
{"schema_version": 1, "ref": "main", "cursor": "<40- or 64-character commit id>"}
```

`ref` in the artifact must equal `skip_if_unchanged.ref`. When the branch tip is that commit, or an ancestor of it, the tick skips, leaves the slot unconsumed, and records the reason `unchanged_since_last_sweep` with both commit ids. `orbit auto-task show <name>` and the dashboard show the skip. The next new commit files the pending occurrence.

If the ref or the cursor cannot be read, no completed sweep matches, or the probe fails, the tick files the task and records why. An unanswerable precondition does not stop the sweep. The shipped `code-review`, `qa-sweep`, and `qa-full-sweep` definitions carry this block against the workspace base branch. A delivery definition does not need it.

`skip_unless_blocked_candidates: true` files a task only when at least one blocked task has a block episode awaiting triage. Otherwise the tick skips with reason `no_blocked_candidates` and records how many blocked tasks it read. A probe that cannot read them files the task and records why. No shipped auto-task sets this. The blocked-task triage routine below applies the same gate itself. The retired `blocked-task-triage` auto-task is the one that used to carry the flag.

## Delivery triggers

`schedule.deliveries_landed` on an auto-task and `trigger.deliveries_landed` on a routine use the same object. The consumer watches verified landings on one branch and files work when the debt is due. It is opt-in.

```yaml
schemaVersion: 1
name: landed-batch-review
description: Review landed changes in batches.
enabled: false
schedule:
  deliveries_landed:
    branch: main
    threshold: 3
    max_wait_minutes: 360
    coverage: landed_code_review_v1
    max_items: 20
    retries: 1
template:
  title: Review a landed batch
  description: Review the frozen batch attached to this task.
  acceptance_criteria:
    - Each delivery in the batch has a verdict.
  task_type: chore
  priority: medium
  status: backlog
dedupe: skip_if_open
```

Write `enabled: false` in a hand-authored file. Omitting `enabled` means enabled. `orbit auto-task add --deliveries-landed '<JSON>'` writes the file disabled for you. The JSON object uses the same fields:

```bash
orbit auto-task add \
  --name landed-batch-review \
  --deliveries-landed '{"branch":"main","threshold":3,"max_wait_minutes":360,"coverage":"landed_code_review_v1","max_items":20,"retries":1}' \
  --title "Review a landed batch" \
  --body "Review the frozen batch attached to this task." \
  --criterion "Each delivery in the batch has a verdict."
```

Replace `main` with this workspace's registered base branch. Preview before you enable, then toggle:

```bash
orbit auto-task show landed-batch-review --preview
orbit auto-task toggle landed-batch-review on
```

Preview reports the owner, the proposed baseline, and any existing debt. It creates no task and moves no checkpoint.

| Field | Required | Meaning |
|---|---|---|
| `branch` | Yes | Branch to watch. No leading `-` and no spaces. |
| `threshold` | Yes | Fire once this many uncovered deliveries are pending. From 1 through `max_items`. |
| `max_wait_minutes` | Yes | Also fire when the oldest pending delivery has waited at least this many minutes. Greater than 0. |
| `coverage` | Yes | `landed_code_review_v1`. |
| `max_items` | No | Deliveries in one batch, default 50, at most 50. The batch takes the oldest pending deliveries, up to this count. |
| `retries` | No | Extra attempts after one that closed without accepted evidence. Default 0, at most 5. The attempt budget is `retries + 1`. Each retry waits 5 minutes, and the budget ends 24 hours after the batch is frozen. |
| `owner_machine` | No | Stable machine id from `orbit host list`. Omit it to use the registered owner of this workspace. |

The first evaluation records the current branch head as the baseline and does not examine older history. Later landings are the debt.

When the checkout has an `origin` remote, observation follows `origin/<branch>` and leaves the worktree untouched. A failed fetch waits and retries; it does not fall back to the local branch. With no remote, observation follows `refs/heads/<branch>`.

A passed before-PR review certificate excludes that landing from the obligations. The landing stays in the examined range as context, and it does not count toward `threshold`.

Only the resolved owner admits new work. Another machine reconciles what it already admitted and admits nothing; `orbit auto-task list` shows the definition as `skipped` and names the owner. If no owner can be resolved, the definition stays enabled and reports `ownership_unresolved`. Register the workspace owner, or set `owner_machine`. An explicit `owner_machine` overrides the registered owner. Reassigning the owner is a definition change: settle the old debt and preview the new baseline first.

`coverage: integrated_qa_v1` is retired. A file that already contains it still loads. `orbit auto-task add`, and `orbit auto-task update` that sets a schedule, refuse it. Hands-on QA is the `qa-sweep` and `qa-full-sweep` auto-tasks. The old `delivery-qa` auto-task is no longer seeded.

The shipped opt-in is `delivery-code-review`, seeded disabled, with `threshold: 3`, `max_wait_minutes: 360`, `max_items: 20`, and `retries: 1` on the workspace base branch. Turn it on with `orbit auto-task toggle delivery-code-review on` after `--preview`. While `workflow.purpose_crews.review` is set, that definition files tasks with a crew from that pool. The deprecated `operation.review_policy = "after-landing"` still turns the same definition on until you toggle it yourself. See [Configuration](../config/).

`orbit auto-task recover` and `orbit auto-task reset` repair a stalled delivery consumer. Both preview until you pass `--reason`. The procedure is in [Schedule recurring work](../../how-to/recurring-work/#repair-a-delivery-triggered-definition).

On a routine, `policy.retries.max` caps `retries` on this object. The default policy max is 0, which leaves no extra attempt. Set them equal when you want a retry. See the delivery routine below.

## Routine definitions

A routine fires `target: job:<name>` from `orbit job list`. `activity:<name>` is reserved and is rejected; wrap the activity in a one-step job and target that job. An inline command is rejected.

A hand-authored cron routine:

```yaml
schemaVersion: 1
name: weekly-report
description: Run the weekly report job.
enabled: false
trigger:
  cron: "0 9 * * 1"
  missed_run: skip
target: job:weekly_report_pipeline
policy:
  timeout_minutes: 60
  overlap: forbid
  retries:
    max: 0
    backoff_minutes: 2
```

Replace `weekly_report_pipeline` with a job this workspace's catalog already has. Set `enabled: true` when you want the clock to fire it. A cron routine runs on every machine that has this checkout registered as an owner and the clock on. There is no host list on the definition.

### One trigger

`trigger` carries exactly one of `cron`, `state`, `deliveries_landed`, or `condition`.

| Form | Extra rule |
|---|---|
| `cron` | Five-field cron in the host's local time. `missed_run` applies to slots missed while the machine was down. |
| `state` | `policy.overlap` is `forbid`. `target` is the job that kind requires. `cron` and `deliveries_landed` are absent. |
| `deliveries_landed` | `policy.overlap` is `forbid`. `cron` and `state` are absent. The object is the [delivery trigger](#delivery-triggers). |
| `condition` | The other forms are absent. The object is the [condition trigger](#condition-triggers). It takes no `when` guard. |

`cron`, `state` and `deliveries_landed` take an optional `trigger.when` guard, and `cron` takes `trigger.guard_miss`. See [Guard a schedule with `when`](#guard-a-schedule-with-when).

`missed_run: skip` is the default. Each missed slot is recorded and nothing is dispatched for it; `orbit routine list --json` shows the reason on `last_fire.detail`. `missed_run: catch_up_once` fires one make-up run. The seeded worktree-GC routine uses `catch_up_once`.

| Policy field | Default | Limit |
|---|---|---|
| `timeout_minutes` | 60 | 1 through 10080 (7 days). After this, an in-flight fire is recorded as timed out and stops blocking `overlap: forbid`. |
| `overlap` | `forbid` | `forbid` skips a fire while one is in flight. `allow` dispatches anyway. State and delivery routines require `forbid`. |
| `retries.max` | 0 | Re-dispatches after the initial fire fails. |
| `retries.backoff_minutes` | 2 | Minutes before that re-dispatch. At most 10080. |

`orbit doctor` warns when an enabled routine's last success is older than three times its schedule interval.

A `hosts:` key is retired. The file still loads, the key is ignored, and the loader warns with the file name. Remove the key. Do not add it to a new routine.

### State triggers

A state trigger watches tasks, waits out a short debounce, and starts its job. It stays quiet while nothing qualifies. Only `owner_machine` evaluates it. Other machines do not admit new work for it.

| Field | Meaning |
|---|---|
| `kind` | Which tasks qualify, and which job `target` must name. |
| `owner_machine` | Required machine id. `orbit workspace init` writes this host's id. `orbit host list` shows it. |
| `branch` | Required branch, with no leading `-` and no spaces. Init writes the registered base branch. |
| `debounce_minutes` | Greater than 0. A member is due once it has stayed unchanged this long. |
| `max_wait_minutes` | At least `debounce_minutes`. A member that keeps changing is still due once it has waited this long since it was first seen. |
| `max_items` | How many members are remembered, from 1 through 50. |
| `batch_size` | How many due members one run takes. From 1 through the smaller of 50 and `max_items`. Absent, the run takes 5, and never more than `max_items`. |
| `retries` | Extra attempts after one that closed without accepted evidence. 0 through 5. The attempt budget is `retries + 1`, each retry waits 5 minutes, and the budget ends `deadline_minutes` after admission. On a routine, `policy.retries.max` caps this number. |
| `deadline_minutes` | 1 through 1440. |

| `kind` | Required `target` | What it watches |
|---|---|---|
| `preparation_eligible` | `job:task_pilot_pipeline` | Proposed or backlog tasks that need a task-pilot assessment. The seeded task-pilot routine. |
| `finding_unverified` | `job:finding_verification_pipeline` | Review-filed bugs with no verification verdict. |
| `pilot_warnings_held` | `job:escalation_triage_pipeline` | Proposed tasks whose latest task-pilot assessment holds promotion on warnings. |
| `block_escalated` | `job:escalation_triage_pipeline` | Blocked tasks whose current block episode awaits triage. |
| `decision_requested` | `job:decision_request_pipeline` | Tasks with an open decision request that has not been notified, one member per open request. The job is the decision channel's notification job; a routine on this kind needs it installed. |
| `execution_failed` | Retired | Kept so an old file still loads. Its job, `task_triage_pipeline`, is no longer shipped, so the routine is skipped as retired. A failed run leaves its task blocked; returning it to the backlog is a human transition. |

`eligibility` and `freshness` are legal only on `preparation_eligible`. On any other kind the file is rejected. The seeded task-pilot routine already contains the default eligibility. Omitting the block resolves to the same predicate: statuses `proposed` and `backlog`, exclude tags `no-diff-expected` and `no-diff-needed`, no required tags, and every task type. Statuses are a non-empty subset of `proposed` and `backlog`. A tag cannot be both required and excluded.

`freshness` overrides [workflow.task_pilot_freshness](../config/#settable-keys) for that routine only. `material_fields` is a non-empty list drawn from `title`, `description`, `criteria`, `plan`, `selectors`, `tags`, `crew`, `tools`, `type`, `complexity`, `relations`, `dependencies`, and `instructions`. `source_sensitivity` is `ignore`, `context_files`, or `any`. An absent key keeps the config value, then the built-in default (title, description, criteria, plan, and selectors; branch moves ignored). Changing the resolved eligibility or freshness makes assessments accepted under the old rule stale.

The finding, pilot-warning, and blocked-task routines each put at most `batch_size` due members that share one stored crew into one run. Do not add a second routine of the same kind: each enabled routine fires.

`kind: execution_failed` and a target of `job:auto_task_scheduler_pipeline` are retired. The clock evaluates auto-tasks directly, so there is no scheduler routine to enable. `orbit workspace sync` retires a seeded copy of a retired routine. A file you wrote yourself is left in place and skipped; delete it, or point `target` at a job `orbit job list` still shows.

### Opt in to finding verification

Init writes `.orbit/routines/finding_verification.yaml`, named `finding-verification-<workspace>`, with `enabled: false`. Set `enabled: true` in that file, or use its switch under **Automation**. Leave the description, target, and trigger as seeded so sync can still refresh the file.

```yaml
enabled: false
trigger:
  state:
    kind: finding_unverified
    owner_machine: hm_example
    branch: main
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    batch_size: 5
    retries: 1
    deadline_minutes: 90
target: job:finding_verification_pipeline
policy:
  timeout_minutes: 90
  overlap: forbid
  retries:
    max: 1
    backoff_minutes: 5
```

`hm_example` and `main` stand in for the machine id and base branch init already wrote. Copying this block into a second file creates a second routine.

Enabled, the routine finds each review-filed bug (type `bug`, tagged `code-review`, `delivery-code-review`, `security-review`, or `qa-sweep`) that still has no verification verdict and is in `proposed`, `someday`, or `backlog`. After the debounce, or the max wait, it submits one batch. Approval, promotion, and drain admission keep refusing that bug until the job records a confirmed verdict. A recorded verdict drops the task from observation. Editing the finding's title, description, criteria, tags, type, or selectors releases a failed claim so it can be verified again. An explicit `orbit run job finding_verification_pipeline` does not use this gate.

### Opt in to pilot-warning triage

The seeded file is `.orbit/routines/pilot_warning_triage.yaml`, named `pilot-warning-triage-<workspace>`, disabled. The safe opt-in is `enabled: true` on that file.

```yaml
enabled: false
trigger:
  state:
    kind: pilot_warnings_held
    owner_machine: hm_example
    branch: main
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    batch_size: 5
    retries: 1
    deadline_minutes: 90
target: job:escalation_triage_pipeline
policy:
  timeout_minutes: 90
  overlap: forbid
  retries:
    max: 1
    backoff_minutes: 5
```

Enabled, it finds each `proposed` task whose latest applied task-pilot assessment holds promotion on warnings, once that hold has settled or waited out. The run only proposes: it does not approve, promote, clear a hold, or edit the task. An operator's approval, or a later task-pilot admission decision, ends membership. A material edit that produces a new holding assessment is a new member. An explicit `orbit run job escalation_triage_pipeline` does not use this gate.

### Opt in to blocked-task triage

The seeded file is `.orbit/routines/blocked_task_triage.yaml`, named `blocked-task-triage-<workspace>`, disabled. The safe opt-in is `enabled: true` on that file.

```yaml
enabled: false
trigger:
  state:
    kind: block_escalated
    owner_machine: hm_example
    branch: main
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    batch_size: 5
    retries: 1
    deadline_minutes: 90
target: job:escalation_triage_pipeline
policy:
  timeout_minutes: 90
  overlap: forbid
  retries:
    max: 1
    backoff_minutes: 5
```

Enabled, it finds each block episode (the task's latest move to `blocked`) that final recovery escalated, or that stayed undecided past the backstop's 72-hour limit, and that nobody outside Orbit's automation has touched since. The run records one disposition: a requeue, a resumed failed run, or a CI rerun on the pull request's failed checks, each counted against the shared final-recovery bound (2 per 24 hours); a friction record for an Orbit defect; a decision proposal; or a named human action. A requeue, resume or rerun is refused for a run another machine executed, for a `security` or `security-review` task, or once the bound is spent, and a rerun is also refused when the pull request's head has moved or no required check failed. Only operator-only or exhausted-bound requeue refusals are recorded with their human action; the task stays blocked and the marker settles the episode. A requeue preflight failure (including an unsettled run) or an unreadable recent-count error stops before a triage comment or marker is written, leaving the episode awaiting triage. If the count cannot be read, record a report that the block remains unresolved. A refused resume or rerun also records nothing, so the episode stays awaiting triage. A recorded marker drops that episode from observation. A later block on the same task is a new episode.

This routine replaces the retired `blocked-task-triage` auto-task. `orbit workspace sync` removes an unedited seeded copy of that auto-task and does not enable this routine. If that auto-task was turned on, sync says so and names this routine. An explicit `orbit run job escalation_triage_pipeline` does not use this gate.

### Delivery routine

A routine can watch landings instead of a clock. `overlap` stays `forbid`, and `policy.retries.max` is at least the trigger's `retries`:

```yaml
schemaVersion: 1
name: landed-batch-job
description: Run a catalog job when landings accumulate.
enabled: false
trigger:
  deliveries_landed:
    branch: main
    threshold: 3
    max_wait_minutes: 360
    coverage: landed_code_review_v1
    max_items: 20
    retries: 1
target: job:weekly_report_pipeline
policy:
  timeout_minutes: 90
  overlap: forbid
  retries:
    max: 1
    backoff_minutes: 5
```

Replace the job name and the branch. Omit `owner_machine` to use the workspace's registered owner, or set it to a machine id from `orbit host list`. The same owner, baseline, coverage, and exclusion rules as an auto-task delivery trigger apply. The routine submits the job; it does not file a task.

`orbit clock tick --dry-run --verbose` prints the decision for every routine and auto-task and writes nothing. `orbit routine show <name>` prints the definition the clock loaded.

## Condition triggers

`trigger.condition` on a routine and `schedule.condition` on an auto-task use the same object: a predicate in the [Common Expression Language](https://github.com/google/cel-spec) (CEL) over the workspace's state. The predicate is type-checked when the file loads. Each clock tick on the workspace owner evaluates every enabled condition, and a subject fires once when its predicate becomes true: an auto-task files one task, a routine submits one run of its job. Use `orbit automation explain <name>` to see what it matches before you enable it. `task` and `workspace` scopes fire; `host` and `file` scopes are refused.

```yaml
schemaVersion: 1
name: release-prep
enabled: false
schedule:
  condition:
    scope: workspace
    when: >-
      board.eligible.empty && board.in_progress.count == 0 && board.open_runs.count == 0
      && (!has(self.last_fired) || now - self.last_fired > duration('72h'))
    for_minutes: 30
template:
  title: Prepare release
  tags: [release]
dedupe: skip_if_open
```

A condition auto-task with `dedupe: skip_if_open` waits while a task it filed is still open; the fire stays pending and files once that task closes.

| Key | Default | Meaning |
|---|---|---|
| `scope` | required | `task` (one subject per task the prefilter selects, bound as `task`) or `workspace` (one subject; read `board`). `host` and `file` parse and are refused as `scope_unavailable`. |
| `when` | required | The CEL predicate. It must be `bool`. |
| `rearm` | `!(when)` | The predicate that re-arms a fired subject. |
| `for_minutes`, `cooldown_minutes` | `0` | Hold before a fire and minimum time between a subject's fires, at most 10080. |
| `max_fires_per_hour`, `max_fires_per_day` | `10`, `50` | Fire caps per definition. The daily cap is at least the hourly one. |
| `max_matches` | `20` | Matches admitted at enable, 1 through 1000. |
| `on_enable` | `baseline` | `baseline` or `fire_existing`: what enabling does with subjects already true. |
| `board_includes_own` | `false` | Count the definition's own minted tasks in `board`. |
| `hosts` | owner | Host scope only. |
| `roots`, `glob`, `max_depth`, `settle_seconds`, `identity`, `hash_full_max_bytes`, `group_by`, `group_settle_seconds` | — | File scope only. |

### How a condition fires

Each subject is either armed or fired. An armed subject fires when `when` has been true for `for_minutes` across consecutive evaluations. A gap longer than three clock cadences (at least five minutes) restarts the hold. A fired subject stays fired until `rearm` is true, then arms again. A value that cannot be read is unknown: it never fires, never re-arms, and breaks a hold, which restarts when the value is true again.

- **Exactly once.** A fire is written to the condition's durable state before its task or run is created, under a fire ID built from the definition, the subject and the subject's revision. Two clocks, a lost reply, a crash or a restart cannot file it twice; the next pass finishes a pending fire. A fire whose task or run cannot be created after five attempts is recorded as failed.
- **Owner only.** Only the workspace's owner machine evaluates and fires. A pull follower or a replica checkout writes nothing and admits nothing; `explain` shows the ownership verdict.
- **Enabling.** Enabling, or changing `when`, `rearm`, `for_minutes`, `scope`, the template or the job, takes a baseline: subjects already true are marked fired without firing, unless `on_enable: fire_existing`. An auto-task whose enable would match more than `max_matches` subjects is refused with the count; `orbit auto-task toggle <name> on --dry-run` shows the count and the subjects that would be baselined or fired, and writes nothing. A routine enabled with too many matches stalls as `too_many_matches` and fires nothing.
- **Caps and cooldown.** A fire past `max_fires_per_hour`, `max_fires_per_day` or a subject's `cooldown_minutes` waits, oldest first, and fires when the cap allows. A subject that has already fired three times in the past hour stalls the definition as `oscillating` instead of firing again.
- **Stalls.** More than `max_matches` new edges in one pass stalls as `match_storm`. A stalled definition fires nothing until an operator runs `orbit automation clear-stall <name> --reason <text>`, which is audited; a cause that persists stalls it again.
- **Chains.** A task filed by a fire records its origin (`trigger`, `fire_id`, `depth`), which a downstream condition reads as `task.origin`. A fire whose subject is itself at depth 5 is refused, recorded, and stalls the definition as `loop_suspected`.
- **Own tasks.** `board` leaves out tasks the definition filed itself unless `board_includes_own: true`. `self` reads the definition's own fire history, which survives edits and a disable.
- **Retention.** Fired subjects and fire records are kept 30 days, at most 1000 per definition.

An auto-task template may use `{{subject.key}}`, `{{subject.fire_id}}`, `{{subject.revision}}` and `{{subject.task.<field>}}` (task scope) or `{{subject.board.<field>}}` (workspace scope). Each value is capped at 200 characters; a missing one renders as `(none)`. A routine's job input carries `subject` and `condition` (`fire_id`, `trigger`, `depth`, `subject`, `revision`).

### Guard a schedule with `when`

A cron, interval, delivery or state trigger can take a `when` guard: a workspace-scope CEL predicate (it cannot read `self`) checked when the trigger is due.

```yaml
schedule:
  cron: "0 9 * * 1-5"
  when: board.in_progress.count < 3
  guard_miss: wait
```

| Trigger | When the guard is false or unknown |
|---|---|
| `cron`, `every_minutes` | `guard_miss: wait` (default) keeps the slot due and fires the latest due slot once when the guard opens; on a routine this replaces `missed_run: skip`. `skip` records the slot as skipped. |
| `deliveries_landed` | The landed debt is kept and fires when the guard opens. |
| `state` | Due members are held, not consumed, and are taken when the guard opens. |

Each held or skipped fire records `guard_false` or `guard_unknown`.

### What an expression reads

| Read | Fields |
|---|---|
| `task` (task scope) | `id`, `title`, `status`, `tags`, `type`, `priority`, `complexity`, `crew` (optional), `parent_id` (optional), `dependencies`, `created_at`, `updated_at`, `status_since`, `origin` (optional: `trigger`, `fire_id`, `depth`) |
| `tasks['<ID>']` | The same fields for one task. The index must be a string literal. |
| `board` | `by_status['<status>']`, `by_tag['<tag>']` (a literal tag), `drain_running`, and `eligible`, `backlog`, `in_progress`, `blocked`, `review` and `open_runs`, each with `count` and `empty` |
| `self` | `last_fired` (optional), `fires_24h`; an auto-task also has `last_minted` and `last_minted_task` (optional) |
| `now` | The evaluation time |

Statuses are spelled as stored: `proposed`, `backlog`, `in_progress`, `review`, `done`, `blocked`, `archived`, `rejected`, `someday`. Read an optional field only after testing it, as in `has(task.origin) && task.origin.depth < 3`. A value that cannot be read, such as a `tasks['<ID>']` that does not exist, makes the evaluation unknown, never false.

The language is a subset of CEL: literals, comparison and logic, `in`, `has()`, `size()`, `startsWith`, `endsWith`, `contains`, `lowerAscii`, `timestamp()` and `duration()` with their arithmetic and accessors, and `all`, `exists`, `exists_one`, `map` and `filter` over lists. `matches()`, map literals and other functions are refused.

### Load-time checks

A condition that fails any check fails to load. The error names the file, the clause (`trigger.condition.when`, `schedule.condition.rearm`) and the line and column inside the expression. Every other definition still loads.

- The expression parses, every identifier and field exists in its scope, every operator is well typed, and the result is `bool`.
- `tasks[...]` and `board.by_tag[...]` take a string literal, never a computed value.
- A `task` scope has a top-level `&&` conjunct that limits the subjects: `task.status == '<status>'`, `task.status in [...]`, or `'<tag>' in task.tags`.
- The expression is at most 4 KiB, 200 parse nodes (macros expanded) and two nested comprehensions.

`orbit automation explain <name>` (or `routine:<name>` / `auto-task:<name>` when both kinds share the name) evaluates the condition over live state and writes nothing. It shows the scope, the prefilter, the ownership verdict, up to 20 matches, up to 20 closest misses (subjects failing exactly one top-level clause), and for one subject (`--subject <task ID>`) each top-level `&&` and `||` clause with its value and the values it read. Each match shows its arm state and remaining hold, and the tracking block shows any stall, the last pass and skip, recent fires with their upstream chain, and `self`. `orbit auto-task list`, `orbit routine list` and the dashboard show the same state.

An Orbit release without condition triggers reports each such file as a load error (``unknown field `condition` ``) and keeps loading the rest.
