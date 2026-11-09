---
title: Operator Attention — Design
owner: claude
last_updated: 2026-11-04
last_validated: 2026-11-04
status: Draft
feature: operator-attention
doc_role: design
type: design
summary: "Design of orbit attention: derived-view architecture, code placement, tool and CLI surface, configuration, relationship to existing reads, and implementation slices"
tags: [operator-attention, operator-ux]
paths: ["crates/orbit-core/src/application/attention/**", "crates/orbit-types/src/tool/attention.rs", "crates/orbit-common/src/governance/**", "crates/orbit-cli/src/command/**", "crates/orbit-mcp/src/federated/**"]
related_features: [operator-attention, human-decision-channel, federated-mcp, host-registry, distributed-drain]
related_artifacts: [ORB-21164, ORB-21171]
---

# Operator Attention — Design

This document describes how `orbit attention` fits together and where it lands in the code.
The task-level feed, local scopes, filters, cursor and shared configuration are implemented in slice 1; federation and `host_unhealthy` in slice 4. Slice 2 implements the runtime sources; slice 3 implements the provider and record sources (`ci_red`, `friction_untriaged`). The prescriptive contracts live in the specs:
[attention-item](./specs/attention-item.md) (kinds, schema, severity),
[attention-filters](./specs/attention-filters.md),
[attention-cursor](./specs/attention-cursor.md) and
[attention-federation](./specs/attention-federation.md). Acknowledge and snooze, the dashboard
reading the same feed, notifications and plugin-contributed kinds are in
[3_vision.md](./3_vision.md).

## 1. A derived view over existing state

`orbit.attention` is a `read_only` tool. On each call it reads existing records and returns
the list of conditions that currently hold. There is no inbox, no item table, no cursor store
and no background job. Three properties follow and are tested as invariants:

- **Resolution is the source's.** An item disappears the moment its source condition stops
  holding: the task is re-piloted clean, the decision is answered, the run is superseded, the
  claim is settled. Nothing needs to "close" an item, and nothing can leave one stale.
- **One condition, one item.** The same underlying fact never appears twice. An open decision
  request folds into the item it decides ([attention-item](./specs/attention-item.md#kinds-and-sources-of-truth)).
- **Same inputs, same list.** Derivation is deterministic given the stores and `generated_at`;
  time-dependent outputs are `age_seconds`, condition thresholds (including reservation expiry
  and the stuck-run bound) and severity. Fingerprints cover condition state and severity, never
  elapsed time itself.

The kinds were chosen from conditions that already exist in Orbit's records and already have
a resolving action. A condition without a source (a stall detected by heartbeat, a notion of
"important" proposed task) is not a kind in v1; adding one means first giving it a source.

## 2. Derivation pipeline

For one workspace on one host:

```
scope ─▶ select kinds ─▶ read sources ─▶ derive items ─▶ fold ─▶ severity ─▶ fingerprint
```

- **Sources** are small adapters, one per kind, each wrapping the predicate the existing
  surface already uses (`held_pilot_warnings`, `pilot_admission_hold`, `triage_candidacy`,
  `decision_requests_from_history`, the claim inspection, the friction store). The adapters
  call these; they do not re-implement them, so `orbit attention --kind pilot_warning` and
  `orbit task list --pilot-warnings` can never disagree. Where a source has to be made
  reusable (some are `pub(crate)` or `pub(super)`), the slice widens it minimally and adds
  the shared predicate's regression check at the public boundary.
- **Fold** merges decision requests into subject items (one pass over the derived list).
- **Severity** applies the per-kind rule table; **fingerprint** hashes the per-kind state.
- Task-level adapters share one pass over the task index: a single task-envelope read gives
  status, tags, crew and orchestrator for filtering, and only tasks in a status a kind can
  occupy (`proposed`, `blocked`, `review`, and `backlog`/`proposed` for holds) are hydrated for
  comments or history. The existing `--pilot-warnings` residual in
  `application/task/listing.rs` already does the per-proposed-task comment read; the adapter
  reuses it rather than adding a second scan.

Failures are per source: an adapter that errors or times out contributes a `degraded` entry
for its kind and workspace and the rest of the list is returned.

## 3. Placement in the code

Layering follows [ARCHITECTURE.md](../../../ARCHITECTURE.md); no new cross-crate dependency is
needed, which the slice verifies before it merges.

| Concern | Location |
|---|---|
| `AttentionItem`, `AttentionKind`, `AttentionSeverity`, `AttentionNext`, `AttentionEnvelope`, `AttentionCursorStatus` types | `crates/orbit-types/src/tool/attention.rs`, re-exported by the existing `tool` domain |
| The operation spec: tool `orbit.attention`, CLI `orbit attention`, filter params, description | `crates/orbit-common/src/governance/attention/registry.rs` (operations-as-data; no clap or runtime types) |
| Derivation, folding, severity, local multi-workspace aggregation and the candidate read | `crates/orbit-core/src/application/attention/` (`sources.rs` for task-level and friction kinds, `runtime_sources.rs` for runs and claims, `provider_sources.rs` for `ci_red`, `host.rs` for local `host_unhealthy`) |
| Filters, cursor codec and fingerprint, and the public page (ordering, delta, limit, cursor minting) | `crates/orbit-common/src/governance/attention/` (`filter.rs`, `cursor.rs`, `page.rs`), beside the operation spec, so the local derivation and the federated aggregator apply one implementation |
| Default `orbit doctor` workspace checks for `host_unhealthy` | `WorkspaceCatalog::attention_doctor_checks`, implemented by the registry catalog in `orbit-cmd` (Core cannot call the doctor directly) |
| Tool registration, MCP read-only annotation, scope | the operation spec's `mcp_scope: WorkspaceRequired` for a bound session; `crates/orbit-tools/src/mcp/annotations.rs` lists `orbit.attention` as `READ_ONLY` |
| Authorization | a catalog entry allowing `Agent` and `Operator`; the runtime kinds check capability themselves and degrade ([attention-item](./specs/attention-item.md#kinds-and-sources-of-truth)) |
| Cross-host fan-out and merge | `crates/orbit-mcp/src/federated/attention_aggregate.rs` (the same federated client the CLI's routed calls use; `orbit attention` reaches it from `host_route.rs` preflight) |
| `[attention]` configuration | `crates/orbit-config` |

The core derivation function is a pure function of a runtime handle and a filter. The CLI,
the MCP tool and, later, the dashboard are thin callers of it, which is what keeps the web
dashboard reading "the same feed" a later, small step.

## 4. Surfaces

- **Tool**: `orbit.attention`, `read_only`, input exactly the filters in
  [attention-filters](./specs/attention-filters.md) in snake_case, output the envelope in
  [attention-item](./specs/attention-item.md#response-envelope). Seeing an item never changes
  it; every `next` is advice. The federated aggregator's destination hop adds one reserved
  input, `candidate_read`, which is not a filter, has no CLI flag, and is refused for every
  other caller ([attention-federation](./specs/attention-federation.md#candidate-read)).
  The federation slice adds that input with the fan-out. It is not part of the public filter
  surface the first slice ships.
- **CLI**: `orbit attention [filters]` in the *Observe* group beside `doctor` and `log`, with
  the global `--format` / `--json` modes. Help text and errors stand alone: no source paths,
  no task or friction ids in help, and the kind names are the user-visible vocabulary.
- **MCP**: one tool, no new transport. Annotated read-only so clients may auto-approve it.
  It belongs to `tasks-read`, so the operator, manager, resident, agent and reader roles
  all serve it, as does the default surface. Roles select tools independently of capability:
  Agent and Operator sessions can both read task-level kinds, and bound sessions still
  refuse scope widening.
- **Goldens**: CLI help and the MCP tool text are golden surfaces; the slice that adds them
  runs `make goldens`.
- **Skill**: the `orbit` skill's reference lists `orbit attention` as the first check for an
  orchestrator or on-call session (skill docs are edited under
  `crates/orbit-core/assets/skills/` and synced, per the workspace guide).

## 5. Configuration

The bounds below were an `[attention]` table of optional keys. [ORB-22808] fixed each at its
default; the keys load with a deprecation warning and are ignored until 2026-12-08.

| Former key | Fixed value | Meaning |
|---|---|---|
| `cursor_ttl_hours` | `24` | Cursor validity after `issued_at` |
| `age_bump_hours` | `[24, 72]` | Age thresholds that raise severity |
| `run_window_hours` | `48` | How far back `run_failed` looks |
| `run_stuck_after_minutes` | `120` | Wall-clock bound for `run_stuck` |
| `ci_cache_seconds` | `60` | Provider-read cache per workspace |
| `source_timeout_seconds` | `10` | Per-source read bound |
| `fanout_budget_seconds` | `20` | Outer deadline for a cross-host call |
| `per_workspace_item_cap` | `200` | Items kept per workspace |

Slice 1 owns `orbit_config::AttentionSettings` and all eight registry keys. Supported readback is `orbit config get attention.<key> --json` (effective values), `orbit config keys` (descriptions and types), and `orbit config show` (layer provenance). No setting is changed by querying attention.

Integer keys must be positive. Upper bounds are 8760 cursor hours, 87600 run-window hours, 525600 stuck minutes, 86400 cache seconds, 3600 source/fan-out seconds and 10000 workspace items. `age_bump_hours` accepts an empty list to disable aging; otherwise values must be strictly increasing positive hours up to 87600. Registry resolution validates these before admitting configuration. Slice 1 consumes cursor TTL, severity thresholds and item caps. Slices 2–4 own runtime/provider/fan-out behavior consuming the remaining fields.

Configuration changes thresholds only. It does not add kinds, change `next`, or change what
counts as a condition; those are code and spec, so two operators' lists mean the same thing.

## 6. Slices

[ORB-21164] designed the feed and filed its slices. Each slice ships a usable feed. Daniel dropped the session hook; [ORB-21165] is archived and is not a dependency or deliverable. Dependencies run top to bottom. Task ids are in Task
References below; all four implementation tasks are done.

1. **Done — tool, CLI, filters and cursor over the task-level kinds** ([ORB-21168]). Item types and schema;
   `pilot_warning`, `pilot_hold`, `blocked`, `decision_open` (with folding) and
   `review_waiting`; severity and `next`; every filter in the filter spec; fingerprint and the
   `--since` cursor; single-workspace and local `--all`; goldens, skill reference and docs.
   Owns the FULL shared `[attention]` configuration: typed settings, validated defaults, registry, resolution and readback for every §5 key. Later slices consume this owner and create no competing schemas.
2. **Done — runtime kinds: `run_failed`, `run_stuck`, `claim_stale`** ([ORB-21169]). Capability-gated sources with
   `degraded` reporting for sessions without the Operator capability; liveness check
   documented per platform (the probe and its limitations are described below). Consumes the shared §5 settings from slice 1. Depends on slice 1.
3. **Done — provider and record kinds: `ci_red`, `friction_untriaged`** ([ORB-21170]). Adds the cached provider
   read and the friction adapter; offline degrade. Consumes the shared §5 settings from slice 1. Depends on slice 1.
   The provider read stays behind `github.run.list` (Orbit stays generic); freshness binds to the
   fetched `origin/<base>` head ([ORB-21680]), pending or unknown observations are not green, and `since` is the
   failing run's `updated_at` labelled as an onset-time proxy (`detail.since_source`), because the
   provider projection supplies no exact completion timestamp.
4. **Done — federation and `host_unhealthy`** ([ORB-21171]). Authority-per-host derivation, the cross-host
   fan-out and merge, unreachable hosts as items, session-binding refusal, the
   federated-mcp doc amendment, and the local doctor / drain-probe sources for
   `host_unhealthy`. The fan-out hop is the candidate read in
   [attention-federation](./specs/attention-federation.md#candidate-read): the slice calls
   that shape and does not merge a public page, produced by an omitted `limit`, as the
   destination's candidate set. Depends on slice 1; its unreachable-host item is useful
   before slices 2 and 3 land. It consumes slice 1’s shared fan-out settings.

No session hook, automation, notification or live configuration change is authorized.

Slice 1 also measures per-source cost on the real workspace so explicit polling guidance in
[attention-cursor](./specs/attention-cursor.md#cost) rests on data.

## 7. Concerns & Honest Limitations

- **Recompute per call.** There is no incremental maintenance. The cost is bounded by task
  counts and, for `ci_red`, a cached network call. If a workspace grows to where a call takes
  seconds, the first fix is the shared task index, not a store of items.
- **No history, no "resolved" feed.** A derived view cannot say what used to be an item or
  how long it was open. Onset times exist only where a source records them; `host_unhealthy`
  often has none, and says so with `since: null`.
- **Thresholds are judgement.** The severity table and defaults are a first reading of how bad
  each condition is. They will be tuned from use; the fingerprint's inclusion of severity means
  a retune can re-inject items once.
- **Runtime observation never reconciles.** Run adapters read raw job-store rows, not the
  ordinary listing/show or resume planner (which can repair journals or interrupt runs).
  `run_failed` uses the newest submission per job and subject task, so a newer successful,
  pending or running attempt supersedes a failure. `run_window_hours` bounds failure history;
  active rows remain eligible for `run_stuck` beyond that window. Blocked items fold their
  referenced runs before runtime filtering. Runtime kinds require Operator capability, with
  one workspace-scoped `degraded` entry per selected unreadable kind otherwise.
- **`run_stuck` has no heartbeat.** `run_owner_liveness` compares the persisted pid/start token
  through the existing job owner classifier. Linux obtains stable process start identity from
  `/proc/<pid>/stat` and `/proc/stat` (boot time), including the PID namespace in v2 tokens.
  macOS reads libproc `proc_pidinfo(PROC_PIDTBSDINFO)` first, with a bounded UTC/C-locale
  `ps -o lstart=` fallback; legacy tokens use the existing legacy matcher. Both Unix paths
  use `kill(pid, 0)` as the fallback presence check: an unavailable identity probe with a
  present PID is not proof of death. A missing process or mismatched start token is stopped;
  a foreign PID namespace is unknown. Non-Unix liveness is unknown. Unknown identity still
  permits the wall-clock bound, never a dead-owner assertion. Pending rows without a recorded
  pid wait for the bound; running rows without an owner count as stopped. Only host-local run
  rows are probed. This detects dead owners before reconciliation records `Interrupted`, and
  live owners past `run_stuck_after_minutes`, but misses live wedged processes inside the bound.
  These are existing platform paths, not a new heartbeat or process-control implementation.
- **Claim inspection is diagnostic.** Only the workspace owner derives `claim_stale`, using
  the non-repairing inspection behind `orbit.drain.claims`. Unsettled claims qualify on expired
  reservation, absent/terminal host-local bound run, unresolved merge intent or invalidated
  landing. A bound remote run's absence from the owner's store proves nothing and is not a
  diagnostic. `since` is the inspection's last event time (`updated_at`). Advice says
  “possibly stale”; recovery is never called. `needs` uses exact tool keys `reason`,
  `request_key`, `expected_phase`, and `status` (operator chooses a lifecycle-valid status);
  the observed phase is exposed in `detail.phase`, not invented in the proposed input.
- **Resume advice is conservative.** Failed-run advice suggests generic resume only when
  a pinned or catalog definition is readable, no claim bound the run, input does not carry
  one-shot trusted-host/reconciliation/source-repair authority, and an interrupted run's owner
  is not confirmed alive. Other runs suggest inspection. The ordinary resume action retains
  all checkpoint, lineage and admission guards; attention never invokes its planner.
- **`review_waiting` uses settled evidence.** Status must be `review`, the review certificate must name the lineage’s latest attempt under its current reset, that attempt must be `Settled`, and no attempt may be `Open` or have `reviewer_running`. A passing verdict awaits approval; a nonpassing verdict with `remaining_at(now).seconds == 0` awaits a budget reset. `since` is the certificate’s `issued_at`. A `review` task with no review-gate certificate yields no item and no `degraded` entry: its review has not settled yet (not run, still running, or in owner landing), and a status or approval request alone is insufficient evidence of settlement. A stuck run behind it surfaces through the runtime kinds (`run_stuck`, `claim_stale`), not here. Real read failures still report `source_unavailable`: an unreadable artifact, an undecodable certificate, or a certificate whose lineage ledger is missing.
- **Pilot hold coverage follows the hold enum.** `pilot_hold` reflects the holds the pilot
  admission code knows. A new hold type that is not added to the adapter is invisible; the
  adapter matches the enum exhaustively so the compiler flags a new variant.
- **Fan-out adds a mux behaviour.** The federated mux gains its first merging call. It is
  confined to one named tool and fails visible, but it is new surface to keep honest.
- **Replica blindness.** A follower with its owner unreachable can show only its own runs
  plus a `host_unhealthy` item. That is intended: showing stale replica tasks as current would
  be worse.

## Task References

- [ORB-21164] — designed `orbit attention` and filed its slices
- [ORB-21165] — archived session-hook proposal; not a dependency
- [ORB-21168] — slice 1: tool, CLI, filters and cursor over the task-level kinds
- [ORB-21169] — slice 2: runtime kinds `run_failed`, `run_stuck`, `claim_stale`
- [ORB-21170] — slice 3: `ci_red` and `friction_untriaged`
- [ORB-21171] — slice 4: federation across workspaces and hosts, and `host_unhealthy`
- [ORB-21680] — made CI attention use the fetched base-branch head

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
