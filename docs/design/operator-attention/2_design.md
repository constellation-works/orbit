---
title: Operator Attention — Design
owner: claude
last_updated: 2026-11-02
last_validated: 2026-11-02
status: Draft
feature: operator-attention
doc_role: design
type: design
summary: "Design of orbit attention: derived-view architecture, code placement, tool and CLI surface, configuration, relationship to existing reads, and implementation slices"
tags: [operator-attention, operator-ux]
paths: ["crates/orbit-core/src/application/attention/**", "crates/orbit-types/src/tool/attention.rs", "crates/orbit-common/src/governance/**", "crates/orbit-cli/src/command/**", "crates/orbit-mcp/src/federated/**"]
related_features: [operator-attention, human-decision-channel, federated-mcp, host-registry, distributed-drain]
related_artifacts: [ORB-21164]
---

# Operator Attention — Design

This document describes how `orbit attention` fits together and where it lands in the code.
The task-level feed, local scopes, filters, cursor and shared configuration are implemented in slice 1; runtime, provider and cross-host sources remain later slices. The prescriptive contracts live in the specs:
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
  the only time-dependent outputs are `age_seconds` and the severity thresholds, and the
  fingerprint ignores time except through `severity`.

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

| Concern | Location (proposed) |
|---|---|
| `AttentionItem`, `AttentionKind`, `AttentionSeverity`, `AttentionNext`, `AttentionEnvelope`, `AttentionCursorStatus` types | `crates/orbit-types/src/tool/attention.rs`, re-exported by the existing `tool` domain |
| The operation spec: tool `orbit.attention`, CLI `orbit attention`, filter params, description | the planned `attention.rs` module under `crates/orbit-common/src/governance/`, in the shape of `governance/decision.rs` (operations-as-data; no clap or runtime types) |
| Derivation, folding, severity, fingerprint, filters, cursor codec, local multi-workspace aggregation | `crates/orbit-core/src/application/attention/` (`sources/` per kind, `derive.rs`, `severity.rs`, `fingerprint.rs`, `filter.rs`, `cursor.rs`, `aggregate.rs`) |
| Tool registration, MCP read-only annotation, scope | the operation spec's `mcp_scope: WorkspaceRequired` for a bound session; `orbit-tools/src/mcp_annotations.rs` lists `orbit.attention` as `READ_ONLY` |
| Authorization | a catalog entry allowing `Agent` and `Operator`; the runtime kinds check capability themselves and degrade ([attention-item](./specs/attention-item.md#kinds-and-sources-of-truth)) |
| Cross-host fan-out and merge | `crates/orbit-mcp/src/federated/` (the same client code the CLI's routed calls use) |
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

An `[attention]` table, all keys optional:

| Key | Default | Meaning |
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

Each slice ships a usable feed. Daniel dropped the session hook; [ORB-21165] is archived and is not a dependency or deliverable. Dependencies run top to bottom. Task ids are in Task
References below; each is a proposed `ws_orbit` task tagged `operator-attention` with acceptance criteria.

1. **Tool, CLI, filters and cursor over the task-level kinds** ([ORB-21168]). Item types and schema;
   `pilot_warning`, `pilot_hold`, `blocked`, `decision_open` (with folding) and
   `review_waiting`; severity and `next`; every filter in the filter spec; fingerprint and the
   `--since` cursor; single-workspace and local `--all`; goldens, skill reference and docs.
   Owns the FULL shared `[attention]` configuration: typed settings, validated defaults, registry, resolution and readback for every §5 key. Later slices consume this owner and create no competing schemas.
2. **Runtime kinds: `run_failed`, `run_stuck`, `claim_stale`** ([ORB-21169]). Capability-gated sources with
   `degraded` reporting for sessions without the Operator capability; liveness check
   confirmed per platform. Consumes the shared §5 settings from slice 1. Depends on slice 1.
3. **Provider and record kinds: `ci_red`, `friction_untriaged`** ([ORB-21170]). Adds the cached provider
   read and the friction adapter; offline degrade. Consumes the shared §5 settings from slice 1. Depends on slice 1.
4. **Federation and `host_unhealthy`** ([ORB-21171]). Authority-per-host derivation, the cross-host
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
- **`run_stuck` has no heartbeat.** Jobs record a pid and a start time, not progress. The kind
  is a liveness check plus a wall-clock bound, which misses a live but wedged process inside
  the bound. A progress signal would be a separate change to the run store.
- **`review_waiting` uses settled evidence.** Status must be `review`, the review certificate must name the lineage’s latest attempt under its current reset, that attempt must be `Settled`, and no attempt may be `Open` or have `reviewer_running`. A passing verdict awaits approval; a nonpassing verdict with `remaining_at(now).seconds == 0` awaits a budget reset. `since` is the certificate’s `issued_at`. A missing or unreadable certificate/ledger reports `source_unavailable`; a status or approval request alone is insufficient evidence of settlement.
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

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
