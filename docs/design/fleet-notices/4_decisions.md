---
title: Fleet Notices — Decisions
owner: claude
last_updated: 2026-11-02
last_validated: 2026-11-02
status: Draft
feature: fleet-notices
doc_role: decisions
type: design
summary: "Decisions for fleet notices: Orbit state not a Library board, notices never grant authority, derived poster class, scoped agent posting not broadcast, mandatory expiry, derived effective state, corroborating dedupe, brief switch separate from the toolset, maintenance hold deferred"
tags: [fleet-notices]
paths: ["crates/orbit-core/src/application/notice/**", "crates/orbit-common/src/governance/notice.rs", "crates/orbit-types/src/tool/mcp_toolset.rs"]
related_features: [fleet-notices, operator-attention, federated-mcp, mid-run-steering]
related_artifacts: [ORB-21207]
---

# Fleet Notices — Decisions

Decisions for [fleet notices](./1_overview.md), recorded by title, each naming the alternative it
rejected. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and the
required `Cost:` line. Most entries are Door 2 (they govern later choices on this feature); Door 1
entries name the proposed code they will explain, from [2_design.md §2](./2_design.md#2-placement-in-the-code),
which the slices confirm.

## Fleet coordination notes are generic Orbit state, not a board outside Orbit

**Recorded:** 2026-11-02 · [ORB-21207]

### Context

The need came from constellation: the operator offline, orchestrators on several hosts, one bug
every agent hits. Three homes were on the table: a page in the orbit-office Library (a "board"
the scribe maintains), a polaris runbook or doc that agents read, or an Orbit record.

### Decision

Coordination facts that agents must see while they work belong in Orbit, as generic records with an
Orbit API; any other system that wants to post them is a client of that API. The operator's
direction (2026-11-02) was "that belongs in orbit". Concretely: no field, kind or default names a
constellation concept, and orbit-office's `#board` routing ([ORB-21208]) calls `orbit.notice.post`
and `close` rather than owning a board.

Rejected: **a Library board.** The Library holds the operator's directions and decisions, which
agents must not write (constellation's AGENTS.md: "never cross-write"), it is not readable from every host's
managed runs, and it has no expiry, authority or brief delivery. **A polaris doc.** Requires a commit
to post, a pull to see, and has no lifecycle; a stale doc is exactly the failure notices exist to
avoid.

### Consequences

Every Orbit user gets the feature, so it must stay free of constellation assumptions (single owner,
specific hosts). The scribe integration is one more client to keep compatible.

Cost: Orbit grows a new record type, a store table, four tools and a dashboard view for what is, in
the motivating fleet, a handful of notices a week.

## A notice never grants authority

**Recorded:** 2026-11-02 · [ORB-21207]

### Context

The motivating maintenance case ("hold off until the binary update is done") invites making notices
do things: pause drains, skip gates, widen a task. Agents read notices; if a notice could change what
an agent may do, any poster would hold a fleet-wide lever.

### Decision

No authorization, admission, validation or review decision reads a notice field, now or in a later
slice, except that a notice may *withhold* new unattended work on named hosts (the
[dispatch-hold extension](./2_design.md#7-extension-point-maintenance-dispatch-hold)). Withholding is
the only effect ever allowed, because it fails safe and is bounded by expiry. A brief block and the
instruction that comes with it say so to the agent. Apply this to any future proposal that wants a
notice to enable, skip or widen anything: refuse it and find a governed operation instead.

Rejected: notices that carry directives (`skip-review until`, `grant tool X`), and a maintenance
notice that cancels running work.

### Consequences

A notice saying "ignore the failing golden" changes nothing about the gate; the agent still has to
get a governed waiver. The board stays informative.

Cost: operators who want an announcement to *do* something must also make the governed change
(retag, waive, pause), so some coordination takes two steps.

## Poster class is derived from the session, never declared

**Recorded:** 2026-11-02 · [ORB-21207]

**Code anchors:** the planned poster-class resolution in `crates/orbit-core/src/application/notice/`;
`crates/orbit-common/src/governance/authorization/caller.rs::CallerEnvelope`.

### Context

The direction lists `author` as "human, orchestrator crew, or managed agent, with the agent's task
id", and agent posting "by: agent". The simplest tool would accept a `by` and a `task_id` input.

### Decision

The owner derives the class from the caller resolution (`Operator` capability, a run binding, or an
unbound session with orchestrator attribution), and the post input has no `by`, `author` or
`task_id` field; unknown fields are refused rather than warned. The `orbit.notice.post` input will
look like it is missing the author; that is deliberate.

Rejected: caller-declared `by`/`task_id` validated against the session. Validation would have to be
right for every transport; omission cannot be wrong.

### Consequences

An agent's notice always names its real task and run. An orchestrator cannot post "as the operator".

Cost: a client that wants to post on behalf of someone else (a scribe relaying the operator's words)
posts as itself; the relayed author goes in the body, not in `author`.

## Managed agents post only through their own task, never broadcast

**Recorded:** 2026-11-02 · [ORB-21207]

### Context

The bug-every-agent-hits case is best reported by the first agent that hits it. But a managed agent
is the least trusted poster: it may be confused, or its inputs may be adversarial.

### Decision

A managed agent may post only in its own task's workspace, only `incident` or `info`, only with a
linked task or evidence, for at most 24 hours, within a per-task rate and open-count limit, and may
edit or close only notices from its own task. Widening an agent's notice to `global` is an
orchestrator's or operator's re-post of the same dedupe key. Keep this ordering for any new poster
type: the less accountable the poster, the narrower its scope and the shorter its notice lives.

Rejected: **unscoped agent broadcast** (any agent posts global notices), which turns one injected
prompt into a fleet-wide one; and **no agent posting**, which leaves the first agent that diagnoses
a shared bug no way to warn the next forty.

### Consequences

Agent reports surface first in their workspace and reach the fleet only after an accountable poster
promotes them.

Cost: an agent-found fleet-wide bug is invisible outside its workspace until an orchestrator or the
operator promotes it, which, with the operator offline, depends on an orchestrator noticing.

## Every notice expires

**Recorded:** 2026-11-02 · [ORB-21207]

### Context

A coordination note's failure mode is staleness: "update in progress" after the update, "bug X"
after the fix. Optional expiry would be used by careful posters and forgotten by everyone else.

### Decision

There is no non-expiring notice. Every post gets an `expires_at` from a per-kind default or an
explicit value within a per-class cap, and an over-cap request is refused rather than clamped. When
a notice and its truth diverge, prefer the notice disappearing early over it lingering: a missing
notice fails to help, a stale one misleads every reader.

Rejected: optional expiry; "until closed" notices for operators; silently clamping an over-cap
request.

### Consequences

Long-lived facts are re-posted (which the dedupe key turns into an extension of the same notice).

Cost: a true long-running incident needs someone to re-post it before expiry, or it drops out of
every brief.

## Effective state is derived on read; stored rows only record explicit acts

**Recorded:** 2026-11-02 · [ORB-21207]

**Code anchors:** the planned effective-state function in `crates/orbit-core/src/application/notice/`
and the closure materializer.

### Context

Expiry and linked-task closure could be applied by a sweep that rewrites rows, which is simple to
read but means a notice is "open" in the store until the sweep runs, and a missed sweep leaves it
open forever.

### Decision

`status` is computed from the row, the clock and the linked task on every read; a stored close is
written only for an explicit close or, by the materializer, for a linked task that is `done`. A
reader that sees `closed: null` on a row must still compute the state. Expired is never stored.

Rejected: sweep-only expiry and closure (stale between sweeps, stale forever if the sweep stops);
event-only closure on the task transition (misses cross-workspace links and older tasks).

### Consequences

Readers agree at the same instant without any job running. The materializer can lag or fail with no
visible effect.

Cost: every list resolves its notices' linked tasks, a task read per linked notice; open counts are
small, but a workspace with many linked notices pays that per read.

## Dedupe corroborates instead of rejecting or duplicating

**Recorded:** 2026-11-02 · [ORB-21207]

### Context

Many agents will report the same bug. Rejecting the second post loses the signal that it is
widespread; accepting it floods the board.

### Decision

One open notice per dedupe key per home workspace. A matching post by someone allowed to edit the
notice updates it; by anyone else it corroborates, bumping `reports` and returning the existing
notice, with no content change. Prefer recording "how many hit this" over a second copy of the
words.

Rejected: reject-on-duplicate (loses the count, forces agents to handle an error); last-writer-wins
(an agent could overwrite an orchestrator's notice); one notice per author.

### Consequences

Briefs can rank by `reports` and show how widespread an incident is. A retried post is harmless.

Cost: a corroborating agent cannot add what it learned to the notice; it has to comment on the linked
task instead.

## Brief delivery is a workspace switch, not the toolset

**Recorded:** 2026-11-02 · [ORB-21207]

**Code anchors:** the planned `notice.brief_delivery` setting in `crates/orbit-config` and its
check in the `notices_for_agent_input` host method.

### Context

The direction asked the design to say whether brief injection follows the `notice` MCP toolset or
has its own switch.

### Decision

Delivery into briefs has its own workspace switch, `notice.brief_delivery`, default on. A toolset is a
per-server choice about which tools one MCP client lists; delivery is the workspace's policy for the
runs it dispatches on every host. Tying them would let the toolset selection on whichever host built
the prompt decide what agents are told, and would turn notices off for every managed run on a host
whose operator only wanted a shorter tool list.

Rejected: delivery follows the toolset; delivery always on with no switch.

### Consequences

A workspace can stop briefs (for example during an injection investigation) without touching any
host's MCP configuration, and enabling the toolset on one host changes nothing for runs.

Cost: two settings to explain where operators might expect one.

## The maintenance dispatch hold is a named extension, not v1

**Recorded:** 2026-11-02 · [ORB-21207]

### Context

The binary-update case would be best served by a maintenance notice that holds new drains on the
named hosts. That is the first notice that changes behaviour, and it interacts with drain admission,
pull drains and the scheduler.

### Decision

v1 ships no hold. The design fixes the hold's contract (operator/orchestrator `maintenance` notices
with `hosts` and an explicit `hold: "dispatch"`, read beside the host-shutdown probe, lifting on
close or expiry) and v1 refuses the `hold` field, so turning it on later needs no migration and no v1
notice can be read as a hold.

Rejected: building the hold in v1; inferring a hold from any open `maintenance` notice.

### Consequences

During a v1 binary update, orchestrators read the maintenance notice and hold off by choice.

Cost: v1's maintenance case still depends on orchestrators following an instruction; a careless
drain can still start during an update.

## Task References

- [ORB-21207] — designed fleet notices and recorded these decisions
- [ORB-21208] — orbit-office scribe `#board` routing, the first client of the notice tools

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
