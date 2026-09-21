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
from every registered, active **owner** checkout on the host. Replica checkouts
are skipped, and there is no config key to set.

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
orbit clock set --cadence-seconds 300  # whole-minute cadence, reloads the unit
```

`clock pause` stops scheduled invocation; a manual `orbit clock tick` still works.
`orbit sweep` is a compatibility alias for the same tick and produces the same output.

**2. Enable routines, one at a time.** Each is a versioned YAML file — flipping
`enabled: true` is a reviewable commit, not a runtime toggle. Registering the
checkout already made the workspace a routine source; an older `.orbit/config.toml`
may still carry a `[routines]` section, which is ignored with a warning and can
be deleted.

## The five seeded routines

`orbit workspace init` seeds all five, **all disabled**, with a workspace-unique
name (`<base>-<workspace>`) resolved at seed time. The four cron routines resolve
nothing else per machine, so two hosts seed identical bytes. `task-pilot` is
state-triggered and additionally resolves this host's machine id as its
`owner_machine` and the registered base branch as the `branch` it observes;
a second host registering the same workspace seeds itself as owner of its own
copy. Run `orbit routine list` to see their names on this host.

| Base name | Cadence | Target | What it does |
|---|---|---|---|
| `worktree-gc` | hourly | `worktree_gc_pipeline` | Reclaims worktrees whose task settled to done, rejected, or archived. |
| `task-pilot` | state trigger (`preparation_eligible`) | `task_pilot_pipeline` | Fingerprints eligible proposed/backlog tasks each tick and preflights one whose material has no fresh assessment; quiet while the backlog is unchanged. |
| `ci-failure-sweep` | hourly at :05 | `ci_failure_sweep_pipeline` | Files deduped proposed CI findings, pilots them, and admits only current warning-free repairs to backlog. |
| `dependabot-alert-sweep` | daily at 03:25 host-local time | `dependabot_alert_sweep_pipeline` | Collects Dependabot, code-scanning, and secret-scanning findings and files remediation tasks. |
| `ship-sweep` | every 20m | `workspace_ship_pipeline` | Ships this workspace's ready backlog through the gated pipeline, unattended. |

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
   pushes, and opens PRs without a human present. It also needs
   `workflow.auto_ship = true`. Do not enable it in the same change as anything
   above; let the earlier ones prove themselves against real traffic first.

## Authoring a routine

Only a `job:` target is accepted — an `activity:` target is rejected, so wrap a
single activity in a one-step job.

```yaml
schemaVersion: 1
name: <routine-name>              # unique across every routine source on the host
enabled: true                      # versioned kill-switch
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
`task_pilot_pipeline` run. The batch is crew-homogeneous: members that
disagree on stored `task.crew` (including set vs unset) stay pending for
the next admission, so dispatch is never asked to reject a mixed-crew
bundle. The run pilots them five per partition; a burst of same-crew
filed tasks is prepared as one run rather than one run per task, and
`orbit clock tick --dry-run` lists the batch it would admit. Narrow the
predicate to, say, `statuses: [backlog]` or `require_tags: [pilot]` to keep the
scheduled pilot off proposals a human has not looked at yet. Unknown keys and
an empty `statuses` fail the definition closed. The resolved predicate is part
of each task's material fingerprint, so changing it re-prepares tasks assessed
under the old one; the default predicate keeps the fingerprints a workspace
accepted before the block existed. Explicit runs (`orbit run task-pilot <id>`)
ignore the block.

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
owner checkout on the host.

## Observe and control

Fires are ordinary runs — they appear in `orbit run history` under the actor
`routine/<name>`.

```bash
orbit routine pause <name>         # this host only; survives reboots
orbit routine resume <name>
```

## "Why didn't it fire?"

Resolve the toggles in this order — `orbit routine list` shows both at once:

1. `enabled: false` in the definition (versioned, affects every host).
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
security job defaults to high-severity Dependabot/code-scanning findings and
always considers secret-scanning findings; it skips dependency alerts with an
open Dependabot PR by default. Its catalog exposes per-source collection caps.
Secret values must not be copied into task prose or logs.

A missing GitHub client, authentication, or API permission is a capability gap,
not evidence of a clean repository. Read the collect/file step outcomes. CI
filing first reuses a still-open `ci-failure:<key>` owner, then a rejected
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
