---
title: Operation Mode — Operations
owner: claude
last_updated: 2026-09-07
last_validated: 2026-09-07
status: Accepted
feature: operation-mode
doc_role: operations
type: design
summary: Shipped operation-mode contract — typed preferences, scoped grants, grant-bound drains, bounded recovery, surfaces, observability, and rollback.
tags: [operation-mode, automation, authorization, recovery, operations]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/operation/**", "crates/orbit-store/src/driver/sqlite/operation/**", "crates/orbit-automation/src/members/**"]
related_features: [automation-triggers, activity-job, routines]
related_artifacts: [ORB-11332, ORB-11331, ORB-11330]
---

# Operation Mode — Operations [ORB-11332]

This file describes what shipped. [The vision](./3_vision.md) keeps the
remaining proposals. Nothing here activates automation: a preference edit
changes no schedule and grants no authority, every shipped routine and
auto-task definition stays disabled, and no existing run or `--complete`
invocation is reinterpreted.

## 1. Preferences: `[operation]` in `config.toml`

Preferences resolve **built-in supervised → global → workspace → run**. An
explicit `preset` at a layer resets the preset-managed fields to that
preset's defaults *before* that layer's own explicit fields apply; an
omitted preset preserves inherited fields. The review fields and the
delivery cap are independent: a preset never resets or infers them. Unknown
keys and out-of-range values fail config load.

| Key | Values (default) | Preset-managed |
| --- | --- | --- |
| `operation.preset` | `supervised` (default), `autonomous` | selector |
| `operation.preparation` | `manual` / `automatic` (supervised `manual`, autonomous `automatic`) | yes |
| `operation.preparation_due_seconds` | 1..=86400 (300) | yes |
| `operation.leaf_ceiling` | 1..=500 (supervised 5, autonomous 10) | yes |
| `operation.promotion` | `separate_approval` / `automatic` (supervised `separate_approval`, autonomous `automatic`) | yes |
| `operation.completion` | `review` / `done` (supervised `review`, autonomous `done`) | yes |
| `operation.recovery` | `existing` / `scheduled` (supervised `existing`, autonomous `scheduled`) | yes |
| `operation.recovery_episodes_per_task` | 0..=10 (2) | yes |
| `operation.recovery_minutes_per_task` | 1..=1440 (30) | yes |
| `operation.review_policy` | `none` (default), `after-landing`, `before-pr` | no |
| `operation.review_crew` | crew name | no |
| `operation.delivery_cap` | `review` (default), `done` | no |

`orbit config show` lists the explicit `operation.*` values with their file
provenance; a preset-managed key reset by a workspace preset shows `null`
with a `built-in` source because no explicit value survived. The *effective*
values and their winning source (`workspace`, `global`, `preset:autonomous@workspace`,
`run`) come from `orbit operation explain`.

`before-pr` is accepted in configuration so the contract exists, but it is
not supported at admission: enablement refuses it until the review gate
ships. `delivery_cap` defaults to `review`, so an autonomous `completion =
done` preference is capped at review until the workspace explicitly raises
the cap. The cap is disclosed in the explanation.

## 2. Authority: grants

```sh
orbit operation explain [--preset autonomous ...]     # preview; changes nothing
orbit operation enable --task ORB-1,ORB-2 --for 2h --right prepare,promote
orbit operation list | show <ID> | stop [--id <ID> --if-revision N] | revoke [...]
orbit run auto --grant <ID> [--for 30m] [--concurrency N]
```

Enablement is the one moment authority is created. It validates a finite,
non-empty task set (proposed or backlog tasks, at most 50), a window of at
most 24 hours, and at least one right; it resolves the effective policy
once (config layers plus the request's run layer), refuses an explicit
escalation past the delivery cap (`--completion done` or `--right complete`
under `delivery_cap = review`), refuses `before-pr`, captures the numeric
limits (leaf ceiling bounded by the leaf job's hard limit), and persists
the grant with its versioned policy snapshot. One active grant per
workspace; enabling a replacement requires stopping the old one first. The
`enable`, `stop`, and `revoke` verbs are governed operator operations on
every surface.

A grant carries separate rights. `prepare` accelerates in-scope preparation
in the shared state evaluator; `promote` lets fresh positive evidence move
proposed in-scope work to backlog; `complete` lets a bound drain capture
`done` when the cap allows it. No right authorizes merge or bypasses
dependencies, reservations, capacity, task approval rules, or repository
gates.

## 3. Grant-bound drains and admission

`orbit run auto --grant <ID>` submits the existing `workspace_auto_pipeline`
with an `operation` snapshot in its immutable run input: grant id and
revision, policy version, absolute expiry, effective completion, and captured
limits. The window is the intersection of the request and the grant's
remaining time; the ceiling is the intersection of the request, the captured
ceiling, and the job's hard limit; completion is `done` only with the
`complete` right and a `done` effective completion. Ordinary job input that
names the reserved `operation` key is refused.

Each classifier iteration rechecks the live grant: stop, expiry, and
revocation set `free_slots` to zero and close the drain window
(`expired_reason` becomes `grant_stopped`, `grant_expired`, or
`grant_revoked`); only the finite scope is offered; and fresh positively
assessed proposed work inside the scope is promoted first (see §4). The
Store rechecks everything again inside the transaction that creates each
detached child: grant admission at that instant, exact revision, scope,
an unclaimed task, and free leaf capacity. Refusals carry a reason
(`grant_stopped`, `grant_expired`, `grant_revoked`, `grant_revision_changed`,
`outside_grant_scope`, `task_claimed`, `capacity_saturated`) in the child
dispatch output and the `pipeline.invoke` audit row. Children inherit
exactly the parent's snapshot at that path; nested gate and PR children
inherit it transitively, so completion and recovery see the same bounds.

Live concurrency changes on the coordinator narrow the captured ceiling but
never widen it. Restart reads the same snapshots; retuning global or
workspace preferences affects future grants only. A replacement window sees
the previous grant's still-live children as claims and cannot double-claim
their tasks.

## 4. Promotion evidence

Promotion under a grant needs, for the exact current task version: the
`promote` right with an automatic promotion preference in the captured
policy, satisfied dependencies, a task the preparation contract deems
eligible (no no-diff disposition), and an accepted preparation assessment
from the shared state consumer whose `ready` flag is set and whose resulting
material fingerprint equals the task's current fingerprint at the current
landing-branch head. The write happens under the task lock after a fresh
recheck, records an `operation_promoted` history event, and audits
`operation.promotion` with the decision. Withheld reasons are reported per
task: `assessment_missing`, `assessment_unready`, `assessment_stale`,
`unmet_dependency`, `special_disposition_withheld`, `promote_right_missing`,
`promotion_separate_approval`, `status_changed`, `grant_no_longer_admits`.

## 5. Stop, expiry, revocation

| Control | New admissions and promotion | Admitted work |
| --- | --- | --- |
| Expiry (absolute deadline, derived at every check) | refused (`grant_expired`) | keeps captured bounds, including completion |
| `stop` (compare-and-set, replay is `unchanged`) | refused (`grant_stopped`); bound drains get the existing admissions stop | keeps captured bounds |
| `revoke` | refused (`grant_revoked`); bound drains stopped | loses privileged actions: the guarded `review -> done` transition is refused at the transition itself |

None of these cancel running children; `orbit run cancel` remains separate.
A revoked grant keeps its stop evidence. Neither transition resets a
deadline or a budget.

## 6. Bounded recovery

A per-task ledger in the host store spans engine step-recovery hooks,
resumed runs, and terminal-run triage. Before a recovery hook is dispatched
for a run carrying an `operation` snapshot, Core reserves an episode (a retry
of the same run and step reuses it) and later settles its wall time; crashes
and timeouts count. Triage reserves an episode when it lists a bound
candidate and settles it when dispositions apply. When the captured
`recovery_episodes_per_task` or `recovery_minutes_per_task` is spent, the
hook is skipped with the original error authoritative, triage lists the task
under `exhausted` with the reason, and the task gains a durable
`recovery_budget_exhausted` history event (recorded once). Runs without a
snapshot keep the pre-existing unbounded behavior. Provider token/cost caps
are not enforced: usage remains unknown.

## 7. Scheduling constraints

Operation mode owns no timer. The operator-enabled state routine remains the
single cadence owner. With an admitting grant, Core hands the shared
evaluator `MemberConstraints`: the grant scope and a due interval
(`preparation_due_seconds` for preparation members when the preference is
automatic and the grant carries `prepare`; zero for incidents when recovery
is scheduled). In-scope members become due sooner; every other member keeps
the routine's own timing. The explanation names the cadence owners and
reports `no_enabled_preparation_routine` / `no_enabled_triage_routine` when
a preference has no owner to act through.

## 8. Surfaces and observability

- CLI/MCP: `orbit operation explain|enable|list|show|stop|revoke`
  (`orbit.operation.*`; `show` is CLI-only), `orbit run auto --grant`, and
  `orbit run readiness`, whose `capacity.operation` block and per-task
  reasons (`outside_grant_scope`, `grant_*`) reflect a live grant-bound drain.
- Dashboard: the Operations → Auto-drain view has an **Operation Mode** panel
  showing every field with its source, the grant, caps, limiting reasons,
  and governed Stop/Revoke controls with compare-and-set. Enablement is
  deliberately CLI/MCP only.
- Audit: `operation.grant` (enabled/stopped/revoked/rejected),
  `operation.promotion`, `operation.recovery`, `operation.completion`,
  plus the existing `pipeline.invoke` and admissions-stop rows. Run inputs
  carry the snapshot; the classifier's `operation` output and the drain
  window's `expired_reason` explain each iteration. Usage and cost stay
  unknown unless measured elsewhere.

## 9. Compatibility and rollback

Existing installs, custom jobs and routines, and omission of `--complete`
behave exactly as before; `--complete` remains the separate blanket
authorization and cannot be combined with `--grant`. Runs without an
`operation` snapshot are never bound. To roll back: `orbit operation stop`
(or `revoke`) the active grant, let admitted children settle, and remove
`[operation]` keys if desired; readable grant, ledger, and audit records
remain. An older binary rejects the unknown `[operation]` keys at config
load and cannot enforce grants, so stop or revoke with a supporting binary
before downgrading.

## 10. Still proposed

Standing or dynamic scopes, federation-wide enrollment, `before-pr` review
and content-specific coverage, provider cost reservations, and automatic
migration of legacy sweeps remain in [the vision](./3_vision.md).
