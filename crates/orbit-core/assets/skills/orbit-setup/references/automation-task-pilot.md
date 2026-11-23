# Automation: tuning task-pilot

## Contents

- [Tuning which tasks task-pilot prepares](#tuning-which-tasks-task-pilot-prepares)

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
stored crew and submits one homogeneous run per crew.

A hand-assembled direct
`orbit run job task_pilot_pipeline` with mixed task crews is still refused at
run start. A burst of same-crew filed tasks is prepared as one run rather than
one run per task, and
`orbit clock tick --dry-run` lists the batch it would admit. Narrow the
predicate to, say, `statuses: [backlog]` or `require_tags: [pilot]` to keep the
scheduled pilot off proposals a human has not looked at yet. Unknown keys and
an empty `statuses` fail the definition closed.

The predicate selects eligible
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
pilot.

An explicit run skips held tasks, records `already_preparing`
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
still apply.

For the operator's explicit `orbit task repilot <id>` procedure,
no-pilot authority and bounded re-check/full behavior, follow
[task-pilot orchestration](../../orbit-orchestrate/references/orchestration-pilot-admission.md).

**Upgrading from the cron form.** A workspace seeded before the state form
holds a cron `task_pilot.yaml` (`*/40 * * * *`). `orbit workspace sync` refreshes
an unmodified one — or one whose only edit is `enabled: true` — onto the state
form owned by this host, keeping the opt-in (`orbit workspace sync --check`
previews it).

A cron file whose cadence or policy was hand-edited is left in
place and reported as `preserved`; either restore the shipped bytes and rerun
the sync, or edit it to the state form above by hand.
