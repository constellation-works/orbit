# Operator attention: item kinds

Contents:

- [Review waits](#review-waits)
- [Red CI](#red-ci)
- [Untriaged friction](#untriaged-friction)
- [Runtime kinds](#runtime-kinds)
- [Host health](#host-health)
- [Fleet notices](#fleet-notices)
- [Stalled residents](#stalled-residents)
- [Tripped routines](#tripped-routines)
- [Standing-drain keeper](#standing-drain-keeper)

## Review waits

`review_waiting` also lists a task still in `review` whose newest delivery run ended `held` on a `delivery_*` refusal, or whose completion is held by a current `completion.hold` rule. A live delivery run (`pending`, `running` or `retrying`) produces neither of these, and a review task with no such hold and no settled review certificate still produces no `review_waiting` item.

`detail.waiting_on` is `delivery_hold` or `completion_hold`.

A delivery hold names the refusal code (`detail.code`), the pull request (`detail.pr_number` and `refs.pr_number`) and the run (`detail.run_id` and `refs.run_id`); for `delivery_forge_observation_unavailable` its `next` is `orbit.run` action `resume` with command `orbit --workspace '<ws>' job resume <run>`, and for any other `delivery_*` code it is `orbit.run` action `show` with command `orbit --workspace '<ws>' run show <run>`, each carrying `input` `{id: <run>, workspace: <ws>}`.

A completion hold names the rule (`detail.rule`), the field, the configured value and what matched (`detail.field`, `detail.pattern`, `detail.matched`); `next` is to merge the pull request and approve the task. A delivery hold is reported ahead of a completion hold on the same task.

A completion hold is low severity; a delivery hold is medium. Reading attention does not resume a run, merge a pull request or approve a task. The settled-certificate cases are unchanged: `waiting_on` `approval` (approve the review) or `review_budget` (reset the budget).

## Red CI

`ci_red` reads the provider behind `github.run.list` for the workspace base branch, cached per workspace for 60 seconds and bounded by a 10-second source timeout. The current base-branch head is the fetched remote-tracking branch (`origin/<base>`), not the local branch, which can lag behind it; advancing that head invalidates the cache. When no fetched head is available the source degrades as `source_unavailable` instead of treating any reported head as current.

Workflows are told apart by the provider's workflow id, so two workflow files that share a display name are decided separately (run numbers count per workflow); `detail.workflow` keeps the display name, and a provider that supplies no id falls back to the name. A workflow with a completed run on the current base-branch head is decided by its newest such run.

A workflow whose runs on the current head are all still pending is decided by its newest completed run on the base branch visible in the 100-run branch listing, whatever head it ran on, so a base that keeps landing while red stays red behind its pending newest head, and a newer green completion clears it; `detail.reported_head_sha` names the run's head and `detail.base_head_sha` the current head.

If the listing is full and has no usable completion for a current-head pending workflow, Orbit reports a workflow-specific degraded entry with code `source_unavailable` because relevant history may extend beyond the listing.

A workflow with no run on the current head is not decided: an older head's result alone is not current, and pending or unknown observations are never green. The deciding run of a workflow that failed or timed out is high severity and critical after both age thresholds. Its `since` is that run's `updated_at`, or its `created_at` when the provider supplies no usable update time. `detail.since_source` names the field used (`updated_at` or `created_at`), and `detail.since_is_proxy: true` labels it an onset-time proxy, not an exact completion timestamp.

When a CI-sweep repair task already exists for the failing run, `refs.task_id` names it and `next` shows that task; otherwise `next` is `github.run.view`. While a task commit is pending, the repair-task lookup is skipped rather than settling the pending commit: the red item still reports the failing run with `github.run.view`, and a `source_unavailable` degraded entry for `ci_red` names the pending commit as the reason the association is unavailable.

Missing provider authentication or a timeout yields `degraded` code `source_unavailable` without failing the call; on the workspace owner, a workspace with no recognisable remote contributes nothing and no degradation; a direct read on a replica checkout degrades with `source_unavailable` before the remote is checked, whether or not it has one; a candidate read (Orbit's own fan-out) on a replica contributes nothing and no degradation. `--exclude-kind ci_red` makes no provider call.

## Untriaged friction

`friction_untriaged` lists friction records still `open`, with `since` at `created_at`, low severity (medium after the first age threshold), and `next` as `orbit.friction` action `update` with `status: triaged`. Triaged and resolved records are not listed.

## Runtime kinds

Runtime kinds read stored runs without reconciliation. `run_failed` reports recent failed,
timed-out or interrupted runs, suppressing older attempts superseded by a newer submission
of the same job/task and runs already named by a blocked item. On the workspace owner a run
whose task is terminal is omitted.

A replica cannot read the owner's task status, so a failed
run that names a task is still listed and `run_failed` is `degraded` with code
`source_unavailable` (task records on a replica are not authoritative; read the workspace
owner). A failed run that names no task is not degraded for that reason.

A failed PR watcher is marked `detail.superseded` and directs the reader to the current task when the owner task points to a newer verified delivery for that task. The current attempt and unverified run IDs retain ordinary advice.
`detail.held_claim`
is true only while a claim for the run is still live. `detail.claimed_leaf` is true when the
run was a claimed leaf, including after the claim settled as `failed`, `landed` or `revoked`.
A settled claim does not present as held. Severity is high when `held_claim` is true.

`run_stuck` detects a gone
owner before reconciliation or a run beyond its bound:

- a run that declares a deadline (a
  drain's stamped or extended window, or an explicit deadline/timeout in its input) is stuck
  only past that deadline plus 120 minutes, or when its pipeline `updated_at` or step activity
  has stalled for 120 minutes — a drain inside its window whose pipeline is still advancing is
  not stuck, and its item names `declared_deadline` and `last_progress_at`;
- a run that declares
  no bound is stuck beyond 120 minutes.

Either cause also carries `detail.current_step_id` and `detail.current_step_status`: the id
and state of the latest step the run has recorded, so you can tell an active run from an
abandoned one without the operator-only `orbit.run` show. A step is recorded when it ends, so
this is the last finished step, and the step now in flight is not yet named. Both are null
when the run has recorded no step. No step output or log text is included.

Linux uses
`/proc` start identity with PID-namespace fencing; macOS uses libproc with a bounded `ps`
fallback. Unavailable identity is not proof of death; the age bound still applies. Active
runs remain visible beyond the 48-hour failure window.

`claim_stale` uses owner-side claim inspection and says “possibly stale”: reservation expiry,
a missing/terminal local bound run, unresolved merge intent or invalidated landing are
diagnostics. A remote run missing locally proves nothing.

Its recovery advice uses
`orbit.task.recover` action `recover_claim`, with `claim_id` and `workspace` known; `needs` declares `reason`,
`request_key`, `expected_phase` and `status` (a status the task lifecycle allows, such as blocked or backlog). Read `detail.phase` and inspect
the claim before supplying operator inputs. Attention never executes recovery, resume or
cancellation. Generic resume advice preserves the action's ordinary admission guards.

## Host health

Local `host_unhealthy` reports failing or warning default `orbit doctor` workspace checks, which include a follower's pull drain ending on protocol skew. Its severity is `critical` on the owner host only when a check errors; a result with warnings alone is `high`. On a federated read it also reports a registered host whose binary version or pull-protocol fingerprint differs from this one.

## Fleet notices

`notice_open` reports one item per effective-open fleet notice in the read workspace; closed and expired notices never appear, and each item's `since` is the notice's opening time. It is default-selected with the task-level kinds and `host_unhealthy`, and it is derived only on the workspace owner: a replica reports a `degraded` entry with code `source_unavailable` instead of notice items.

A resident's status line is never an item. Severity starts from the notice kind (`warn` and `incident` high, `maintenance` medium, `info` low) and steps up one level when the notice expires within two hours or its linked task sits terminal-but-unfinished (`rejected`, `archived`). `detail` carries `notice_kind`, the notice's own `scope` (`workspace` or `global`) and `expires_at`, and `reason` quotes the notice title.

`next` suggests `orbit.notice.show`, with `orbit.notice.close` (needing `reason`) as the follow-up. A notice with a linked task attaches its `task_id` ref and `task` metadata; a taskless notice carries only its `notice_id` ref and no `task` object, so the task filters (`status`, `tag`, `task`, `crew`, `orchestrator`) exclude it.

## Stalled residents

`resident_stalled` reports an active resident mandate (not paused, taken over, handed off or ended) with no checkpoint for twice its heartbeat, whose last three checkpoints are all `incomplete`, or whose workspace's cycle routine is disabled — no cycle can fire at all then, however recently it checkpointed. It is default-selected and derived only on the workspace owner.

Severity starts at medium and follows the age-bump ladder; `detail` carries `stall` and `last_checkpoint_at`, and `since` is the last checkpoint (or the first of the three incomplete ones). `next` suggests `orbit.resident.control` `wake` with `status_request` (needs `reason`), with `orbit.resident.show` as the follow-up.

When the workspace's resident cycle routine is disabled, the stall reason names that routine as the cause, `detail` carries `cycle_routine_disabled` and `cycle_routine_name`, and `next` instead suggests `orbit.routine.control` toggling that routine to enabled (no `reason` needed), since a `wake` cannot fire a cycle while the routine is off.

## Tripped routines

`routine_tripped` reports a routine whose circuit breaker is open on this host: its last three fires failed with the same error class, so the sweep stopped firing it. It is default-selected, high severity, and each checkout reports the routines its workspace defines. `detail` carries `error_class`, `last_run_id`, `trips` and `next_trial_at`; `since` is the trip time.

The sweep lets one trial fire through after a backoff (15 minutes, doubling per failed trial up to 4 hours) and on the first sweep after the host's Orbit build changes; a successful trial closes the breaker. `next` suggests `orbit routine breaker reset <name>` once the cause is fixed, with `orbit routine show <name>` and `orbit routine breaker bypass <name>` (one fire on the next sweep) as follow-ups.

## Standing-drain keeper

`drain_paused` and `drain_breaker_open` report the standing-drain keeper and exist only while `workflow.standing_drain.enabled` is set on the workspace owner; with the keeper off (the default) neither is ever raised. `drain_paused` (medium) reports a drain intent paused by hand for over 12 hours while `backlog` or `proposed` work waits; `detail` names `paused_by`, `source` and `reason`, and `since` is the pause.

`drain_breaker_open` (high) reports that the keeper stopped submitting because `max_failed_resubmits` of its drains ended failed or interrupted within an hour, and stays open until a hand start however much time passes; `detail` carries `failed_in_last_hour` (the failures inside the hour that tripped it) and `threshold`, and `since` is the newest failure.

Both are default-selected; `next` suggests starting a drain by hand (`orbit run auto --for 24h`), which lifts the pause or closes the breaker, with `orbit run readiness` as the follow-up. Reading them starts nothing.

## Stale owner base

`delivery_stale_base` reports held deliveries when the owner base diverges from origin or a behind checkout has tracked changes. Inspect the owner-only commits and preserve them and any tracked work before reconciling the base and retrying. The item uses local remote-tracking refs and clears when setup admits again.
