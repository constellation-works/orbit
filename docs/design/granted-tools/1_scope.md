---
title: "Scope: operator-granted tools for the implementing activity (`granted_tools`)"
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
feature: granted-tools
doc_role: scope
type: design
summary: "Scope: an operator-only `granted_tools` task field that lifts the implementing activity's deny list for named tools, without granting capability, placement or live side effects"
tags: [granted-tools]
---

# Scope: operator-granted tools for the implementing activity (`granted_tools`)

Status: Draft — proposed, not yet approved for implementation.
Bearing: tasks already carry `required_tools`
([`artifacts.rs`](../../../crates/orbit-types/src/task/artifacts.rs));
`agent_implement` is a deny-list activity
([`agent_implement.yaml`](../../../crates/orbit-core/assets/activities/agent_implement.yaml))
whose requirements never override a deny [ORB-15162]
([`activity_tools.rs`](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/activity_tools.rs));
field-level operator authorization already exists in
[`tool_exec.rs`](../../../crates/orbit-runtime/src/tool_exec.rs); and claimed
(follower) implementers already refuse coordination requirements in
[`serve.rs`](../../../crates/orbit-core/src/application/distributed/serve.rs).

## Problem

An operator wants a task whose implementing agent can call a tool the
activity normally denies — the motivating example is `orbit.auto_task.update`,
so a task can revise an auto-task definition as part of its work.

Today that is impossible by design. `agent_implement`'s `tool_disallow_list`
names `orbit.auto_task.add`, `orbit.auto_task.mint`, `orbit.auto_task.update`,
`orbit.command.exec`, `orbit.pipeline.invoke`, `orbit.workflow.run.resume`
and `orbit.workflow.ship`. Listing one in `required_tools` does not help:
`admit_required_tools` in
[`activity_tools.rs`](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/activity_tools.rs)
fails implementer admission when the deny policy covers a requirement, and
only non-implementer activities (review, recovery, pilot — see
[`activity_tool_requirements.rs`](../../../crates/orbit-core/src/application/job/activity_tool_requirements.rs))
drop a covered requirement with a note instead. [ORB-15162] made "a
requirement never overrides a disallow list" the rule, because the obvious
alternative is a privilege-escalation hole: any agent that can file a task
could file one granting itself denied tools, and automatic filing and
approval paths would carry the grant through without an operator ever
seeing it.

So the mechanism needed is not a stronger `required_tools`. It is a grant
whose *authority* is different from the requirement's, and whose effect is
narrower than "the agent may now do this."

## Goal

A task may carry operator-granted tools that the implementing activity —
and only that activity — may use despite its deny list, while every other
gate (capability, filesystem/process policy, external authentication,
placement, and the review gate for side effects) keeps applying exactly as
it does today. An agent can request tools; only an operator can grant the
denied ones.

## Shape

### Current behaviour this builds on

| Fact | Source |
|---|---|
| `agent_implement` denies the seven tools listed above, `on_denial: terminate` | [`agent_implement.yaml`](../../../crates/orbit-core/assets/activities/agent_implement.yaml) (`tool_disallow_list`) |
| A requirement that the deny list covers fails implementer admission; non-implementers drop it with a note [ORB-15162] | [`activity_tools.rs`](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/activity_tools.rs) (`admit_required_tools`, `resolve_activity_tool_denials`), [`activity_tool_requirements.rs`](../../../crates/orbit-core/src/application/job/activity_tool_requirements.rs) (`drops_disallowed_requirements`) |
| Requirements must be exact canonical names of registered, agent-facing, enabled tools; wildcards are refused | [`activity_tools.rs`](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/activity_tools.rs) |
| Field-level operator authorization: `orbit.auto_task.update` carrying `expected_enabled`, and `orbit.auto_task.mint` carrying `acknowledge_unconditional`, additionally require the governed operation `orbit.routine.control` | [`tool_exec.rs`](../../../crates/orbit-runtime/src/tool_exec.rs) (`authorize_registered_tool`), [`catalog.rs`](../../../crates/orbit-common/src/governance/authorization/catalog.rs) (`orbit.routine.control`, Operator-only) |
| Every governed tool call passes one capability chokepoint; a session's capabilities, not its asserted mode, decide | [`tool_exec.rs`](../../../crates/orbit-runtime/src/tool_exec.rs), [`authorization.rs`](../../../crates/orbit-runtime/src/authorization.rs) (`authorize_tool_operation`) |
| A claimed implementer is held when a requirement is in `CLAIMED_MODE_DENIED_TOOLS` or is a coordination tool outside the closed claimed-owner bridge | [`serve.rs`](../../../crates/orbit-core/src/application/distributed/serve.rs) (`claimed_required_tool_hold`), [`invocation.rs`](../../../crates/orbit-types/src/tool/invocation.rs) (`CLAIMED_MODE_DENIED_TOOLS`, `CLAIMED_OWNER_TOOLS`) |
| An activity's system of record is its durable artifact; agent-loop output is advisory and the returned object is not persisted as the decision record | [`4_decisions.md`](../activity-job/4_decisions.md) (activity-job: "provider output is not the system of record") |

### 1. Who can grant — operator only, frozen at dispatch

**Decision: a separate `granted_tools` task field, settable only under
operator authority, following the field-level authorization pattern of
`expected_enabled` → `orbit.routine.control`.**

- Setting, adding to, or removing from `granted_tools` on
  `orbit.task.add` / `orbit.task.update` is a governed operation in its own
  right. Follow the pattern in
  [`tool_exec.rs`](../../../crates/orbit-runtime/src/tool_exec.rs): when the
  input carries a `granted_tools` change, `authorize_registered_tool`
  additionally requires a governed operation (recommended id
  `orbit.task.grant_tools`, `allowed: &[Operator]` in
  [`catalog.rs`](../../../crates/orbit-common/src/governance/authorization/catalog.rs);
  reusing `orbit.routine.control` is the acceptable minimal variant, but a
  dedicated id keeps the audit predicate unambiguous). As today, MCP
  sessions resolve capabilities from the session only
  (`CapabilityEnforcement::McpSessionOnly`): hosting a server never lends
  operator authority.
- An agent-capability caller that supplies `granted_tools` is *refused*,
  not silently stripped. Silent stripping would let an auto-filed task
  look granted in its text while running ungranted; a loud refusal names
  the missing authority at filing time. Automatic approval, task-pilot
  and auto-task filing paths therefore cannot carry a grant through: they
  hold agent authority, so the write fails closed unless an operator
  performs or re-performs it.
- **Change window.** An operator may change the grant freely while the
  task is pre-dispatch (no run, claim or worktree exists for it). Once an
  attempt is dispatched, the grant is frozen for that attempt: widening it
  mid-run would be an unreviewed steering change to a running agent's
  authority. Changing it after dispatch means cancelling/requeuing so a
  new attempt starts under the new grant; revoking it likewise takes
  effect at the next attempt, never by reaching into a live run.
- **Audit.** Every grant change writes (a) a task-history entry recording
  actor provenance, the before/after tool sets and the run/claim state at
  the time, and (b) the standard authorization audit event from
  [`authorization.rs`](../../../crates/orbit-runtime/src/authorization.rs),
  so "who granted what" is queryable with the same predicate as every
  other governed decision. A forwarded remote-caller label is attribution
  only, never a grant (as in the existing chokepoint).

*Alternative rejected:* gating grants on ordinary task-update authority
plus a warning. That is the escalation hole above with a label on it —
the filer's authority, not the tool's risk, is the problem.

### 2. What a grant means — list membership only, never capability

**Decision: a grant removes one name from the implementing activity's
effective deny intersection. Nothing else moves.**

After the grant, the tool call still passes, in order: the exact-canonical
/ registered / agent-facing / enabled admission checks; the activity
tool-policy check; the governed capability chokepoint
(`authorize_tool_operation`); the filesystem profile and
`proc_disallowed_programs` policy; worker-coordination and claimed-mode
checks; and any external authentication the tool itself performs. A grant
is therefore *not* a capability: it cannot mint `Operator` or `Runner`,
and it does not satisfy inner field-level gates — a granted
`orbit.auto_task.update` still refuses `expected_enabled` from an agent
session, because that input separately requires `orbit.routine.control`.

**Always-operator-only tools are not grantable.** If a tool's governed row
admits only `Operator` (or `Operator` + `Runner`) — e.g.
`orbit.command.exec`, `orbit.workflow.ship`, `orbit.workflow.run.resume`,
`orbit.auto_task.delete` — grant validation refuses it at grant time,
naming the governed row. Granting it would either be dead weight (the
capability gate still refuses every agent call, confusingly) or, if a
future change made grants satisfy capability, a standing privilege
escalation. *Alternative rejected:* grant plus a scoped temporary
capability for the run. That mints operator authority inside an agent
sandbox, which is exactly what the capability design (agent sessions
never hold operator grants) exists to prevent. An operator who needs
such an operation performed uses §4's artifact path or does it as a
separate operator task.

Grantable therefore means: denied to `agent_implement`, but passable by an
`Agent` capability once listed — ungoverned, or governed with `Agent` in
its allowed set. `orbit.auto_task.update` (ungoverned except for the
`expected_enabled` input) is the canonical grantable case.

The grant reaches the implementer only. Reviewers, recovery, pilot and
every other activity keep their own deny lists untouched: the existing
non-implementer drop-with-a-note behaviour for `required_tools` is not
extended to grants, because those activities never receive them at all.

### 3. Placement — a grant pins the task to the owner host

**Decision: any non-empty `granted_tools` makes the task owner-host-only,
enforced as an admission hold beside `claimed_required_tool_hold`.**

Coordination tools are denied to claimed implementers because a follower
reaches its owner only through the closed `CLAIMED_OWNER_TOOLS` bridge;
`orbit.auto_task.*` is not on that bridge. Extending the bridge per-grant
would widen a closed list by task data, so instead the grant is a
placement constraint: dispatch, drain and pull must not offer a granted
task to a follower, with the hold reason naming the granted coordination
tools (mirroring how `claimed_required_tool_hold` names its restriction).

Composition with label-based host placement [ORB-18316] (tag
`host-placement`): the grant is one more hard constraint fed into the
same placement resolution, not a parallel mechanism. Effective placement
is the intersection — owner host *and* the task's required machine
labels. If the owner does not satisfy the labels, the task is held with
both reasons named; it never falls back to a follower to satisfy labels,
and labels never relocate ownership. The operator resolves the conflict
by dropping the grant, dropping a label, or moving the task's ownership.
In v1 the pin applies to *any* grant, not only coordination tools: the
grantable set today is coordination-dominated, and one rule is easier to
audit than a per-tool placement table. Relaxing non-coordination grants
to claimed execution is a later, separate decision.

### 4. Side effects outside the diff — mutations travel as artifacts (central question)

A granted mutation such as `orbit.auto_task.update` changes live state
the moment it runs. The PR review gate never sees it, landing cannot
include it, rollback cannot undo it, and a retried run repeats it.

| Option | What happens | Verdict |
|---|---|---|
| (a) Grant only reads and repeat-safe operations | Direct calls, no live mutation ever | Safe but fails the motivating case: `orbit.auto_task.update` is a mutation |
| (b) Agent emits the change as a task artifact; the pipeline applies it after review approval | The proposed mutation is reviewed with the diff, applied once by the pipeline, receipt recorded | **Recommended** |
| (c) Live call plus a mutation receipt on the task, checked by the reviewer, with replay protection | Effect precedes review; the reviewer can see but not prevent it; PR rollback cannot undo it | Rejected for mutations |

**Recommendation: (b), with (a) as its direct-call subset.** Each
grantable tool is classified at grant time as *read/repeat-safe* or
*mutating*; unclassified tools default to mutating (fail closed).

- Read and repeat-safe granted tools (same arguments, no further effect
  on repeat) execute directly in the implementing run.
- A mutating granted tool does not execute live. The agent instead emits
  a **mutation proposal artifact** on the task: canonical tool name, the
  exact input, a rationale, and an idempotency key derived from
  task + proposal content hash. The artifact — not the agent's returned
  output — is the system of record for the intended change, consistent
  with the durable-artifact doctrine above.
- After review approval (at the same gate that approves the diff), the
  pipeline applies the proposal exactly once under host authority,
  deduplicated by the idempotency key, and records an **application
  receipt artifact** (applied input, result, timestamp). A retried
  implementing run that re-emits the same proposal deduplicates against
  the same key; a changed proposal is a new artifact requiring the same
  review. If application fails, the failure is a visible task state, not
  a silent skip, and landing does not claim the change happened.
- The reviewer reviews the proposal artifact beside the diff, which is
  what makes the side effect reviewable, landable and auditable even
  though it can never be part of the diff itself. Option (c)'s receipt
  idea survives only in this post-approval form; its live-first ordering
  is the part rejected.
- Consequence, stated plainly: a mutation the agent needs *during* the
  run in order to proceed cannot be served by this mechanism — the
  proposal applies only after review. That need is an operator action or
  a separate prerequisite task, not a grant.

### 5. Relation to `required_tools` — keep both

**Decision: keep `required_tools` and `granted_tools` as separate fields.**

| | `required_tools` | `granted_tools` |
|---|---|---|
| Meaning | "The implementer needs this tool" | "An operator authorizes lifting the deny for this tool" |
| Who may set it | Any task filer | Operator capability only (§1) |
| Effect on a deny | Never overrides [ORB-15162] | Overrides, for the implementer only (§2) |
| Lifecycle | Editable as task text | Frozen at dispatch; audited (§1) |
| Placement effect | Claimed hold per tool | Owner-host pin (§3) |

A granted tool must also be validated exactly like a requirement (exact
canonical name, registered, agent-facing, enabled) and is treated as
required for admission: granting a tool the implementer does not get is
a filing error, not a silent no-op. In practice the grant list will
usually also appear in `required_tools`; admission computes the
effective set as baseline ∪ required ∪ granted, with granted names first
checked against the deny list they lift.

*Alternative rejected:* one field with an operator-only override flag.
The flag would ride inside agent-editable task data, so every filing and
auto-approval path would need to police one boolean forever; a task
copied or minted from a granted template would carry the flag's
authority with it. Separate fields make the authority boundary a field
boundary, which the existing field-level authorization pattern can gate
in one place.

### Configuration

No new configuration keys. Grant admission derives from data that
already exists: the activity's `tool_disallow_list`, the tool registry,
and the governed-operation catalog. The one new governed id
(`orbit.task.grant_tools`, §1) is a catalog row, not a setting.

### Surfaces

- Task show / list: `granted_tools` shown beside `required_tools`, with
  grant provenance (operator identity, time) on the task history view.
- Admission and drain/pull holds: refusals name the granted tool and the
  specific gate that refused it (deny, capability, placement), reusing
  the existing hold-reason style.
- Review: mutation proposal and application receipt artifacts appear in
  the task's artifact list; the review gate names unapplied proposals.
- Audit: grant changes and denied granted-tool calls are queryable
  through the existing authorization audit predicate.

## Non-goals

- **No capability grants.** A grant never confers `Operator` or `Runner`,
  satisfies an inner field-level gate, or makes an always-operator-only
  tool callable (§2).
- **No grants for non-implementers.** Review, recovery, pilot and
  exploration activities are unchanged; this doc does not revisit the
  non-implementer drop-with-a-note rule [ORB-15162].
- **No claimed-mode bridge widening.** `CLAIMED_OWNER_TOOLS` stays a
  closed list; grants pin placement instead (§3).
- **No live pre-review mutations.** The artifact path is the only route
  for granted mutations (§4); mid-run operator operations remain
  operator work.
- **No filing of the implementation tasks below.** This doc scopes them;
  filing is an operator action after approval.

## Concerns and honest limitations

- **Classification is the load-bearing judgement.** Calling a tool
  "repeat-safe" when it is not re-opens option (c)'s replay problem
  inside option (b). The default-to-mutating rule bounds the damage, but
  each classification needs review like a governed-catalog row.
- **Owner-host pinning reduces drain elasticity.** Granted tasks cannot
  soak follower capacity; if granted tasks become common, owner-host
  queueing is the price, and the v1 any-grant pin (not just coordination
  grants) deliberately over-pays it for auditability.
- **The pipeline applier is new trusted surface.** Applying proposals
  after approval needs host authority scoped to exactly the reviewed
  artifact content; a bug there applies unreviewed input. Hash-binding
  the applied input to the reviewed artifact is mandatory, not polish.
- **Grants are visible in task data.** Tool names are not secrets, but a
  grant advertises which denied surface a task touches; task visibility
  rules are unchanged and this doc does not widen them.

## Phasing

Listed as implementation tasks to be filed by an operator after this doc
is accepted. Not filed by this doc.

1. **Grant field and operator gate.** Add `granted_tools` to the task
   record, the `orbit.task.grant_tools` governed row (or the
   `orbit.routine.control` reuse), the field-level check in
   `authorize_registered_tool`, refusal for agent filers, the
   pre-dispatch change window with freeze at dispatch, and the
   history + audit records. No activity behaviour changes yet: grants
   are stored and shown, and admission still refuses a granted tool that
   the deny list covers. Lands dark.
2. **Grantable classification.** A reviewed classification of the
   `agent_implement` deny set (and a default-mutating rule for anything
   unclassified): grantable vs always-operator-only, read/repeat-safe vs
   mutating, enforced at grant time against the governed catalog.
3. **Implementer admission.** `activity_tools.rs` admits a granted name
   past the deny list for `agent_implement` only, with every downstream
   gate (capability, policy, external auth) untouched; non-implementer
   activities ignore grants. Read/repeat-safe granted tools become
   callable.
4. **Owner-host placement hold.** The `claimed_required_tool_hold`
   analogue for grants in dispatch, drain and pull, composed with
   [ORB-18316] label placement as an intersection of hard constraints.
5. **Mutation proposal artifacts and pipeline application.** The proposal
   and receipt artifact shapes, reviewer surfacing, post-approval
   exactly-once application keyed by content hash, and visible
   application-failure state.
6. **Surfaces and docs.** Task show/history provenance, hold reasons,
   audit queries, operator runbook notes, and this doc's move to
   Accepted with the shipped behaviour recorded.

Each phase is independently landable; phases 1–2 alone close the
escalation question (an operator *can* record a grant, nothing executes
differently), and phase 5 is the only one that can change live state.

## Open questions

1. **Governed id.** New `orbit.task.grant_tools` row versus reusing
   `orbit.routine.control`? This doc recommends the new id for audit
   clarity; the reuse is smaller. Either follows the same
   `tool_exec.rs` pattern.
2. **Classification ownership.** Does the read/repeat-safe vs mutating
   classification live beside the governed catalog, or in the tool
   registry entry? It must be one reviewed list; its home decides who
   reviews additions.
3. **Grant expiry.** Should a grant lapse if the task sits undispatched
   (e.g. is re-piloted or its text changes materially)? v1 has no expiry
   beyond the dispatch freeze; an operator re-check at dispatch is the
   alternative if stale grants become a concern.

## Acceptance (feature level)

- An agent-capability `orbit.task.add` / `orbit.task.update` carrying
  `granted_tools` is refused naming the missing operator authority; the
  same write from an operator session succeeds and is recorded in task
  history and the authorization audit.
- A task granting `orbit.command.exec` (always operator-only) is refused
  at grant time; a task granting `orbit.auto_task.update` is admitted,
  and its implementer can call `orbit.auto_task.update` without
  `expected_enabled`, while a call carrying `expected_enabled` is still
  refused for want of `orbit.routine.control`.
- Review, recovery and pilot runs of a granted task receive none of the
  granted tools and behave exactly as today.
- A granted task is never claimed by a follower; the hold names the
  grant, and with [ORB-18316] labels present the placement is the
  owner-host ∩ label intersection or a hold naming both constraints.
- A mutating granted call produces a proposal artifact and no live
  change; after review approval the change is applied once, a receipt
  artifact exists, and a retried run re-emitting the same proposal does
  not apply it twice.
- Changing `granted_tools` after dispatch is refused for the live
  attempt; the change takes effect only on a new attempt.

## Task References

- [ORB-18317] — this scope doc: operator-granted tools for the implementing activity
- [ORB-15162] — established that `required_tools` never overrides an activity disallow list
- [ORB-18316] — label-based host placement, whose placement resolution grants compose with

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
