---
title: Scheduling YAML
description: "Fields, defaults and examples for hand-authored auto-task and routine YAML."
---

Use this page to write or check an auto-task or routine file by hand. An **auto-task** files a task on a schedule; a **routine** starts a catalog job. [Routines and auto-tasks](/concepts/scheduling/) explains the model, and [Schedule recurring work](/how-to/recurring-work/) covers the clock.

Both files live under `.orbit/` in a checkout, which Git ignores, so a definition applies to that checkout on that machine. A file with an unknown key or an invalid value is skipped; it never fires with defaults.

## Names and `enabled`

A name uses lowercase letters, digits, `-` and `_`, starting with a letter or digit.

| | Auto-task | Routine |
|---|---|---|
| Path | `.orbit/auto_tasks/<name>.yaml` | `.orbit/routines/<file>.yaml` (`.yml` is also read) |
| Name rule | The file stem equals `name`. | `name` is unique across every routine on this host. The file stem can differ. |
| `enabled` absent | Enabled | Enabled |

`enabled: false` keeps the file and its history. From the CLI:

```bash
orbit auto-task toggle <name> off   # disable an auto-task; on enables it
orbit routine disable <name>        # disable a routine; enable reverses it
orbit routine pause <name>          # this machine only, not in the file; resume clears it
```

`orbit workspace init` seeds the shipped definitions. The `worktree-gc`, `task-pilot` and `crew-memory-dream` routines start enabled, and the rest start disabled, including the `host-health` auto-task. A seeded routine is named `<stem>-<workspace>`; `orbit routine list` prints the names. `orbit workspace sync` refreshes a seeded file without turning a routine on. It does turn off a `host-health` auto-task that no operator has configured; `orbit auto-task toggle host-health on` turns it on and keeps it on.

A replica checkout schedules only a cron routine whose target is the worktree-GC or store-GC job. A delivery, state or condition trigger stays with the owner, even on those jobs.

## Auto-task definitions

Create one with `orbit auto-task add`, or write the file and read it back with `orbit auto-task show <name>`. This definition files one chore each Monday at 09:00 once you set `enabled: true`:

```yaml
schemaVersion: 1
name: weekly-dep-audit
description: Weekly check for outdated dependencies.
enabled: false
schedule:
  cron: "0 9 * * 1"
template:
  title: Audit outdated dependencies
  description: Audit the lockfile for known advisories.
  acceptance_criteria:
    - List outdated packages and their latest versions.
  tags:
    - maintenance
  required_tools:
    - github.run.list
  context_files:
    - file:package.json
  complexity: medium
dedupe: skip_if_open
```

| Field | Default | Meaning |
|---|---|---|
| `schemaVersion` | Required | `1`. |
| `name` | Required | The definition name. |
| `description` | Empty | Free text. |
| `enabled` | `true` | Whether the clock fires it. |
| `schedule` | Required | See [One schedule](#one-schedule). |
| `template` | Required | See [Template fields](#template-fields). |
| `dedupe` | `skip_if_open` | See [Dedupe](#dedupe). |
| `skip_if_unchanged`, `skip_unless_blocked_candidates` | None, `false` | See [Preconditions](#preconditions). |
| `completion` | None | See [Completion evidence](#completion-evidence). |
| `granted_tools` | None | Tools an operator grants every filed task. Set it with `orbit auto-task update --granted-tools`. |
| `created_by`, `created_at`, `updated_by`, `updated_at` | — | Written by Orbit. |

Every filed task also carries the tag `auto-task:<name>`.

### One schedule

`schedule` carries exactly one of these forms:

| Form | Meaning |
|---|---|
| `cron` | Five-field cron in the host's local time. |
| `every_minutes` | An integer from 1 through 525600. |
| `deliveries_landed` | Verified landings on a branch. See [Delivery triggers](#delivery-triggers). |
| `condition` | A predicate over workspace state. See [Condition triggers](#condition-triggers). |

```yaml
schedule:
  every_minutes: 60
```

`cron`, `every_minutes` and `deliveries_landed` take an optional `when` guard, and `cron` and `every_minutes` take `guard_miss`. See [Guard a schedule with `when`](#guard-a-schedule-with-when).

A cron or interval definition that missed several slots files one make-up task.

### Template fields

| Field | Default | What it sets on each filed task |
|---|---|---|
| `title` | Required | Title. |
| `description` | Empty | Body. `--body` on the CLI. |
| `acceptance_criteria` | None | One string per criterion. `--criterion`, repeatable. |
| `task_type` | `chore` | `feature`, `bug`, `refactor` or `chore`. `--type`. |
| `tags` | None | Extra tags. `--tag`, repeatable. |
| `required_tools` | None | Exact tool names copied onto the task. `--required-tools`. An unknown name, or one the implementing activity disallows beyond any grant, fails when the definition is saved. |
| `context_files` | None | Context selectors: `file:`, `dir:` or `symbol:`. No CLI flag. |
| `priority` | `medium` | `low`, `medium`, `high` or `critical`. |
| `complexity` | Omitted | `low`, `medium`, `hard` or `xhard`. Omit the key to leave the task unassessed. |
| `crew` | Omitted | Crew override. `--crew`. |
| `status` | `backlog` | Status of the filed task. `--status`. A `backlog` template files the task as `proposed` unless its tags include `no-diff-expected`. Other statuses are kept. |

### Dedupe

| Value | Meaning |
|---|---|
| `skip_if_open` | Default. A fire is skipped while a task this definition filed is still open: any status other than `done`, `archived`, `rejected` or `someday`. |
| `always` | Files a task even when an earlier one is open. |

On the CLI, `--dedupe` takes `skip-if-open` or `always`.

`orbit auto-task mint <name>` files one task now. It ignores the schedule, dedupe, `enabled` and preconditions, and does not move the next scheduled fire.

### Preconditions

A precondition skips a due fire when there is nothing to do. Both are file fields with no CLI flag. Editing either on a shipped definition makes the file your own: `orbit workspace sync` stops refreshing it.

`skip_if_unchanged` skips while the branch tip is already covered by the last completed sweep:

```yaml
skip_if_unchanged:
  ref: main
  cursor:
    tags:
      - code-review
      - auto-task:code-review
    legacy_tags:
      - code-review-sweep
```

| Field | Meaning |
|---|---|
| `ref` | Required. The branch to compare. |
| `cursor.tags` | Required, at least one tag. The newest `done` chore that carries every tag is the last sweep. Include `auto-task:<name>` so a hand-filed chore cannot match. |
| `cursor.legacy_tags` | Consulted only when `tags` selects nothing, so a tag rename does not reset the cursor. |

That sweep must attach a `sweep-cursor.json` artifact whose `ref` equals `skip_if_unchanged.ref`:

```json
{"schema_version": 1, "ref": "main", "cursor": "<40- or 64-character commit id>"}
```

When the branch tip is that commit or an ancestor of it, the tick skips with reason `unchanged_since_last_sweep`. If the ref or cursor cannot be read, the tick files the task. The shipped `code-review`, `qa-sweep` and `qa-full-sweep` definitions use this block.

`skip_unless_blocked_candidates: true` files a task only when at least one blocked task has an unsettled block. Otherwise the tick skips with reason `no_blocked_candidates`.

### Completion evidence

`completion` declares the evidence a [job step](/reference/activity-job-yaml/#auto-task-steps) must see before it accepts a task from this definition. The step's `require_result` must name the same contract. Without that match and a granted adapter, the step waits and files nothing.

```yaml
schemaVersion: 1
name: example-evidence-check
enabled: false
schedule:
  every_minutes: 60
template:
  title: Assemble evidence for the team check
  acceptance_criteria:
    - Attach the evidence files and the completion manifest.
completion:
  contract: release-readiness/v1
  adapter: evidence-check
  form: review-summary
  evidence_artifact: completion-evidence.json
  max_age_minutes: 1440
  read_task_tags:
    - qa-evidence
  evidence_instructions: Attach review.json.
```

| Field | Meaning |
|---|---|
| `contract` | Required. `release-readiness/v1` is the only value a job step can require. |
| `adapter` | Required. The name of the workspace script that judges the evidence. |
| `form` | Required. A label passed to the adapter. Orbit gives it no meaning. |
| `evidence_artifact` | Required. Relative path of the manifest the task must attach. |
| `max_age_minutes` | Required, positive. How long a later step may rely on an accepted result. |
| `read_task_tags` | Optional. Tags whose tasks the owner reads back for the adapter. |
| `evidence_instructions` | Optional. What the evidence must hold, added to the task a job step files. |

**The adapter.** Write a [script manifest](/reference/activity-job-yaml/#script-steps) at `.orbit/resources/scripts/evidence-check.yaml`, then grant it on the workspace owner with `orbit script grant evidence-check`. An absent, stale or ungranted adapter holds the step. Orbit sends the adapter one JSON object on stdin:

| Key | Holds |
|---|---|
| `schema_version`, `contract`, `form` | `1` and the definition's contract and form. |
| `trusted` | Owner-recorded `workspace_id`, `definition`, `definition_digest`, `task_id`, `run_id`, `source_revision`, `completed_at`, `execution_machine` and `release_survey`. |
| `evidence.manifest` | The task's candidate manifest, shown below. |
| `evidence.artifacts` | Per listed path: its `sha256` and `json`, the parsed value or `null` when the bytes are not JSON. |
| `tasks` | Owner-verified task facts under `bound`, `spawned`, `tagged` and `referenced`. |

**The task.** The job step adds a **Required evidence** block to its task with the binding values. Before finishing, the agent attaches the evidence files and then this manifest, at the `evidence_artifact` path, with `orbit.task.artifact` action `put`:

```json
{
  "schema_version": 1,
  "contract": "release-readiness/v1",
  "binding": {
    "workspace_id": "<from Required evidence>",
    "definition": "example-evidence-check",
    "definition_digest": "<from Required evidence>",
    "task_id": "<this task id>",
    "run_id": "<this run id>",
    "source_revision": "<from Required evidence>"
  },
  "artifacts": [
    {"path": "review.json", "sha256": "<sha256 of the exact file bytes>"}
  ],
  "task_refs": []
}
```

Copy the six binding values exactly. List at least one evidence file in `artifacts`, not the manifest itself. Do not add `status` or `completed_at`.

**The verdict.** The adapter prints one JSON verdict on stdout and exits 0:

```json
{
  "schema_version": 1,
  "contract": "release-readiness/v1",
  "form": "review-summary",
  "task_id": "<task id from trusted input>",
  "source_revision": "<pinned revision from trusted input>",
  "status": "passed",
  "reasons": []
}
```

`status` is `passed`, `blocked` or `unavailable`. With the last two, give `reasons` as `[{"code":"...","detail":"..."}]`. Only `passed` with matching contract, form, task ID and source revision is accepted. Anything else holds the step, and Orbit checks again until the step expires. A step is also held when the workspace base branch moves off the revision pinned for the job; start a fresh job run.

A later step reuses an accepted result only while the task is still done, the definition and pinned source still match, the result is within `max_age_minutes` and its artifacts are unchanged. Conditions read the accepted time as `self.last_succeeded_at` or `auto_tasks['<name>'].last_succeeded_at`.

To inspect a held step, run `orbit run show <run-id> -s <step-id>` or `orbit automation explain job:<name>`.

## Delivery triggers

`schedule.deliveries_landed` on an auto-task and `trigger.deliveries_landed` on a routine use the same object. It watches verified landings on one branch.

```yaml
schemaVersion: 1
name: landed-batch-review
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
  acceptance_criteria:
    - Each delivery in the batch has a verdict.
```

| Field | Required | Meaning |
|---|---|---|
| `branch` | Yes | Branch to watch. No leading `-` and no spaces. |
| `threshold` | Yes | Fire once this many uncovered deliveries are pending. 1 through `max_items`. |
| `max_wait_minutes` | Yes | Also fire when the oldest pending delivery has waited this long. Greater than 0. |
| `coverage` | Yes | `landed_code_review_v1`. |
| `max_items` | No | Deliveries in one batch, oldest first. Default 50, at most 50. |
| `retries` | No | Extra attempts after one that closed without accepted evidence. Default 0, at most 5. Each retry waits 5 minutes, and the budget ends 24 hours after the batch is frozen. |
| `owner_machine` | No | A machine id from `orbit host list`. Default: the workspace's registered owner. |

`orbit auto-task add --deliveries-landed '<JSON>'` takes the same fields and writes the file disabled. Before you enable it, `orbit auto-task show <name> --preview` reports the owner, the proposed baseline and any existing debt without creating a task.

- **Baseline.** The first evaluation records the current branch head and ignores older history.
- **Source.** With an `origin` remote, Orbit observes `origin/<branch>`, and a failed fetch waits and retries. With no remote, it observes the local branch.
- **Exclusions.** A landing with a passed before-PR review certificate does not count toward `threshold`.
- **Owner.** Only the resolved owner files new work; other machines list the definition as `skipped`. With no resolvable owner it reports `ownership_unresolved`: register the workspace owner or set `owner_machine`.

`coverage: integrated_qa_v1` is retired. Existing files still load, but `orbit auto-task add` and `update` refuse it. The shipped `delivery-code-review` definition is seeded disabled.

`orbit auto-task recover` and `orbit auto-task reset` repair a stalled delivery consumer. See [Repair a delivery-triggered definition](/how-to/recurring-work/#repair-a-delivery-triggered-definition).

## Routine definitions

A routine fires a job from `orbit job list`. To run an activity or a script, wrap it in a one-step job.

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

A cron routine runs on every machine that has this checkout registered as an owner and the clock on.

| Field | Default | Meaning |
|---|---|---|
| `schemaVersion`, `name`, `description`, `enabled` | — | As in an auto-task definition. |
| `trigger` | Required | See [One trigger](#one-trigger). |
| `target` | Required | `job:<name>`. |
| `policy.timeout_minutes` | `60` | 1 through 10080 (7 days). After this, an in-flight fire is recorded as timed out and stops blocking `overlap: forbid`. |
| `policy.overlap` | `forbid` | `forbid` skips a fire while one is in flight. `allow` dispatches anyway. |
| `policy.retries.max` | `0` | Re-dispatches after the first fire fails. |
| `policy.retries.backoff_minutes` | `2` | Minutes before a re-dispatch. At most 10080. |

A `hosts:` key is retired: the file loads, and the key is ignored with a warning. `orbit doctor` warns when an enabled routine's last success is older than three times its schedule interval. After three fires in a row fail with the same error class, this host stops firing that routine until a trial succeeds; see [Recover a tripped routine](/how-to/recurring-work/#recover-a-tripped-routine). `orbit clock tick --dry-run --verbose` prints the decision for every routine and auto-task and writes nothing.

### One trigger

`trigger` carries exactly one of these forms:

| Form | Rule |
|---|---|
| `cron` | Five-field cron in the host's local time. |
| `state` | See [State triggers](#state-triggers). `policy.overlap` must be `forbid`. |
| `deliveries_landed` | The [delivery trigger](#delivery-triggers) object. `policy.overlap` must be `forbid`. |
| `condition` | The [condition trigger](#condition-triggers) object. It takes no `when` guard. |

| Field | Default | Meaning |
|---|---|---|
| `trigger.missed_run` | `skip` | For cron slots missed while the machine was down. `skip` records each missed slot and dispatches nothing. `catch_up_once` fires one make-up run. |
| `trigger.when`, `trigger.guard_miss` | None | See [Guard a schedule with `when`](#guard-a-schedule-with-when). |

### State triggers

A state trigger watches tasks, waits out a debounce and starts its job. Only `owner_machine` evaluates it.

| Field | Meaning |
|---|---|
| `kind` | Which tasks qualify, and which job `target` must name. |
| `owner_machine` | Required machine id from `orbit host list`. `orbit workspace init` writes this host's id. |
| `branch` | Required. No leading `-` and no spaces. Init writes the registered base branch. |
| `debounce_minutes` | Greater than 0. A member is due once it has stayed unchanged this long. |
| `max_wait_minutes` | At least `debounce_minutes`. A member that keeps changing is due once it has waited this long. |
| `max_items` | How many members are remembered, 1 through 50. |
| `batch_size` | How many due members one run takes, 1 through `max_items`. Default 5. |
| `retries` | Extra attempts after one that closed without accepted evidence, 0 through 5. Each retry waits 5 minutes. `policy.retries.max` caps it. |
| `deadline_minutes` | 1 through 1440. The retry budget ends this long after admission. |

| `kind` | Required `target` | What it watches |
|---|---|---|
| `preparation_eligible` | `job:task_pilot_pipeline` | Proposed or backlog tasks that need a task-pilot assessment. |
| `resident_due` | `job:resident_cycle_pipeline` | Resident mandates that are due. `batch_size` must be 1. |
| `decision_requested` | `job:decision_request_pipeline` | Tasks with an open decision request that has not been notified. |
| `finding_unverified`, `pilot_warnings_held`, `block_escalated`, `execution_failed` | Retired | An old file still loads, and the routine is skipped as retired. |

A routine that targets a removed job is also skipped as retired. `orbit workspace sync` removes an unedited seeded copy; delete a file you wrote yourself. Do not add a second routine of the same kind: each enabled routine fires.

`eligibility` and `freshness` are legal only on `preparation_eligible`.

| Field | Default | Meaning |
|---|---|---|
| `eligibility.statuses` | `[proposed, backlog]` | A non-empty subset of those two. |
| `eligibility.exclude_tags` | `[no-diff-expected, no-diff-needed, no-pilot]` | Tasks with any of these tags are skipped. |
| `eligibility.require_tags` | `[]` | Tags a task must carry. A tag cannot be both required and excluded. |
| `eligibility.task_types` | `[]` | Task types to admit. Empty admits every type. |
| `freshness.material_fields` | Config value | A non-empty list from `title`, `description`, `criteria`, `plan`, `selectors`, `tags`, `crew`, `tools`, `type`, `complexity`, `relations`, `dependencies` and `instructions`. |
| `freshness.source_sensitivity` | Config value | `ignore`, `context_files` or `any`. |

`freshness` overrides [`workflow.task_pilot_freshness`](/reference/config/#settable-keys) for that routine only. Changing either block does not re-pilot a task that already has an assessment; use `orbit task repilot`.

### Blocked tasks: final recovery decides

No routine triages blocked tasks. Final recovery decides each one. It first tries bounded mechanical steps: resume the failed run, rerun the pull request's failed CI jobs, requeue, complete a no-diff task, or link a friction record.

What remains gets a decision card. Answer it from the dashboard or with `orbit decision answer`; the answer moves the task out of `blocked`.

### Delivery routine

A routine can watch landings instead of a clock. `policy.overlap` stays `forbid`, and `policy.retries.max` caps the trigger's `retries`, so set them equal when you want a retry.

```yaml
trigger:
  deliveries_landed:
    branch: main
    threshold: 3
    max_wait_minutes: 360
    coverage: landed_code_review_v1
    retries: 1
target: job:weekly_report_pipeline
policy:
  retries:
    max: 1
```

The owner, baseline and exclusion rules of [delivery triggers](#delivery-triggers) apply. The routine submits the job and files no task.

## Condition triggers

`trigger.condition` on a routine and `schedule.condition` on an auto-task use the same object: a [CEL](https://github.com/google/cel-spec) predicate over the workspace's state. Each clock tick on the workspace owner evaluates it, and a subject fires once when the predicate becomes true: an auto-task files one task, a routine submits one run.

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

| Key | Default | Meaning |
|---|---|---|
| `scope` | Required | `task`: one subject per task the prefilter selects, bound as `task`. `workspace`: one subject; read `board`. `host` and `file` parse and are refused as `scope_unavailable`. |
| `when` | Required | The CEL predicate. It must be `bool`. |
| `rearm` | `!(when)` | The predicate that re-arms a fired subject. |
| `for_minutes`, `cooldown_minutes` | `0` | Hold before a fire, and minimum time between a subject's fires. At most 10080. |
| `max_fires_per_hour`, `max_fires_per_day` | `10`, `50` | Fire caps per definition. The daily cap is at least the hourly one. |
| `max_matches` | `20` | Matches admitted at enable, 1 through 1000. |
| `on_enable` | `baseline` | `baseline` or `fire_existing`: what enabling does with subjects already true. |
| `board_includes_own` | `false` | Count the definition's own filed tasks in `board`. |
| `hosts` | — | Host scope only. |
| `roots`, `glob`, `max_depth`, `settle_seconds`, `identity`, `hash_full_max_bytes`, `group_by`, `group_settle_seconds` | — | File scope only. |

### How a condition fires

Each subject is armed or fired. An armed subject fires when `when` has stayed true for `for_minutes`. A fired subject stays fired until `rearm` is true. A value that cannot be read, such as a `tasks['<ID>']` that does not exist, is unknown, not false: it never fires, never re-arms and restarts the hold.

- **Once.** A fire is recorded before its task or run is created, so a crash or a second clock cannot file it twice.
- **Enabling.** Enabling, or changing `when`, `rearm`, `for_minutes`, `scope`, the template or the job, marks subjects that are already true as fired without firing them, unless `on_enable: fire_existing`. An auto-task that would match more than `max_matches` subjects is refused; `orbit auto-task toggle <name> on --dry-run` shows the count. Such a routine stalls as `too_many_matches`.
- **Caps.** A fire past an hourly or daily cap, or inside a subject's cooldown, waits and fires when the cap allows.
- **Stalls.** A definition stalls as `oscillating` when a subject has fired three times in an hour, as `match_storm` when one pass finds more than `max_matches` new matches, and as `loop_suspected` when a fire's subject is itself at chain depth 5. A stalled definition fires nothing until you run `orbit automation clear-stall <name> --reason <text>`.
- **Dedupe.** A condition auto-task with `skip_if_open` waits while a task it filed is open, then files.

An auto-task template may use `{{subject.key}}`, `{{subject.fire_id}}`, `{{subject.revision}}`, and `{{subject.task.<field>}}` (task scope) or `{{subject.board.<field>}}` (workspace scope). A missing value renders as `(none)`. A routine's job input carries `subject` and `condition` (`fire_id`, `trigger`, `depth`, `subject`, `revision`).

### Guard a schedule with `when`

A cron, interval, delivery or state trigger can take a `when` guard: a workspace-scope CEL predicate, checked when the trigger is due. It cannot read `self`.

```yaml
schedule:
  cron: "0 9 * * 1-5"
  when: board.in_progress.count < 3
  guard_miss: wait
```

| Trigger | When the guard is false or unknown |
|---|---|
| `cron`, `every_minutes` | `guard_miss: wait` (default) keeps the slot due and fires the latest due slot once when the guard opens. On a routine it replaces `missed_run: skip`. `guard_miss: skip` records the slot as skipped. |
| `deliveries_landed` | The landed debt is kept and fires when the guard opens. |
| `state` | Due members are held and taken when the guard opens. |

Each held or skipped fire records `guard_false` or `guard_unknown`.

### What an expression reads

| Read | Fields |
|---|---|
| `task` (task scope) | `id`, `title`, `status`, `tags`, `type`, `priority`, `complexity`, `crew` (optional), `parent_id` (optional), `dependencies`, `created_at`, `updated_at`, `status_since`, `origin` (optional: `trigger`, `fire_id`, `depth`) |
| `tasks['<ID>']` | The same fields for one task. The index is a string literal. |
| `board` | `by_status['<status>']`, `by_tag['<tag>']` (literal keys), `drain_running`, and `eligible`, `backlog`, `in_progress`, `blocked`, `review` and `open_runs`, each with `count` and `empty` |
| `self` | `last_fired` (optional) and `fires_24h`. An auto-task also has `last_minted` and `last_minted_task` (optional), and, with a completion contract, `last_succeeded_at` (optional). |
| `auto_tasks['<name>']` | `last_succeeded_at` (optional): when the named auto-task's completion contract last accepted a result. The name is a string literal. |
| `now` | The evaluation time |

`last_succeeded_at` is absent until a result is accepted, so guard it: `!has(auto_tasks['release-prep'].last_succeeded_at) || now - auto_tasks['release-prep'].last_succeeded_at > duration('24h')`. A clause that reads it is unknown when the history cannot be read, the name is not a defined auto-task, or the auto-task declares no completion contract.

Statuses are spelled as stored: `proposed`, `backlog`, `in_progress`, `review`, `done`, `blocked`, `archived`, `rejected`, `someday`. Test an optional field before you read it: `has(task.origin) && task.origin.depth < 3`.

The language is a subset of CEL: literals, comparison and logic, `in`, `has()`, `size()`, `startsWith`, `endsWith`, `contains`, `lowerAscii`, `timestamp()` and `duration()` with their arithmetic and accessors, and `all`, `exists`, `exists_one`, `map` and `filter` over lists. `matches()`, map literals and other functions are refused.

### Load-time checks

A condition that fails a check fails to load. The error names the file, the clause (`trigger.condition.when`) and the line and column in the expression.

- The expression parses, every identifier and field exists in its scope, and the result is `bool`.
- A `task` scope has a top-level `&&` clause that limits the subjects: `task.status == '<status>'`, `task.status in [...]` or `'<tag>' in task.tags`.
- The expression is at most 4 KiB, 200 parse nodes and two nested comprehensions.

`orbit automation explain <name>` evaluates the condition over live state and writes nothing. It shows the matches, the closest misses and any stall. With `--subject <task ID>`, it shows each clause with the values it read. Use `routine:<name>` or `auto-task:<name>` when both kinds share a name.
