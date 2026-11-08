---
title: Fleet Notices — Overview
owner: claude
last_updated: 2026-11-05
last_validated: 2026-11-05
status: Draft
stage: shipped
next: "—"
feature: fleet-notices
doc_role: overview
type: design
summary: "Fleet notices: short-lived, scoped, authored coordination notes (incident, maintenance, info) held by the owner, reached through an opt-in notice MCP toolset and quoted into agent briefs as untrusted data"
tags: [fleet-notices, federated-mcp, operator-attention]
paths: ["crates/orbit-common/src/governance/**", "crates/orbit-core/src/application/notice/**", "crates/orbit-types/src/record/notice.rs", "crates/orbit-store/src/driver/sqlite/notice_store.rs", "crates/orbit-core/src/adapter/tool_host/notice_tools.rs", "crates/orbit-cli/src/command/notice.rs", "crates/orbit-store/src/driver/sqlite/migration/**", "crates/orbit-types/src/tool/mcp_toolset.rs", "crates/orbit-core/src/adapter/engine_host/actions/task_context.rs", "crates/orbit-mcp/src/federated/**", "crates/orbit-cli/src/command/host_route.rs", "crates/orbit-core/src/application/routines/sweep.rs"]
related_features: [operator-attention, federated-mcp, mid-run-steering, distributed-drain, path-leases, human-decision-channel]
related_artifacts: [ORB-21207, ORB-21208, ORB-21164, ORB-21211, ORB-21214, ORB-21215]
---

# Fleet Notices — Overview

A fleet notice is a short, scoped, authored note that the whole fleet should see until it stops
being true: "the review gate refuses every candidate with X, fix in flight", "Orbit binary update
on all hosts, hold off on new drains until 14:00Z". Notices are ordinary Orbit state, held by the
owner host of the workspace they are posted in and read by followers. They are posted, closed and
listed through `orbit.notice.post`, `orbit.notice.close`, `orbit.notice.list` and
`orbit.notice.show` (CLI `orbit notice …`), advertised on MCP only with the opt-in
`--toolset notice`. Open notices in scope are quoted into managed agents' activity briefs as
untrusted data, shown on the dashboard and listed by `orbit attention`. Every notice expires;
one with a linked task closes when that task is done unless `close_on_linked_done` is false.
Open notices are deduplicated by subject. A notice informs; it never grants authority, changes a
task's mandate or holds dispatch in v1.

## 1. Motivation

When the operator is offline, orchestrators and managed agents run the fleet with no shared place
to coordinate. Two situations from 2026-11-02 motivate the feature:

- **A critical bug every agent hits.** Each agent that hits it rediscovers it, often files a
  duplicate task or friction record, and sometimes "repairs" around it in its own diff. One
  announcement, visible to every agent until the fix lands, would replace all of that.
- **An Orbit binary update across hosts.** Polaris RB-15 deploys every host in lockstep. Until it
  is done, orchestrators on other hosts should not start drains. Today the only channel is the
  operator's own messages, which agents on other hosts never see.

Existing records do not fit. A task is work with a lifecycle and a crew; a friction record is a
retrospective problem report; a decision request asks a human a question; a lease notice is an
event on one path lease. None is "a fact everyone should know, for a while, in this scope". The
operator's direction (2026-11-02) was that this belongs in Orbit as an optional MCP feature,
`--toolset notice`, rather than in a constellation-specific board ([4_decisions.md](./4_decisions.md)).

## 2. Core Concepts

- **Notice** — one record: `scope`, `kind`, `title`, `body`, `author`, an optional linked task,
  `expires_at`, and open/closed state ([specs/notice-record.md](./specs/notice-record.md)).
- **Home workspace** — the workspace whose owner store holds the notice. Every notice has one,
  including a global notice.
- **Scope** — `workspace` (applies to the home workspace) or `global` (applies to every workspace
  whose readers reach the home owner).
- **Kind** — `incident` (something is broken now), `maintenance` (planned work that others should
  hold off or plan around), `info` (anything else worth knowing).
- **Effective state** — `open`, `closed` or `expired`, computed on read from the stored state, the
  clock and the linked task's status.
- **Dedupe key** — the subject identity. Posting a key that is already open updates or
  corroborates that notice instead of opening a second one.
- **Poster class** — `operator`, `orchestrator` or `agent`, derived from the caller's session, not
  chosen by it ([specs/notice-authority.md](./specs/notice-authority.md)).
- **Brief block** — the bounded `fleet_notices` section a managed agent's prompt carries, marked as
  untrusted data ([specs/notice-delivery.md](./specs/notice-delivery.md)).

Terms are defined in [references/glossary.md](./references/glossary.md).

## 3. At a Glance

All five slices ([ORB-21211], [ORB-21212], [ORB-21213], [ORB-21214], [ORB-21215]) are built: the notice store, the
`orbit.notice.*` tools, `orbit notice`, the opt-in `notice` toolset, brief delivery of open
notices into opted-in agent prompts as untrusted data, and managed-agent posting through a
run's own task (always on since [ORB-22808] removed the `notice.agent_posting` switch). Slice 4
makes notice tools `control_plane` so a replica refuses them, routes follower CLI and worker-bound calls to
the owner by selector and owner route, aggregates a workspace-less list with one owner-local read
per owner machine (an unreachable owner is a `degraded` entry), and has the scheduler sweep
materialize same-owner cross-workspace linked-task closures. Slice 5 adds the dashboard banner and
Notices view and the `notice_open` attention kind ([2_design.md §8](./2_design.md#8-slices)).

| Concern | File | Task |
|---------|------|------|
| Notice record, effective state, dedupe, expiry | [specs/notice-record.md](./specs/notice-record.md) | [ORB-21207] |
| Tool and CLI contracts, `notice` toolset | [specs/notice-tools.md](./specs/notice-tools.md) | [ORB-21207] |
| Poster classes, close rights, limits | [specs/notice-authority.md](./specs/notice-authority.md) | [ORB-21207] |
| Brief injection as untrusted data, dashboard, attention | [specs/notice-delivery.md](./specs/notice-delivery.md) | [ORB-21207] |
| Owner holds, followers read, global aggregation | [specs/notice-federation.md](./specs/notice-federation.md) | [ORB-21207] |
| Toolset enum the `notice` toolset joins | [mcp_toolset.rs](../../../crates/orbit-types/src/tool/mcp_toolset.rs) | [ORB-21207] |
| Prompt task context the brief block joins | [task_context.rs](../../../crates/orbit-core/src/adapter/engine_host/actions/task_context.rs) | [ORB-21207] |
| Notice record type | [record/notice.rs](../../../crates/orbit-types/src/record/notice.rs) | [ORB-21211] |
| Store migration and SQLite queries | [notice_store.rs](../../../crates/orbit-store/src/driver/sqlite/notice_store.rs) | [ORB-21211] |
| Operation noun and tool specs | [governance/notice.rs](../../../crates/orbit-common/src/governance/notice.rs) | [ORB-21211] |
| Post, close, list, show and poster authority | [application/notice/](../../../crates/orbit-core/src/application/notice/mod.rs) | [ORB-21211] |
| Tool adapter | [notice_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/notice_tools.rs) | [ORB-21211] |
| `orbit notice` CLI | [notice.rs](../../../crates/orbit-cli/src/command/notice.rs) | [ORB-21211] |
| Notice tool classes (`control_plane`) | [capability.rs](../../../crates/orbit-mcp/src/federated/capability.rs) | [ORB-21214] |
| Workspace-less per-owner aggregation | [notice_aggregate.rs](../../../crates/orbit-mcp/src/federated/notice_aggregate.rs) | [ORB-21214] |
| Follower CLI owner routing | [host_route.rs](../../../crates/orbit-cli/src/command/host_route.rs) | [ORB-21214] |
| Cross-workspace closure sweep | [sweep.rs](../../../crates/orbit-core/src/application/routines/sweep.rs) | [ORB-21214] |
| Slices and dependencies | [2_design.md §8](./2_design.md#8-slices) | [ORB-21207] |
| Proposed client: Orbit Office scribe `#board` routing | — (separate repository) | [ORB-21208] |

## Task References

- [ORB-21207] — designed fleet notices and filed the implementation slices
- [ORB-21208] — proposes Orbit Office scribe `#board` routing to the notice tools
- [ORB-21164] — designed `orbit attention`, which lists open notices as a kind
- [ORB-21211] — slice 1: notice store, tools, CLI and the `notice` toolset (done)
- [ORB-21212] — slice 2: brief delivery as untrusted data (done)
- [ORB-21213] — slice 3: managed-agent posting (done)
- [ORB-21214] — slice 4: federated reads and writes, global aggregation, sweep materializer (done)
- [ORB-21215] — slice 5: dashboard banner and view, attention kind (done)

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
