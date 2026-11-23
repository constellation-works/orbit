# Dispatch tasks and start a drain

## Contents

- [Dispatch authority](#dispatch-authority)
- [Entry points](#entry-points)
  - [Drain admission and capacity](#drain-admission-and-capacity)

## Dispatch authority

Dispatch and supervise authorized tasks. Use [workflows-run-jobs.md](workflows-run-jobs.md) only
when job/activity internals or a particular run command need explanation.

## Entry points

Use `orbit_workflow` action `ship` with explicit `task_ids`, `workspace`, and attribution
when driving an authoritative MCP connection. Observe with
`orbit_run action show/list`; resume eligible terminal work with
`orbit_run` action `resume`, which returns a new linked run. These operations
require operator authority. Managed leaf runs cannot dispatch follow-up runs.
See [tool-surface-routing.md](../../orbit/references/tool-surface-routing.md).

The CLI offers additional discovery modes below. Use them only where the user
and workspace dispatch policy permit auto-discovery; creating tasks or enabling
a filing routine does not itself authorize execution.

```bash
orbit run ship                      # ship ready backlog tasks through the gated pipeline
orbit run ship <task-id> ...        # ship exactly these
orbit run ship <task-id> --allow-crew sol # ship exactly these, only on Sol
orbit run ship --mode local         # implement in a worktree, merge to the base; no PR
orbit run auto --for 2h             # drain the backlog for a window
orbit run auto --for 2h --concurrency 8   # ... with 8 tasks in flight at a time
orbit run auto --for 2h --allow-crew opus,sonnet  # ... using only these crews
orbit run auto --host <owner-name> --pull <workspace-name-or-ws_id> --for 2h # on a replica
orbit run auto --stop                      # stop new admissions; children keep running; records a pause
orbit run concurrency <run-id> --set 7     # retune a live drain, without replacing it
orbit run extend <run-id> --for 4h         # lengthen a live drain's window in place
orbit run readiness                        # explain current auto-drain eligibility, read-only
orbit run readiness <task-id> --json        # explain selected task IDs as JSON
orbit run ship <task-id> --complete  # ... and also carry it through to `done`
orbit run ship <task-id> --review before-landing # ... and review the PR before it lands
```

`ship` with no IDs discovers ready backlog work itself. `--mode` defaults to the
workspace's registered ship mode (`pr` unless set otherwise), and `--base`
defaults to the registered workspace base branch, else `workflow.base_branch`.

An explicit task ID must be in `backlog` or `in-progress`: shipping a `blocked`,
`proposed`, `review` or `done` task is refused at submission, naming its status,
so return a blocked task to the backlog (`orbit task update <id> --status backlog`)
before shipping it again.

`orbit run ship --json` returns `workflow`, `job_id`, `run_id`, `state`,
`submitted_at`, and the captured `review` layer (`{timing, source}`, or null),
matching MCP ship and HTTP `POST /workflows/ship`. The CLI also retains its
one-shot dispatch fields: `wait_timeout: false`, `attempt: 1`, `error_code: null`
and `error_message: null`. Submission notices use `warning` and `note` strings
(one notice per line); a validation-environment warning also appears as
`validation_env_warning`.

Absent notices omit their keys. Text output keeps
its workflow/job/run/state and inspect lines, followed by warnings and notes.

Readiness gates and lock waits are drain policy. An operator's explicit ship
of named tasks (`orbit run ship <id>` or the dashboard's Ship) overrides them
and dispatches: a pending `not_before`, unassessed complexity, no
`context_files`, and context locks another in-flight task or reservation holds
(its gate takes a reservation holding no files and edits beside them). Once
the task is `in-progress`, its own locks hold against the drain as usual.

The
task's history records a `readiness_overridden` event naming the gates and the
lock holders it overrode. It still refuses a task this host cannot run (host
OS, `requires_labels`, a `ui` task without a verified browser kit), a
human-crew task, a task a live run, execution claim or task-pilot preparation
holds, an inherited-only epic root, a task-pilot finding, and an overlap with
a pull follower's live execution claim. `orbit.workflow` action `ship` does
not override readiness.

### Drain admission and capacity

`run auto` drains backlog leaf tasks for a bounded window of at most 24 hours.
The window bounds only the *start* of new work — a task already shipping when
it expires still finishes.

It re-lists the whole backlog every pass, so a slot is refilled as soon as its
own task finishes and a task filed mid-window starts without waiting for the
batch around it. For owner/local drains, `--concurrency` (5 by default) is a
workspace-wide bound: workers left running by stopped coordinators, explicit
`run ship` deliveries, and unsettled pull admissions all consume slots. A
wrapper and its delivery consume one slot together. Delivery jobs impose no
active-run limit of their own.

A replica's pull drain uses `--concurrency` as an admission ceiling with capped
before-landing review credit. Before each claim it counts
`occupied - min(reviewing, N)` against N. A leaf becomes reviewing after the
owner's persisted before-landing admission has `applies=true`; validation and
publishing before then count as implementing, while later review revalidation
and handoff remain reviewing. The credit is capped at N, but reviewing leaves
can exceed N as replacements reach review.

With an unchanged N and no
pre-existing excess, total occupied leaves stay at most 2N and implementing
leaves at most N. Lowering N preserves running leaves, so existing excess can
remain temporarily. `orbit run show` reports the leaf stages. Review rebuilds
still compete for repository build-budget slots, so N is not a hard total-live
task cap for pull drains.

Resource forecasting is an additional admission check, not a replacement for
that worker ceiling. Global `machine.auto_concurrency.mode` defaults to `off`;
`shadow` records what forecast fit would do while admitting as `off`, and
`enforce` uses forecast CPU/memory fit within the free slots left by the ceiling
and resource throttle.

Inspect read-only `orbit run profiles` for delivery
profiles and sparse fallback, `orbit run profiles --accuracy` for forecast
error and shadow decisions, and `orbit run show <drain-run-id> --json` for the
recorded pass. For one local drain, `orbit run auto --auto-concurrency <MODE>`
selects an explicit mode (`off`, `shadow` or `enforce`); omitting the flag
follows `machine.auto_concurrency.mode` each admission pass.

MCP
`orbit_workflow` action `auto` `mode: "start"` accepts the same `auto_concurrency`
values. Explicit overrides and inheritance source (`explicit` or `configured`)
survive restart/resume without writing configuration. Host thresholds and
provider-limit policy refresh next pass; unchanged config is cached by mtime,
and invalid config warns and retains the previous policies. Run show and readiness expose
`mode`, `source`, and the current `configured_mode`.

Pull drains refuse this
override; set the executor host's mode instead. No profiles means forecasting
adds no restriction. Preserve operator policy when explaining waits: this
per-drain choice does not authorize starting work or changing machine settings,
host thresholds, or the worker ceiling. Settings and limits:
[configuration-settings-workflow.md](../../orbit-setup/references/configuration-settings-workflow.md#common-settings).
