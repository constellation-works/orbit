# Dispatch and completion authorization

## Choose the requested delivery behavior

Default shipping ends in `review`. In PR mode it prepares a PR and stops with it
unmerged; submission or PR creation does not prove a merge. In local mode it
may commit and merge to the configured base before the task reaches `review`,
so review is not a pre-merge stop. Use `--complete` only when the user has
authorized delivery through `done`:

```bash
orbit run ship <task-id> --complete
orbit run ship <task-id> --allow-crew <crew-a>,<crew-b>
orbit run auto --for 3h --concurrency 7 --complete
orbit run auto --for 3h --concurrency 7 --complete --allow-crew <crew-a>,<crew-b>
```

The numbers and crews are examples, not defaults or standing policy. Use the
requested duration, concurrency, and configured crews. Without completion
authorization, omit `--complete`. If the user will start the drain themselves,
prepare work without launching a second coordinator.

Prefer the connected MCP submission tool when it supports the requested
options. `orbit_workflow_ship` intentionally has no completion input and always
submits review-only work. If completion is authorized, use `orbit run ship
<task-id> --complete` through an available authorized operator command surface
on the owning host; do not invent a `completion` argument, silently submit a
review-only run, or use a local shadow store. See
[tool-surface.md](../../orbit/references/tool-surface.md).

`--complete` is default-off and applies to that submitted run:

- PR mode requires a verified merged PR before `done`.
- Local mode requires merge and push before `done`.
- Validated no-diff work may complete without a PR.
- Auto completion covers every task admitted during the window, including
  tasks prepared after it starts. Do not use a whole-backlog drain when only
  named tasks were authorized.
- It does not promote proposed tasks, bypass repository protections, or
  authorize another window. Enabling GitHub auto-merge is not proof of merge.

Use the configured base branch and ship mode unless the user requests an
explicit override. Inspect effective inputs; `--base` and `--mode` are deliberate
overrides. Never replace a user's branch choice with a hardcoded convention.
Full delivery semantics: [orchestration.md](../../orbit/references/orchestration.md).

## Keep authorized work moving

Under a continuous-completion policy, do not insert an unrequested blocking
review stage before merge. Post-merge review, QA, and CI feed new repair tasks
through pilot and promotion. Existing branch protections still apply.

`--concurrency` sets the drain's worker limit. Inspect active leaf runs and
resource pressure before changing capacity; count system activities as well
when assessing provider cost and host load. Do not start a second drain merely
to raise capacity. Discover whether the installed version supports a live
update, and follow its advertised command and authority requirements.

`--allow-crew` is an **allowlist**: it permits the named configured crews and
excludes others. On an explicit `ship`, an excluded task is refused before its
run is created; on an auto drain, excluded backlog tasks are skipped. Neither
path automatically remaps a task.
It is scoped to the run and checked against resolved crew identity, including
system activities; it does not cancel already-running workers. Diagnose with:

```bash
orbit run readiness --allow-crew <crew-a>,<crew-b> --json
```

Changing a task's crew is a separate operator action. Preserve explicit user
choices, including a request to leave one existing expensive run alone.
For a provider restriction during a window, use the recovery sequence in
[walkthroughs.md](walkthroughs.md); do not reset the clock or drop `--complete`.

## Window expiry and stopping

The window bounds new admissions. Tasks admitted before its deadline retain
their original completion authority and may finish afterward. Expiry is not
cancellation, and an unfinished worker is not permission to extend the drain.
Verify the coordinator's persisted deadline and terminal state rather than
estimating from the conversation clock.

A user stop overrides the supervision plan. Stop new actions as requested and
state whether workers remain active; do not conflate stopping supervision
with cancelling workers. For an authorized cancellation, inspect ownership and
target only the intended run:

```bash
orbit run show <run-id> --json
orbit run cancel <run-id>
```

Cancelling a parent may affect children. Inspect that behavior before replacing
a coordinator; preserve unrelated workers and the original deadline. If no
safe supported replacement exists, report the limitation. Do not work around
it with overlapping drains or direct store edits.

Further new work needs authorization that covers it. Existing explicit repair
or deployment instructions may be broader than a drain window; evaluate the
actual session scope rather than asking for approval already given. A later
unrelated request does not inherit a previous window's completion permission.

## Report an evidence-backed handoff

Record the workspace/host, coordinator deadline and outcome, live child run
IDs, completed task/PR evidence, queued work and its blockers, and the next
step. Include operational changes requiring restoration and any pending
binary install, asset sync, or service restart. Task/run comments can preserve
necessary recovery context; do not substitute an agent summary for live state.

Measure clearly defined cohorts:

- PRs merged during the window versus runs started and finished inside it.
  Identify pre-existing work that landed during the window.
- Pipeline duration or task cycle time, with the actual start/end definition;
  report sample size and median/p90 where useful.
- Failed attempts, retries, cancellations, and eventual recovered tasks
  separately, so one task is not counted twice as delivery.

For historical comparisons use the same branch, timezone, and duration.
Squash merges make raw commit counts incomparable to histories with merge
commits; report merged PRs or first-parent landings alongside raw counts.
Do not claim equal task difficulty, quality, or human-only authorship from
commit counts. Never infer spend from token usage or invent missing costs.

## Scoped operation-mode grants

`orbit operation enable --task <ids> --for <window> --right prepare,promote[,complete]`
records one bounded, attributable authorization for a finite task set;
`orbit run auto --grant <ID>` starts a drain whose every admission rechecks
it. `--complete` and `--grant` are separate mechanisms and cannot be
combined. A grant never authorizes merge, and the `complete` right is
refused unless the workspace sets `operation.delivery_cap = "done"`. Use
`orbit operation explain` to see each effective setting with its source and
the limiting reasons before enabling anything; `stop` ends new admissions
and `revoke` also withdraws completion from admitted work.
