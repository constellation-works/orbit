---
title: Fleet Notices — Design
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-14
status: Draft
feature: fleet-notices
doc_role: design
type: design
summary: "Design of fleet notices: a stored owner record, the NOTICE operation noun and notice toolset, managed-agent posting, brief delivery, configuration, the maintenance dispatch-hold extension point, and implementation slices"
tags: [fleet-notices]
paths: ["crates/orbit-common/src/governance/**", "crates/orbit-core/src/application/notice/**", "crates/orbit-types/src/record/notice.rs", "crates/orbit-store/src/driver/sqlite/migration/**", "crates/orbit-types/src/tool/mcp_toolset.rs", "crates/orbit-core/src/adapter/engine_host/actions/task_context.rs", "crates/orbit-engine/src/activity_job/cli_runner/orchestrator/dispatch/preparation.rs"]
related_features: [fleet-notices, operator-attention, federated-mcp, mid-run-steering, distributed-drain, operations-as-data]
related_artifacts: [ORB-21207]
---

# Fleet Notices — Design

This document describes how fleet notices fit into Orbit and where they land in the code. All five slices
([ORB-21211], [ORB-21212], [ORB-21213], [ORB-21214], [ORB-21215]) are built; only the maintenance dispatch hold (§7) is unfiled. The prescriptive contracts
are the specs: [notice-record](./specs/notice-record.md) (fields, effective state, expiry, dedupe),
[notice-tools](./specs/notice-tools.md) (tools, CLI, toolset),
[notice-authority](./specs/notice-authority.md) (poster classes, rights, limits),
[notice-delivery](./specs/notice-delivery.md) (briefs, dashboard, attention) and
[notice-federation](./specs/notice-federation.md). Open directions are in
[3_vision.md](./3_vision.md).

## 1. A stored record, not a derived view

Unlike [orbit attention](../operator-attention/2_design.md#1-a-derived-view-over-existing-state),
a notice is new information: the fact "agents keep hitting bug X" exists nowhere else in Orbit until
someone posts it. So notices are a stored record in the owner's coordination store, written only
through governed operations and audited like tasks and friction.

What *can* be derived is derived. Whether a notice is in force is computed on read from its row, the
clock and its linked task ([notice-record](./specs/notice-record.md#effective-state)), following the
decision-request precedent where supersession and expiry are derived when read. Nothing has to run
for a notice to stop being in force; the closure materializer only tidies rows.

The record is generic. Its vocabulary (scope, kind, author class, linked task, expiry) names Orbit
concepts. The optional `hosts` list names machine ids on maintenance notices and is display- and
filter-only in v1; no field, kind or default hardcodes a particular host or client. The Orbit
Office scribe's `#board` routing was proposed as a client in its own repository; [ORB-21208]
remains someday work.

## 2. Placement in the code

Layering follows [ARCHITECTURE.md](../../../ARCHITECTURE.md). No new cross-crate dependency is
needed: every crate below already depends on the ones it would call.

| Concern | Location |
|---|---|
| `Notice`, `NoticeKind`, `NoticeScope`, `AuthorClass`, `EffectiveStatus`, `PostOutcome` | `crates/orbit-types/src/record/notice.rs` |
| The `NOTICE` noun: post, close, list, show; params, descriptions, CLI | `crates/orbit-common/src/governance/notice.rs`, in the shape of `governance/decision.rs` (operations as data) |
| Registration in the `notice` toolset | `operation::register_noun(registry, &NOTICE)` in `crates/orbit-tools/src/builtin/orbit/register.rs` (the toolset comes from the `builtin_mcp_toolset` table in `crates/orbit-tools/src/mcp/toolsets.rs`); `McpToolset::Notice` in `crates/orbit-types/src/tool/mcp_toolset.rs`; `orbit.notice.list`/`show` annotated read-only in `crates/orbit-tools/src/mcp/annotations.rs` |
| Authorization catalog entries for `post` and `close` (Agent, Operator and Runner as an identity floor, like leases; poster-class resolution in core; `list` and `show` are open reads) | `crates/orbit-common/src/governance/authorization/catalog/operations.rs` |
| Store table and migration; repository contract | the `notices.rs` migration (schema v85) under `crates/orbit-store/src/driver/sqlite/migration/`, `crates/orbit-store/src/contracts/notice.rs`, `crates/orbit-store/src/driver/sqlite/notice_store.rs`, the store schema golden |
| Post, close, list, show, effective state, dedupe, limits, poster class | `crates/orbit-core/src/application/notice/` |
| Closure materializer | `materialize_notice_closures_for_done_task` in `application/notice/close.rs`. A transition into `done` calls it through `task::TaskNoticePort` (`close_linked_notices_for_done_transition`), right after the transition rather than inside its transaction; the hook keeps the helper's best-effort log. Reads derive the closure for anything it missed; the scheduler sweep (`run_notice_closure_tick` from `application/routines/sweep.rs`) materializes cross-workspace closures |
| Brief block | the required `RuntimeHost` method `notices_for_agent_input`, implemented by Core in `actions/task_context.rs`, with its `NullRuntimeHost` default; attached in `dispatch/preparation.rs`, emitting `NoticesDelivered`; a claimed leaf reads through `route_worker_host_tool("orbit.notice.list", …)` |
| Fixed limits | `crates/orbit-core/src/application/notice/limits.rs` |
| CLI | registry-derived `orbit notice`; `crates/orbit-cli/src/command/notice.rs` holds the CLI-only flag folding and the rendering |
| Dashboard banner and view | `crates/orbit-web` (`assets/dashboard/js/notices.js`, `css/notices.css`) |
| Attention kind `notice_open` | `crates/orbit-core/src/application/attention/notice_sources.rs` |

The core service is one set of functions over a runtime handle. The tool handlers, the CLI, the
dashboard, the brief builder and the attention adapter all call it, so they cannot disagree about
which notices are open.

## 3. Surfaces

- **Tools**: `orbit.notice.post`, `orbit.notice.close`, `orbit.notice.list`, `orbit.notice.show`
  ([notice-tools](./specs/notice-tools.md)). `post` is idempotent for a repeat of its latest write —
  a repeat changes no stored field, including `expires_at` and `updated_at`, for an editor or a
  corroborator, and is never refused with `notice_expiry_invalid` when a retried absolute
  `expires_at` is now less than 5 minutes ahead (it still returns `updated` or `corroborated`) —
  and `close` for a repeat close; `list` and `show` are read-only.
- **MCP**: advertised only with the opt-in `notice` toolset (`orbit mcp serve --toolset notice`,
  or `machine.mcp.toolsets = ["notice"]`, or `all`), like `leases` and `experiments`. The default
  toolset is unchanged.
- **`orbit tool run` and activities**: reach the tools whatever the toolset, as for every opt-in
  toolset ([notice-tools](./specs/notice-tools.md#the-notice-toolset)).
- **CLI**: `orbit notice post|close|list|show` in the *Operate* group.
- **Brief**: the `fleet_notices` block ([notice-delivery](./specs/notice-delivery.md)).
- **Dashboard**: banner and Notices view. **Attention**: kind `notice_open`.
- **Skill**: the `orbit` skill's references gain a short "post a notice" section for orchestrators
  (when to post, scope, dedupe key, linking the fix, closing it) and one line telling implementers
  how to treat the brief block. Skill docs are edited under `crates/orbit-core/assets/skills/` and
  synced.
- **Goldens**: CLI help, MCP descriptions, the toolset list, the config key description and the
  store schema golden change in the slices that add them.

Naming: a lease notice (`orbit.lease.notices`) is an event in one lease's stream
([path-leases 2_design.md §5](../path-leases/2_design.md#5-notices)); a fleet notice is a posted
record. The noun is `notice` because the operator chose `--toolset notice`; the glossary and both
tools' descriptions say which is which ([references/glossary.md](./references/glossary.md)).

## 4. Managed agents posting

Opted-in managed activities *read* notices through their brief. Posting needs the tool:

- Slice 3 (built) adds `orbit.notice.post`, `orbit.notice.show` and `orbit.notice.close` to the
  tool baselines of `agent_implement`, `agent_review_repair` and `final_recovery` for closing the
  agent's own notice, resolved in `resolve_activity_tool_denials`/`resolve_activity_tools`.
  Nothing else is granted.
- When the resolved activity tools include a notice tool, the dispatcher stamps the `notice` toolset
  onto the run's own MCP server, so the agent
  sees the tool whatever the host's `machine.mcp.toolsets`.
- The owner resolves such calls as poster class `agent` from the run binding and applies the agent
  rules: home workspace = the task's, workspace scope only, `incident`/`info` only, linked task or
  evidence required, 24 h cap, per-task rate and open counts
  ([notice-authority](./specs/notice-authority.md#rights)).
- Agent posting is always on: the `notice.agent_posting` switch was removed [ORB-22808], so
  `notice_agent_posting_disabled` comes only from an older owner.

The activity instruction for posting is short: post only when the problem is outside your task, will
affect other agents, and you have a linked task or evidence; prefer linking the fix task; check
open notices first (the brief shows them); re-posting the same subject
corroborates rather than duplicates.

## 5. Brief delivery

Delivery is specified in [notice-delivery](./specs/notice-delivery.md). The design points:

- The block is attached in `dispatch/preparation.rs` through one new `RuntimeHost` capability, so
  the engine stays free of store reads.
- It is rebuilt at every prompt build, so a notice posted or closed mid-run takes effect at the next
  safe point.
- Brief delivery is independent of the MCP toolset
  ([4_decisions.md](./4_decisions.md#brief-delivery-is-a-workspace-switch-not-the-toolset)) and
  always on: its `notice.brief_delivery` switch was removed [ORB-22808].
- Delivery is audited (`NoticesDelivered`) and fails open with an `unavailable` marker.

## 6. Configuration

None. Every limit is a fixed constant in `application/notice/limits.rs`; the former `[notice]`
keys were removed [ORB-22808] and load with a deprecation warning until 2026-12-08:

| Former key | Fixed value | Meaning |
|---|---|---|
| `brief_delivery` | always on | Quote open notices into opted-in activities' prompts |
| `agent_posting` | always on | Managed agents may post and close their own notices |
| `brief_max_items` | `5` | Notices per brief |
| `brief_body_chars` | `600` | Body characters per brief entry |
| `brief_read_timeout_ms` | `2000` | Bound on the brief's notice read |
| `default_expiry_hours` | `{incident = 72, maintenance = 12, info = 168}` | When a post names no expiry |
| `max_expiry_hours` | per class, [notice-record](./specs/notice-record.md#expiry) | Caps; operator cap ≤ 720 |
| `max_title_chars`, `max_body_bytes`, `max_evidence` | `120`, `4096`, `8` | Size limits |
| `max_open_per_workspace`, `max_open_global_per_owner`, `max_open_per_agent_task` | `20`, `10`, `2` | Open-count limits |
| `agent_posts_per_task_per_day`, `orchestrator_posts_per_hour` | `5`, `30` | Rate limits |
| `retention_days` | `30` | How long closed and expired rows stay listable |

Machine configuration gains only the new `notice` value in `machine.mcp.toolsets`.

Kinds, who may post or close, the brief's trust marking and these bounds are code and spec, so
every workspace's notices mean the same thing.

## 7. Extension point: maintenance dispatch hold

Holding dispatch while an Orbit update is in progress is the obvious next use, and it is **not v1**.
v1 builds nothing that acts on a notice. It fixes the contract a later slice implements, so v1's
data can carry it without a migration:

- **Input.** Effective-open `maintenance` notices authored by `operator` or `orchestrator` (never
  `agent`), with a non-empty `hosts` list, carrying a new `hold: "dispatch"` field that the
  extension adds. v1 refuses `hold` as an unknown field, so no v1 notice can be mistaken for a hold.
- **Reader.** One admission source beside the host-shutdown probe
  ([distributed-drain 2_design.md §7.4](../distributed-drain/2_design.md#74-host-shutdown-hold)):
  on a host whose `machine_id` is in `hosts`, unattended admission (drain waves, pull-drain claim
  requests, scheduler-minted auto-tasks) holds with reason `notice_maintenance_hold: <notice id>`;
  explicit operator commands are admitted with a warning, as for a scheduled shutdown.
- **Release.** The hold lifts when the notice closes or expires, with no other write. It never
  cancels live runs.
- **Surfaces.** `orbit run readiness`, `orbit doctor` and `orbit run show` name the notice, exactly
  as they name a scheduled shutdown.
- **Reads.** A follower reads holds for itself from the owner on its admission pass; an unreachable
  owner does not create a hold (the existing pull-drain already cannot claim then).

This keeps the authority rule intact: a notice still grants nothing, and the only effect the
extension adds is to *withhold* new unattended work on named hosts, bounded by the notice's expiry.

## 8. Slices

Each slice is a `ws_orbit` task tagged `fleet-notices` with acceptance criteria (all five done);
dependencies run top to bottom, and ids are in Task References.

1. **Notice store, tools, CLI and the `notice` toolset** ([ORB-21211], done). Types; store migration and
   repository; core post/close/list/show with effective state, expiry defaults and caps, dedupe and
   corroboration, linked-task closure on read; poster-class resolution and every right and limit in
   [notice-authority](./specs/notice-authority.md) (the agent rules are enforced here; slice 3
   grants the tools to managed agents, section 4); `McpToolset::Notice` and `machine.mcp.toolsets`; registry-
   derived CLI; audit rows; `[notice]` configuration; goldens; skill reference. Single-host,
   owner-local reads.
2. **Brief delivery** ([ORB-21212], done). The `RuntimeHost` capability, the `fleet_notices` block, the
   `notice_instruction` on the opted-in activities, `NoticesDelivered`, fail-open marker, the
   `brief_delivery` switch, the claimed-run owner read. Depends on 1.
3. **Managed-agent posting** ([ORB-21213], done). Activity tool baselines, toolset stamping onto the run's MCP
   server, the posting instruction, the `agent_posting` switch. Depends on 1; its end-to-end check
   (one agent posts, a second run's brief shows it) depends on 2.
4. **Federation** ([ORB-21214], done). Follower writes through the owner route, routed reads, the
   per-owner fan-out for workspace-less lists with `degraded` hosts, global aggregation, the
   closure materializer in the sweep for cross-workspace links, the federated-mcp doc amendment.
   Depends on 1. It uses the federated host's destination probing and owner routing but keeps
   workspace-less notice-list aggregation in `notice_aggregate.rs`; it does not share the
   attention fan-out's aggregation implementation ([ORB-21171]).
5. **Dashboard and attention** ([ORB-21215], done). Banner, Notices view and forms; the `notice_open`
   attention kind with its severity rule and the attention spec amendment. Depends on 1, and on
   [ORB-21168] for the attention part.

The maintenance dispatch hold (§7) is deliberately not filed; it waits for v1 to be used.

## 9. Concerns & Honest Limitations

- **Orchestrator authority is attribution.** The `orchestrator` class rests on self-declared crew
  attribution. The rights it gets are bounded to what expires within days and cannot touch operator
  notices, but a misconfigured unmanaged session can post as an orchestrator. A real orchestrator
  credential would be a separate change to session authority.
- **Untrusted data is still read.** Quoting and an instruction reduce, not remove, the chance that a
  notice's text steers an agent. The mitigations are structural (agents post only about their own
  task, with evidence, in their workspace, for a day; operators' notices cannot be closed by agents)
  plus the audit trail tying every agent-authored word to one run.
- **Global reaches one owner's briefs.** A brief reads only its workspace's owner. Multi-owner fleets
  must post global notices on each owner they mean ([notice-federation](./specs/notice-federation.md#scope)).
- **Mid-step notices wait for a safe point.** A run that never rebuilds its prompt never sees a notice
  posted after dispatch; it can still read one with `orbit.notice.list` when granted.
- **Dedupe keys are only as good as the poster.** Two agents describing one bug in different words
  and without a shared linked task produce two notices. Deriving the key from the linked task, and
  the posting instruction's "check the brief first", are the mitigations; an orchestrator can close
  the duplicate.
- **Expiry can end a true notice.** A notice whose truth outlasts its expiry disappears; someone must
  re-post it. That is the intended failure direction: a stale notice misleads everyone, a missing one
  only fails to help.
- **No acknowledgement.** Orbit records delivery, not that an agent understood or obeyed a notice.

## Task References

- [ORB-21207] — designed fleet notices and filed the implementation slices
- [ORB-21208] — proposes Orbit Office scribe `#board` routing to the notice tools
- [ORB-21168] — attention slice 1, which the `notice_open` kind builds on
- [ORB-21171] — attention federation, whose fan-out the notice list reuses
- [ORB-21211] — slice 1: notice store, tools, CLI and the `notice` toolset
- [ORB-21212] — slice 2: brief delivery as untrusted data
- [ORB-21213] — slice 3: managed-agent posting
- [ORB-21214] — slice 4: federation and global aggregation
- [ORB-21215] — slice 5: dashboard banner and view, attention kind
- [ORB-22808] — removed the `notice.*` config switches, making agent posting and brief delivery always on

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
