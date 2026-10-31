---
title: "Scope: operator-granted tools for the implementing activity (`granted_tools`)"
owner: claude
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Accepted
stage: shipped
next: "Operate the shipped grants and reviewed mutation proposal path"
feature: granted-tools
doc_role: scope
type: design
summary: "Scope: an operator-only `granted_tools` task field that lifts the implementing activity's deny list for named tools, without granting capability, placement or live side effects"
tags: [granted-tools]
---

# Scope: operator-granted tools for the implementing activity (`granted_tools`)

Status: Accepted — all six implementation phases are shipped. Grants are
operator-only, owner-host-only and frozen for a dispatched attempt. Every
currently denied implementer tool is mutating: grants authorize reviewed
proposals, never live mutations. See the shipped
[operator runbook](../../../crates/orbit-core/assets/skills/orbit/references/task-fields.md#operator-runbook-granting-tools).
Bearing: tasks already carry `required_tools`
([`artifacts.rs`](../../../crates/orbit-types/src/task/artifacts.rs));
`agent_implement` is a deny-list activity
([`agent_implement.yaml`](../../../crates/orbit-core/assets/activities/agent_implement.yaml))
whose requirements never override a deny [ORB-15162]
([`activity_tools.rs`](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/activity_tools.rs));
field-level operator authorization already exists in
[`tool_exec.rs`](../../../crates/orbit-runtime/src/tool_exec.rs); and claimed
(follower) implementers already refuse coordination requirements in
[`serve/admission.rs`](../../../crates/orbit-core/src/application/distributed/serve/admission.rs).

## Problem

An operator wants a task whose implementing agent can call a tool the
activity normally denies — the motivating example is `orbit.auto_task.update`,
so a task can revise an auto-task definition as part of its work.

Before this feature that was impossible by design.
`agent_implement`'s `tool_disallow_list`
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
| A claimed implementer is held when a requirement is in `CLAIMED_MODE_DENIED_TOOLS` or is a coordination tool outside the closed claimed-owner bridge | [`serve/admission.rs`](../../../crates/orbit-core/src/application/distributed/serve/admission.rs) (`claimed_required_tool_hold`), [`invocation.rs`](../../../crates/orbit-types/src/tool/invocation.rs) (`CLAIMED_MODE_DENIED_TOOLS`, `CLAIMED_OWNER_TOOLS`) |
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
  additionally requires the dedicated governed operation
  `orbit.task.grant_tools`, `allowed: &[Operator]` in
  [`catalog.rs`](../../../crates/orbit-common/src/governance/authorization/catalog.rs). MCP
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
  new attempt starts under the new grant after any preserved worktree is
  removed. Changes are accepted only in proposed, backlog or someday, with no
  live run, footprint-protecting claim or preserved worktree [ORB-18641]. Revoking it likewise takes
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

After the grant, the tool call still passes all applicable gates: the
exact-canonical / registered / agent-facing / enabled admission checks; the activity
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
such an always-operator-only operation performed does it as separate
operator work; §4's artifact path cannot grant operator capability either.

Grantable therefore means: denied to `agent_implement`, but passable by an
`Agent` capability once listed — ungoverned, or governed with `Agent` in
its allowed set. `orbit.auto_task.update` (ungoverned except for the
`expected_enabled` input) is the canonical grantable case.

**Classification ownership (phase 2 decision).** The single reviewed list is
`AGENT_IMPLEMENT_TOOL_GRANT_CLASSIFICATIONS` in
[`authorization/grants.rs`](../../../crates/orbit-common/src/governance/authorization/grants.rs),
beside the governed-operation catalog. Authorization reviewers own additions
and any change to repeat-safety, as they do catalog rows. It classifies all
seven shipped deny entries: the three auto-task operations are grantable;
the command, pipeline and workflow operations are always operator-only.
Every current entry is mutating: definition edits, task minting, arbitrary
commands and run submission/resumption can all change live state. There are
no reviewed read/repeat-safe entries yet. `classify_agent_tool_grant` treats
an unclassified name as mutating and always rechecks the governed row, so
neither an omitted entry nor a stale grantable classification can bypass a
row that excludes `Agent`.

Task creation and grant replacement both call `validate_granted_tools` in
[`task/grants.rs`](../../../crates/orbit-core/src/application/task/grants.rs).
It refuses an always-operator-only tool, naming its governed row, and reuses
the activity's exact-canonical, registered, agent-facing and enabled admission
rule. Disabled grants fail at filing time, including a resubmitted unchanged
grant; ordinary `required_tools` keep their existing disabled-tool warning.
Phases 1 and 2 landed dark: classification alone does not lift an activity deny
or execute a mutation. Reviewed proposal application is phase 5 (§4).

**Implementer admission (phase 3 decision).** `resolve_activity_tool_denials`
in
[`activity_tools.rs`](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/activity_tools.rs)
reads grants for `agent_implement` only. A covered read/repeat-safe grant
lifts its exact disallow entry before required-tool admission and the
callable set are computed, so the effective set is baseline ∪ required ∪
granted. A covered mutating grant stays denied and is never called live; a
task that also requires it still fails admission, with the refusal naming
the mutating classification. A grant that is unavailable, now always
operator-only, or still covered by a wildcard entry refuses admission. The
lifted names travel in `ResolvedActivityTools::granted_tools`, and the
dispatcher
([`policy.rs`](../../../crates/orbit-engine/src/activity_job/cli_runner/orchestrator/policy.rs))
removes them from the deny list the child enforces, except a claimed-mode
denial, which stays. `activity_tool_requirements.rs` stays grant-blind, so
every other activity, including reviewers, recovery and pilot, receives no
grant. Because every current classification is mutating, no shipped grant
lifts a deny yet.

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
| (b) Agent emits the change as a task artifact; the pipeline applies it after review approval | The proposed mutation is reviewed with the diff, applied once by the pipeline, receipt recorded | **Shipped** |
| (c) Live call plus a mutation receipt on the task, checked by the reviewer, with replay protection | Effect precedes review; the reviewer can see but not prevent it; PR rollback cannot undo it | Rejected for mutations |

**Decision: (b), with (a) as its direct-call subset.** Each
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

**Proposals and application (phase 5 decision).** The shapes live in
[`task/mutation_proposal.rs`](../../../crates/orbit-types/src/task/mutation_proposal.rs);
admission and application in
[`task/mutation_proposals.rs`](../../../crates/orbit-core/src/application/task/mutation_proposals.rs).

- *Proposal.* The writer attaches `mutation-proposals/<name>.json` holding
  `{tool, input, rationale}`. `admit_task_artifact` in
  [`update/artifacts.rs`](../../../crates/orbit-core/src/application/task/update/artifacts.rs)
  admits it only on an owner task update or start, when the task grants
  the tool, the tool is a grantable mutation, and an agent-only session
  passes `authorize_registered_tool` for that exact input. So a granted
  `orbit.auto_task.update` carrying `expected_enabled` is refused for want
  of `orbit.routine.control`, whoever writes it. The stored artifact adds
  the task id and an idempotency key: SHA-256 over the task id, tool and
  canonical input. The rationale is left out, so rewording it never applies
  the same change twice. Claimed-worker evidence cannot carry a proposal.
- *Receipt.* `mutation-receipts/<key>.json` is reserved for the system
  writer, like the `review-` names, and counts only when the artifact
  manifest records a system write. It holds the applied input, the
  proposal path and digest, the authorizing review attempt, the run, the
  state (`pending`, `applied` or `failed`) with result or error, and a
  timestamp.
- *Review.* `review_gate_admit` lists every proposal without an applied
  receipt in the manifest's `mutation_proposals`, each with its path,
  stored-bytes digest and content. The reviewer judges proposals beside the
  diff and cannot repair one. A run that reviews nothing (timing off,
  no-diff exemption, `skip-review`, no captured admission) refuses its
  tasks' proposals by name. The certificate is unchanged; the system-written
  manifest of the same attempt carries the binding.
- *Application.* When `review_gate_settle` passes an attempt, including a
  replayed settlement, it applies the manifest's proposals before anything
  is pushed or landed. A task proposal applies only when that manifest holds
  it with the same path, digest and content, and the reviewed input is what
  runs. The host makes the call, but its capability check is the agent's, so
  approval never mints operator authority. The rules are rechecked first; a
  `pending` receipt is written, then `applied` or `failed`. An applied key is
  skipped. A pending one means the outcome is unknown, and it stays
  unapplied until an operator investigates the actual outcome. The runbook
  requires retained evidence and separate operator work; no automatic
  pending-receipt reconciliation command is shipped. An unreviewed, changed
  or failed proposal fails the step after the others run, with a task comment naming each.
- *Completion.* A task is never `done`, except by a human force, while any
  proposal lacks an applied receipt. That also covers the local route,
  which has no review gate.
- *Limitation.* Proposals ride with a reviewed diff. A task whose only
  product is a mutation has no candidate for the gate to admit, so such a
  delivery is refused rather than applied unreviewed.

### 5. Relation to `required_tools` — keep both

**Decision: keep `required_tools` and `granted_tools` as separate fields.**

| | `required_tools` | `granted_tools` |
|---|---|---|
| Meaning | "The implementer needs this tool" | "An operator authorizes lifting the deny for this tool" |
| Who may set it | Any task filer | Operator capability only (§1) |
| Effect on a deny | Never overrides [ORB-15162] | Lifts exact entries for read/repeat-safe tools in the implementer only (§2) |
| Lifecycle | Fixed at creation | Frozen at dispatch; audited (§1) |
| Placement effect | Claimed hold per tool | Owner-host pin (§3) |

A granted tool must also be validated exactly like a requirement (exact
canonical name, registered, agent-facing, enabled) and is checked at
admission. For reviewed read/repeat-safe grants, the callable set is
baseline ∪ required ∪ lifted grants. Mutating grants remain denied live and
travel only as proposals; also requiring a denied mutation refuses
implementer admission. Do not duplicate a mutating grant in `required_tools`.

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
- Audit: grant authorizations (including ordinary operator-capability
  sessions) and activity-policy/capability denials are queryable through
  `command = 'authorization'`. Task history records committed grant changes;
  an authorization row alone does not prove a write committed. Existing
  denial aggregates deduplicate entry-point rows for the same invocation.

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

All six phases are implemented (all done). The phase descriptions below record their
responsibilities; the decisions above describe the shipped constraints.

1. **Grant field and operator gate [ORB-18545] (done).** Add `granted_tools` to the task
   record, the dedicated `orbit.task.grant_tools` governed row, the
   field-level check in
   `authorize_registered_tool`, refusal for agent filers, the
   pre-dispatch change window with freeze at dispatch, and the
   history + audit records. No activity behaviour changes yet: grants
   are stored and shown, and admission still refuses a granted tool that
   the deny list covers. Lands dark.
2. **Grantable classification [ORB-18546] (done).** A reviewed classification of the
   `agent_implement` deny set (and a default-mutating rule for anything
   unclassified): grantable vs always-operator-only, read/repeat-safe vs
   mutating, enforced at grant time against the governed catalog.
3. **Implementer admission [ORB-18547] (done).** `activity_tools.rs` admits a granted name
   past the deny list for `agent_implement` only, with every downstream
   gate (capability, policy, external auth) untouched; non-implementer
   activities ignore grants. Read/repeat-safe granted tools become
   callable.
4. **Owner-host placement hold [ORB-18548] (done).** The `claimed_required_tool_hold`
   analogue for grants in dispatch, drain and pull, composed with
   [ORB-18316] label placement as an intersection of hard constraints.
5. **Mutation proposal artifacts and pipeline application [ORB-18549] (done).** The proposal
   and receipt artifact shapes, reviewer surfacing, post-approval
   exactly-once application keyed by content hash, and visible
   application-failure state.
6. **Surfaces and docs [ORB-18550] (done).** Task show/history provenance, hold reasons,
   audit queries, operator runbook notes, and this doc's move to
   Accepted with the shipped behaviour recorded.

Each phase is independently landable; phases 1–2 alone close the
escalation question (an operator *can* record a grant, nothing executes
differently), and phase 5 is the only one that can change live state.

## Open questions

1. **Governed id — resolved in phase 1.** The dedicated
   `orbit.task.grant_tools` row is shipped, with operator-only authority.
2. **Classification ownership — resolved in phase 2.** The single list lives
   beside the governed catalog in `authorization/grants.rs`, under
   authorization review; see §2 for the grantability and mutating defaults.
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
  and its implementer can propose `orbit.auto_task.update` without
  `expected_enabled`. A proposal carrying `expected_enabled` is still
  refused for want of `orbit.routine.control`; a direct live mutation remains
  denied by the implementing activity.
- Review, recovery and pilot runs of a granted task receive none of the
  granted tools and behave exactly as today.
- A granted task is never claimed by a follower; the hold names the
  grant, and with [ORB-18316] labels present the placement is the
  owner-host ∩ label intersection or a hold naming both constraints.
- A mutating grant is exercised by attaching a proposal artifact, with no
  live change; after review approval the change is applied once, a receipt
  artifact exists, and a retried run re-emitting the same proposal does
  not apply it twice.
- Changing `granted_tools` after dispatch is refused for the live
  attempt; the change takes effect only on a new attempt.

## Task References

- [ORB-18317] — this scope doc: operator-granted tools for the implementing activity
- [ORB-15162] — established that `required_tools` never overrides an activity disallow list
- [ORB-18316] — label-based host placement, whose placement resolution grants compose with
- [ORB-18545] — phase 1: grant field, operator gate, dispatch freeze, and history and audit records.
- [ORB-18546] — phase 2: grantable classification enforced at grant time.
- [ORB-18547] — phase 3: implementer admission past the deny list.
- [ORB-18548] — phase 4: owner-host placement hold for granted tasks.
- [ORB-18549] — phase 5: mutation proposal artifacts and post-approval application.
- [ORB-18550] — phase 6: surfaces, runbook notes, and this doc to Accepted.
- [ORB-18641] — the preserved-worktree refusal in the grant change window.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
