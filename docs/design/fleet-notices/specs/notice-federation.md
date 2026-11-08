---
type: design
summary: "Spec: where fleet notices live (the home workspace's owner), how followers read and write them, what global scope reaches, and linked tasks across workspaces"
last_validated: 2026-11-05
title: Spec — Notice federation
owner: claude
status: Draft
feature: fleet-notices
tags: [fleet-notices, spec, federated-mcp]
related_features: [fleet-notices, federated-mcp, host-registry, distributed-drain, operator-attention]
related_artifacts: [ORB-21207, ORB-21171]
last_updated: 2026-11-05
---

# Spec: Notice federation

Notices are federated like tasks. Every notice lives in exactly one store: the coordination store
of its home workspace, on that workspace's owner host. Writes are control-plane operations and are
accepted only there. Followers and other hosts read through the federated routes they already use;
no host keeps a replica copy of notices, and no reader substitutes a stale copy for an unreachable
owner.

## Why This Exists

A notice that a follower could write locally would split the board: dk-server-1's agents would see
one set and dk-server-2's another. A notice copied to replicas would keep showing "binary update in
progress" on a follower after the owner closed it. Holding notices where tasks are held, and reading
them the way tasks are read, avoids inventing a second ownership or replication model
([federated-mcp 2_design.md §3–6](../../federated-mcp/2_design.md)).

## Classes

| Tool | Federated class |
|---|---|
| `orbit.notice.post`, `orbit.notice.close` | `control_plane` (coordination-store write) |
| `orbit.notice.list`, `orbit.notice.show` with a workspace | `control_plane` (coordination-store read, like `orbit.task.list`) |
| `orbit.notice.list` without a workspace | aggregated by the caller (below); each hop is a `control_plane` read on an owner |

## Writes

A write names its home workspace. The federated gateway delivers it to the selector's destination;
a replica destination refuses it with `capability_refused`, as for `orbit.task.add`, and the
gateway does not redirect to the owner. The CLI on a follower addresses the owner by the federated
selector (`orbit notice … --workspace <selector>`; `orbit tool run` may spell it `--host` plus
`--workspace`, while on `orbit notice` itself `--host` names the machines a maintenance notice
concerns), the same way an operator files a task on the owner from a follower.

A managed agent on a follower posts through its worker binding's owner route, as it already does for
`orbit.task.update`, so its post lands on the owner with the binding's task and run attached. The
owner resolves the poster class from that binding ([notice-authority](./notice-authority.md#poster-classes)).

A post that times out after dispatch is `outcome_unknown`, as for any routed write. Re-sending the
same input is safe: the dedupe key makes it `updated` or `corroborated`, never a duplicate, and a
repeat of the notice's latest write is not counted twice ([notice-record](./notice-record.md#retries)).
A repeat is idempotent: it changes no `expires_at` or `updated_at` (for an editor or a corroborator)
and, because the repeat check runs before expiry validation, it is not refused with
`notice_expiry_invalid` when the original absolute `expires_at` is now less than 5 minutes ahead —
it still returns `updated` or `corroborated` with the stored expiry untouched.

## Reads

- **With a workspace**: delivered to that workspace's owner like a task read. A replica refuses.
- **Without a workspace** (`orbit notice list` on a host with registered hosts, or a federated MCP
  session): the caller enumerates the federated workspace list, keeps one destination per owner
  machine, and issues one owner-local aggregate read per owner in parallel, each with `all: true`
  and the item filters but without `limit`. It merges, deduplicates by `id`, orders, then applies
  `limit`. An owner that does not answer becomes a `degraded` entry naming the host and the
  workspaces it owns; it never yields a silently shorter list.
- `orbit attention` federation landed in [ORB-21171](../../operator-attention/specs/attention-federation.md),
  but the notice list still has its own per-owner fan-out in
  `crates/orbit-mcp/src/federated/notice_aggregate.rs`; the two paths have not been folded together.
  The notice fan-out issues one untruncated owner-local read per owner, then merges, deduplicates,
  orders and applies the notice limit. Owners are the machines the listed workspaces name as their
  owner: a reachable follower that owns nothing listed is read through its owners and is never a
  `degraded` entry, while a configured destination that does not answer, and an owner with no
  configured destination, both degrade.

## Scope

- `scope: workspace` applies to the home workspace only.
- `scope: global` applies to every workspace. It is still stored in one home workspace, which
  decides who owns it, which store enforces dedupe and limits, and which owner's sweep materializes
  its closure.

What a reader actually sees of a global notice depends on what it reads:

| Reader | Global notices seen |
|---|---|
| `orbit notice list` / dashboard / `orbit attention` with all hosts | every reachable owner's |
| A managed run's brief | those homed on the owner of the run's workspace ([notice-delivery](./notice-delivery.md#which-notices)) |

The brief deliberately reads one owner: dispatch must not fan out across hosts or wait on an
unrelated host. In a fleet with one owner for every workspace (constellation's dk-server-2), the two
rows coincide. In a fleet with several owners, a global notice reaches the briefs of the owner it is
homed on only; an operator who needs it everywhere posts it on each owner. This is a documented
limitation ([2_design.md §9](../2_design.md#9-concerns--honest-limitations)), not a fallback.

## Linked tasks

`linked_task` must resolve on the home owner: a task in the home workspace, or in another workspace
that the same host owns. A task on another owner host is refused (`notice_invalid_field`), because
the owner could not evaluate effective closure without a cross-host read on every list. The
cross-workspace case is materialized by the owner's sweep ([notice-record](./notice-record.md#materializing-closure)).

## Compatibility

- A new owner-store table arrives through the store's migration path. Hosts deploy in lockstep
  (polaris RB-15); a follower on an older binary simply has no notice tools, and a routed notice call
  to an older owner is `tool_not_on_this_host`.
- No existing tool, list or schema changes shape. The default MCP toolset is unchanged, so long-lived
  MCP clients are unaffected by the upgrade.
- The brief block is a new optional key in the task context; an activity that does not opt in
  (`fleet_notices: read` or `post`, or its own `notice_instruction`) never receives it.

## Agent Signature

claude (claude-opus-5-5), 2026-11-02, [ORB-21207].
