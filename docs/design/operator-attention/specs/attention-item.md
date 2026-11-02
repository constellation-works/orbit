---
type: design
summary: "Spec: attention item kinds with their source of truth, the item and response schema, severity derivation and the next action"
last_validated: 2026-11-01
title: Spec — Attention item
owner: claude
status: Draft
feature: operator-attention
tags: [operator-attention, spec]
related_features: [operator-attention, human-decision-channel, distributed-drain]
related_artifacts: [ORB-21164]
last_updated: 2026-11-01
---

# Spec: Attention item

Every entry `orbit.attention` returns is one **item**: a closed `kind`, a derived `severity`,
the ids it concerns, the time it started needing attention, a one-line reason quoted from its
source, the exact action that resolves it, and a fingerprint that changes only when the item
materially changes. An item exists exactly while its source condition holds; the tool has no
store of its own and never reports a condition its source no longer shows.

## Why This Exists

Operator-waiting work lives in a dozen stores with a dozen queries ([1_overview.md §1](../1_overview.md)).
A single list is useful only if each entry says why it is there, how bad it is, how long it has
waited, and what to run. An entry that cannot be acted on, or that changes text on every call,
trains readers to ignore the list.

## Kinds and sources of truth

The kind set is closed in v1: adding a kind is a schema change to this spec and to the tool's
enum. Each kind has exactly one source; the tool reads it and decides nothing the source does
not already record. "Task-level" kinds are derived from one workspace's task, decision and
review records alone; "runtime" kinds need run, claim, provider or host state.

| Kind | Level | Source of truth (reused predicate) | Item exists while | `since` |
|---|---|---|---|---|
| `pilot_warning` | task | `held_pilot_warnings` in `crates/orbit-core/src/application/task/pilot/promotion.rs`, the predicate behind `orbit task list --pilot-warnings` | a `proposed` task's latest pilot receipt holds promotion on at least one warning finding | the latest pilot receipt comment's time |
| `pilot_hold` | task | `OrbitRuntime::pilot_admission_hold` (`task/decision.rs`) over `PilotAdmissionHold` (`task/pilot/admission.rs`) | the task's latest applied assessment holds admission: stale assessment, duplicate, already landed, operator validation, host operational, native OS or unverified finding | the history entry that recorded the hold; the open decision request's `raised_at` when none |
| `blocked` | task | `triage_candidacy` in `task/blocked_recovery/triage.rs`, over `current_block` and the final-recovery escalation | status is `blocked` and the block episode awaits a human: final recovery escalated, or its backstop is silent. A block that final recovery is still resolving mechanically is not an item | the time the task moved to `blocked` (`blocked_at`) |
| `decision_open` | task | `decision_requests_from_history`, effective state `open` (what `orbit.decision.list` returns) | an open request exists that no other item folds (see below): gates `agent_request` and `proposed_approval`, or any gate whose subject has no other item | `raised_at` |
| `review_waiting` | task | the review gate record (`crates/orbit-core/src/application/review/`) and the `review_approval` decision gate | status is `review`, no review run is active, and the task waits on an approval or on an exhausted review budget (`detail.waiting_on`: `approval` or `review_budget`) | the time the review settled |
| `run_failed` | runtime | the job-run store (`JobRun`, `orbit.workflow.run.list`) | the newest terminal run of a job (per task when it has one) is `failed`, `timeout` or `orphaned`, falls inside `run_window`, and nothing newer superseded it. The owning task is not terminal and is not already an item that names this run | `finished_at` |
| `run_stuck` | runtime | the job-run store: state `running` or `pending` | the run's owner process is gone (`pid` and `pid_start_time` no longer match a live process on the run's host) before reconcile has marked it orphaned, or it has run past `run_stuck_after` | `started_at` (or `scheduled_at` when pending) |
| `claim_stale` | runtime | the claim inspection behind `orbit.drain.claims` (`ClaimInspection`, `crates/orbit-store/src/contracts/task/coordination/settlement.rs`) | a live claim has an expired reservation, a bound run that is absent or terminal, an unresolved merge intent or an invalidated landing. These are diagnostics, not proof the worker died, so `reason` says "possibly stale" | the claim's last event time |
| `ci_red` | runtime | provider read of the workspace's base branch (the data `github.run.list` returns for `git_remote` and `base_branch`) | the newest completed run of a workflow on the base branch head is `failure` or `timed_out` and no newer run of that workflow succeeded | that run's completion time |
| `friction_untriaged` | record | the friction store, status `open` (`orbit friction list` default minus `triaged`) | the record's status is still `open` | `created_at` |
| `host_unhealthy` | runtime | `orbit host list` live probe (reachability, version, pull protocol), the default `orbit doctor` workspace check, and `orbit.drain.probe` refusals on a follower | a registered host does not answer or reports a skewed binary or protocol, or a default doctor check fails or warns | the check's recorded onset when the source has one, else `null` |

Notes that bind implementations:

- **Folding.** One condition yields one item. An open decision request folds into the item of
  the state it decides rather than becoming a second row: gate `pilot_admission` into
  `pilot_hold`, `block_triage` into `blocked`, `review_approval` into `review_waiting`. The
  folded item carries `refs.decision_id` and `expires_at`, and its `next` answers the request.
  A `decision_open` item exists only for requests with no other subject item. When the request
  expires or is superseded, the underlying item stays and `next` falls back to its
  decision-free action. `--kind decision_open` therefore lists only the unfolded requests;
  `--kind pilot_hold` lists every pilot hold, with or without a request.
- **`pilot_warning` and `pilot_hold` are separate.** A warning holds *promotion* of a proposed
  task; a hold gates *admission* on the latest assessment. They resolve differently (re-pilot
  versus an operator verdict), so a task can carry both.
- **A plain `proposed` task is not an item.** Awaiting approval is a normal state of most
  proposed tasks; listing them would drown the rest. See [3_vision.md §1](../3_vision.md#1-open-questions).
- **Runtime kinds respect the caller's read rights.** `run_*`, `claim_stale` and
  `host_unhealthy` read sources that require the Operator capability
  (`orbit.workflow.run.list`, `orbit.drain.claims`). A session without it gets the task-level
  kinds, and each skipped kind appears in `degraded` with code `capability` rather than
  silently missing ([4_decisions.md](../4_decisions.md#a-derived-view-never-widens-who-may-read-its-sources)).
- **Network sources degrade, they do not fail.** `ci_red` needs provider authentication
  (`github.auth.status`); with none, or on timeout, it yields `degraded` code
  `source_unavailable`. A workspace with no recognisable remote contributes no `ci_red` and
  no degradation.
- **`ci_red` and repair tasks.** When Orbit's CI sweep has already filed a repair task for the
  failing run (the filing evidence in `task/ci_sweep_filing.rs`), the item sets `refs.task_id`
  to it and `next` points at that task instead of suggesting another filing.
- **No source, no kind.** The pilot found no durable "last activity" heartbeat on job runs.
  `run_stuck` is therefore the liveness check above plus a wall-clock bound, not a heartbeat
  stall; slice 2 confirms the pid check on each platform before claiming it.

## Item schema

Fields are stable names; the tool's JSON schema is the contract and the table is its
description. `required` fields are always present; others are omitted, not `null`, except
`since` (below).

| Field | Type | Meaning |
|---|---|---|
| `id` | string, required | Stable identity: `att_<kind>_<16 hex>`, a hash of `kind`, `host.machine_id`, `workspace.id` and the subject ids. The same condition keeps the same `id` across calls and across material changes. |
| `kind` | enum, required | One of the kinds above. |
| `severity` | enum, required | `critical`, `high`, `medium` or `low`; derivation below. |
| `workspace` | object, required | `{id, name}`; `selector` is added in federated results. Items with no workspace (an unreachable host) use `null` for both and carry `host`. |
| `host` | object, required | `{machine_id, name}` of the host whose state the item describes. |
| `refs` | object | Subject ids that apply: `task_id`, `run_id`, `claim_id`, `decision_id`, `friction_id`, `pull_request`. |
| `task` | object | For items with a `task_id`: `{status, tags, crew, orchestrator, title}`. This is what the task filters match against. |
| `since` | RFC 3339 string or `null`, required | When the condition began. `null` only for `host_unhealthy` checks whose source records no onset; such items skip the age rules and never match `--older-than`. |
| `age_seconds` | integer | `generated_at - since`; omitted when `since` is `null`. Not part of the fingerprint. |
| `expires_at` | RFC 3339 string | Present when a decision request is attached. |
| `reason` | string, required | One line, at most 240 characters, quoted or trimmed from the source (a finding, an escalation line, a run's failing step, a doctor check message), ending in `…` when truncated. The tool does not paraphrase. |
| `detail` | object | Kind-specific, bounded, machine-readable context (`hold`, `findings[]` with their finding kinds, `basis`, `waiting_on`, `workflow`, `phase`, `checks[]`). |
| `next` | object, required | The action that resolves the item; below. |
| `fingerprint` | string, required | `fp_` plus 16 hex; see [attention-cursor](./attention-cursor.md#fingerprint). |

### `next`

`next` is `{summary, tool, input, command, needs?, also?}`:

- `summary` is a short imperative ("Re-pilot after fixing the cited warning").
- `tool` and `input` are the exact MCP call; `command` is the same call rendered for the CLI.
  `tool` and `input` are omitted when no MCP tool performs the action (the operator-only
  `orbit decision answer`, `orbit run cancel`, `orbit task update --approve`); `command` is
  always present. Both resolve ids and carry `--workspace` (and `--host` for a non-local host) so the text is
  copyable from where the reader sits.
- `needs` lists arguments only the operator can supply (a `--reason`, a `--request` key, a
  chosen `--phase`). They are left as `<placeholder>` in `command` and absent from `input`;
  Orbit never invents an audit reason.
- `also` is an array of further `next` objects: the answers of a folded decision request, or a
  read action such as `orbit task show`.

| Kind | Primary `next` |
|---|---|
| `pilot_warning` | `orbit.task.repilot` after the cited material is fixed; `also`: `orbit.task.show` |
| `pilot_hold` | `orbit decision answer` on the folded request, the recommended answer first when it has one; with no request, `orbit.task.repilot` for a stale assessment, otherwise `orbit.task.show` (the hold text names the verdict comment) |
| `blocked` | `orbit decision answer` on the folded request; else `orbit.task.requeue`; `also`: `orbit.task.show` |
| `decision_open` | `orbit.decision.show` (read the options); `also`: one `orbit decision answer` per allowed answer |
| `review_waiting` | `orbit task update <id> --approve` for `approval`; `orbit.task.review_reset` (needs `lineage`, `reason`) for `review_budget` |
| `run_failed` | `orbit.workflow.run.resume` when the run is resumable, else `orbit.workflow.run.show`; `also`: run logs |
| `run_stuck` | `orbit.workflow.run.show`; `also`: `orbit run cancel` |
| `claim_stale` | `orbit.task.recover_claim` (needs `reason`, `request`, observed `phase`); `also`: the claim inspection |
| `ci_red` | the repair task's `orbit.task.show` when one exists, else `github.run.view` on the failing run |
| `friction_untriaged` | `orbit.friction.update` with `status: triaged` (the operator edits tags or body first when needed) |
| `host_unhealthy` | `orbit.drain.probe` or `orbit host show` for diagnosis, plus the failing doctor check's own fix hint when it names one |

`next` is a suggestion with the right shape, never an action the tool takes. Choosing among
`also` answers, supplying `needs`, and running the call are the operator's.

## Severity

Severity is derived, never stored, by one rule applied per kind:

```
severity = min(ceiling(kind), raise(base(kind), age, conditions))
```

`age` raises the base by one level at each threshold in `attention.age_bump_hours` (default
`[24, 72]`), starting from the `since` time. `conditions` are the explicit raises in the table.
A kind never exceeds its ceiling. `critical` is reserved for conditions that stop other work
and are not waiting on a single task.

| Kind | Base | Ceiling | Explicit raises |
|---|---|---|---|
| `pilot_warning` | medium | high | high when a finding is a contract conflict |
| `pilot_hold` | medium | high | high when another task lists this one in `blocked_by` |
| `blocked` | high | high | none |
| `decision_open` | medium | high | request `risk_tier` high; and one level when `expires_at` is within 6 hours |
| `review_waiting` | low | high | `review_budget` waits start at medium |
| `run_failed` | medium | high | high when the run held a claim |
| `run_stuck` | medium | high | high when the run holds a claim |
| `claim_stale` | high | high | none |
| `ci_red` | high | critical | critical after the age thresholds are both passed |
| `friction_untriaged` | low | medium | none |
| `host_unhealthy` | high | critical | critical when the host is the owner of an in-scope workspace, or its failure is the reason another item is degraded |

The thresholds and `run_window`, `run_stuck_after`, `ci_cache_seconds`, `source_timeout_seconds`
and `per_workspace_item_cap` live in an `[attention]` config table with the defaults named
here. Severity is part of the fingerprint, so an item that ages from medium to high re-surfaces
once in a delta feed ([attention-cursor](./attention-cursor.md)).

Ordering of the list: `severity` descending, then `since` ascending (`null` last), then `id`.

## Response envelope

```json
{
  "schema_version": 1,
  "generated_at": "2026-11-01T22:10:03Z",
  "filters": { "workspace": ["orbit"], "kind": ["pilot_warning", "blocked"], "severity": "medium" },
  "items": [ ],
  "counts": { "matching": 4, "returned": 4, "truncated": false,
              "by_kind": {"pilot_warning": 3, "blocked": 1},
              "by_severity": {"high": 1, "medium": 3} },
  "cursor": { "next": "atc1.…", "status": "none" },
  "hosts": [ { "machine_id": "hm_…", "name": "dk-server-2", "reachability": "reachable" } ],
  "degraded": [ { "scope": "workspace:orbit", "kind": "ci_red", "code": "source_unavailable",
                  "message": "GitHub authentication missing" } ],
  "message": "4 items need attention"
}
```

- `filters` echoes the resolved filters (workspaces as `{id, name}`), so an empty or surprising
  result is explainable.
- `cursor.status` is `none` (no `since` supplied), `accepted`, `expired`, `invalid` or
  `unsupported`; see [attention-cursor](./attention-cursor.md).
- `hosts` is present when more than the local host was in scope.
- `degraded[].code` is one of `capability`, `source_unavailable`, `source_timeout`, `item_cap`
  (per kind and workspace) or `unreachable_destination`, `tool_not_on_this_host` (per host,
  the federated routing identities of [attention-federation](./attention-federation.md)).
- **Empty is explicit.** With no items, `message` is `Nothing needs attention` followed by the
  echoed filters. When `degraded` is non-empty the message is instead `No items found; N
  sources could not be read`, so an empty list is never read as all clear while a source was
  blind.
- Output follows the global `--format` / `--json` modes: `table` shows
  `SEVERITY KIND WORKSPACE REF AGE REASON`, with `next.command` on a second line under
  `--format plain` and `--verbose`; `json` and `ndjson` emit the envelope (ndjson emits one
  item per line, then a final `{"summary": …}` line carrying `counts`, `cursor`, `degraded`).

## Agent Signature

Specified by claude in [ORB-21164].

## Task References

- [ORB-21164] — specified the attention item contract

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
