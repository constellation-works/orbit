---
title: "Scope: Auto-sequences"
owner: codex
last_updated: 2026-10-30
last_validated: 2026-10-30
status: Draft
stage: designing
next: "Review condition, completion and expiry semantics before filing implementation work"
feature: auto-sequence
doc_role: scope
type: design
summary: "Condition-driven composition of existing auto-tasks, with sequential defaults, CEL waits and expiry, and durable step evidence."
tags: [auto-sequence, condition-triggers, auto-tasks, scheduling]
---

# Scope: Auto-sequences

This proposes a new definition, not an available configuration. Writing this
design does not enable a sequence or authorize task execution or a release.

Bearing: the [condition-trigger design](../condition-triggers/1_scope.md) supplies
typed CEL predicates, edge identities and bounded observation. Existing jobs
supply durable runs and checkpoints; auto-tasks supply templates and normal
admission. Source inspected at `8217460c0`: keyed task creation is in
`crates/orbit-core/src/application/task/add.rs`, keyed job lookup in
`crates/orbit-core/src/application/automation/members/member_host/host.rs`, and
standalone slot minting in `crates/orbit-automation/src/auto_tasks/scheduler.rs`.
These are building blocks, not an existing cross-definition sequence API.

## Problem

Release preparation should arrange its own prerequisites. When enabled, after a
chosen date and once the board drains, Orbit should obtain a full QA sweep and
security review before preparing the release. It should reuse suitable recent
results, avoid duplicate open tasks and explain why it is waiting.

A rigid list cannot express every step's readiness. Daniel wants each member to
have its own condition, such as completion of its predecessor and an empty board.
Conditions can also express 30-minute spacing and a maximum wait. An agent polling
these facts is unnecessary; several independently firing definitions lose the
durable record of which steps this particular preparation still owes.

## Goal

An **auto-sequence** names existing auto-tasks and records one durable execution
of that composition. A sequence has its own trigger. Each step may have a CEL
condition; a step without one defaults to following its predecessor. CEL also
expresses expiry. Reuse the job/checkpoint and admission machinery rather than
introduce another scheduler, expression language or agent coordinator.

## Shape

### Definitions and execution

Definitions live in `.orbit/auto_sequences/<name>.yaml`. The carrier accepts the
condition trigger from the condition-trigger design, initially workspace scope
only. An admitted edge starts one owner-side coordination job, with a frozen
definition, referenced auto-task definitions, source revision and fire identity.
At most one active execution per sequence is allowed initially. A definition has
at most 32 named steps, so observation, literal cross-reads and retained bindings
remain bounded. This is not an unbounded task generator.

The coordinator observes and mints tasks; ordinary authorized drains and delivery
pipelines execute them. It holds no workspace context lock or worker slot while
waiting and does not sleep inside a provider process. The existing owner clock
revisits pending coordination state. Only deterministic observation/admission
activities are needed. Nested sequences, arbitrary jobs and scripts are excluded.

### Conditions replace implicit ordering

Declaration order defines `self.prev`; it does not impose a dependency when a
step supplies `when`.

| Step definition | Eligibility |
|---|---|
| First step, no member or inherited condition | Immediately eligible after sequence admission |
| Later step, no member or inherited condition | Equivalent to `self.prev.status == 'done'` |
| Any step with `when` | Its expression is the entire sequence-local eligibility condition |
| `when: "true"` | Independently eligible; no implicit predecessor dependency |

For example, `self.prev.status == 'done' && board.empty` explicitly asks for both
ordering and an idle board. A condition referencing only time or board state may
become true while a predecessor is unfinished. Ordinary admission and resource
limits can still serialize otherwise eligible work. There is no separate
`delay_minutes`, `needs` or `max_wait_minutes` vocabulary in this proposal.

An auto-task's own workspace condition/guard remains an additional guard. It is
not bypassed by composition. If no member `when` is provided but the referenced
definition has such a condition, that condition replaces the implicit predecessor
dependency too. Thus sequential fallback applies only when no condition is
specified at either location. Explain renders the resolved condition and its
origin so editing a referenced condition cannot hide a change in ordering.
Standalone cron/interval slots do not restrict a sequence request, and sequence
mints do not consume or advance those slot/edge cursors. The initial loader
refuses referenced task/host/file-scope conditions whose subjects cannot be
bound from a workspace sequence, naming the incompatible definition.

All eligible steps are considered in one pass; there is no hidden "current step"
that stops later conditional members from being evaluated. Read snapshots are
consistent within a pass; a predecessor settling in that pass can enable a
dependent step on the next tick. Forward references, self references and cycles
in `steps[...]` are load errors in the initial version.

### CEL reads

Use the admitted, type-checked CEL subset from condition triggers. `now` is the
owner's UTC timestamp for the pass; examples use `timestamp()` and `duration()`,
not a new `cur.datetime()` function. Cross-read names must be string literals.
Load checks retain the existing expression size, cost and unknown-value rules.

| Context | Additional typed facts |
|---|---|
| Sequence trigger | `self.last_succeeded_at` (optional); `auto_tasks['name'].last_succeeded_at` and `last_succeeded_task` (optional, accepted completion rather than mint time) |
| Active sequence | `sequence.started_at`, `sequence.revision`, `sequence.status` |
| Step `when` / `expire_when` | `self.status`, `self.wait_started_at`, optional `self.task_id`, optional `self.prev`; `self.prev.status` and optional `self.prev.end_time`; literal `steps['id']` with the same status/end-time facts |
| Sequence `expire_when` | `self.started_at`, `self.status` for this execution |
| Step `reuse_when` | One typed `result`: `status`, `completed_at`, `revision`, `contract`, `definition_digest`, `task_id` |

`self.prev` is absent for the first step; its `end_time` is absent before accepted
settlement. Use `has()` before reading optional facts unless the checker can
prove availability from a short-circuited status test. A referenced auto-task's
own `self` still refers to that auto-task's history, never the sequence step.
Descriptions, comments, arbitrary artifacts and agent output are not CEL input.

`board.empty` is a proposed convenience read meaning no tasks in `backlog`,
`in-progress` or `review`, matching the current release-prep unfinished-work gate.
It does not silently mean "no eligible backlog". Proposed, blocked, someday and
scheduled future work are not included; use explicit status counts for a stricter
policy. Blocking audit findings are also checked through the readiness contract.
The observer explains those exclusions. The always-running drain, clock and
sequence coordinator do not themselves make the task board non-empty.

At a step's mint/admission check, exclude only that step's bound task from its
board view. Other sequence members count normally. Thus a queued task cannot
block its own start, while `board.empty` on security can intentionally wait for
QA to finish. Outside work is never exempted merely because it is an auto-task.
Sequence-start board exclusion follows the condition trigger's own-output rules;
the start predicate does not have to stay true throughout an active execution.

### Waiting, spacing and expiry

These are CEL policies, not dedicated timing fields:

```yaml
# Wait for the previous step and for the board to drain.
when: "self.prev.status == 'done' && board.empty"

# Or wait at least 30 minutes after its accepted completion.
when: >-
  self.prev.status == 'done'
  && now >= self.prev.end_time + duration('30m')
  && board.empty

# Stop waiting after two hours, rather than remaining false indefinitely.
expire_when: "now >= self.wait_started_at + duration('2h')"
```

`now + duration('30m') > self.prev.end_time` is not a 30-minute delay: it is
normally already true when the predecessor finishes. The lower bound must be
on `now`, as above. A maximum window inside `when` alone is insufficient: after
it closes, false means "still waiting", not "expired". `expire_when` supplies
that separate terminal meaning, using the same CEL checker and facts.

Every sequence requires a clock-based `expire_when`; steps may add earlier
expiry. For the first version, sequence expiry must have the simple absolute
form `now >= timestamp('<RFC3339>')` or the relative form
`now >= self.started_at + duration('<positive finite duration>')`. These are
ordinary CEL expressions, but the admitted shapes let the loader prove a finite
outer bound (at most seven days) and the clock schedule the next wake. More
general conditions are supported for step expiry under that bound. A missing,
unavailable or invalid expiry fact never silently removes the outer deadline.

An unconditional first step and every conditional step start their wait clock
at sequence admission. A default sequential step starts it when its predecessor
settles `done`. `self.wait_started_at` is persisted once, not recomputed each tick.
Dependent steps can instead anchor expiry to a named step's `end_time` explicitly.
Wait clocks include time in admission queues. Pauses, crashes and owner downtime
consume wall time; they do not reset deadlines.

Reconcile existing terminal receipts first, then evaluate expiry, then readiness.
For a receipt first observed after downtime, only a trusted settlement time at or
before the recorded deadline can pay an already-owed completion; a later task
completion cannot escape expiry by being reconciled first. An arbitrary step
expiry without a recoverable time boundary is evaluated conservatively on the
first observation and cannot backdate acceptance.
Expiry wins a simultaneous new admission. `expire_when: true` makes an unfinished
step `timed_out`, stops new admissions for the whole sequence, and records
`sequence_wait_expired` or `step_wait_expired`. This also bounds an already-bound
task's wait for completion. It does **not** kill, cancel, requeue or change that
task. Its normal execution timeout remains its pipeline's responsibility. A late
receipt is retained but cannot silently turn the timed-out sequence into success.

### Binding, reuse and completion

For each eligible step, first use an accepted result satisfying `reuse_when`, if
configured; otherwise bind a compatible open instance or perform a keyed mint.
Eligibility and the referenced definition's guard apply even to reuse. A match
must have the same workspace, effective definition/template and inputs, required
result contract, and source revision. Incompatible open work is an explained
`existing_instance_conflict`, not permission to create a duplicate.

Sequence mints and the standalone scheduler share the same definition-instance
admission fence and dedupe policy. A concurrent normal slot and sequence request
therefore bind the same compatible instance when `skip_if_open` applies. Each
sequence binding has its own durable identity; it neither steals an existing
instance nor changes that instance's independent obligations. Disabled or missing
referenced definitions hold admission; explicit sequence enablement does not
enable its members or their paid execution. Successful-result reuse is opt-in;
without `reuse_when`, a historical result does not discharge a step.

Step status is `waiting`, `queued`, `running`, `done`, `held`, `failed`,
`timed_out` or `cancelled`. `done` means the bound task completed and the required
durable completion contract was accepted, or a qualifying result was reused.
The raw task/run status is shown separately. `end_time` is the time the step
settled in this execution, including a reused step; `result.completed_at` keeps
the original evidence time for age checks.

Generic steps use normal structured task completion. A readiness-sensitive step
declares `require_result: release-readiness/v1`. QA/security adapters must expose
a system-validated receipt naming the task/run, definition digest, source SHA,
completion time, evidence references, and readiness `passed`, `blocked` or
`unavailable`. A task marked done while filing unresolved release-blocking
findings is `held: prerequisite_findings`, not a done sequence step. Missing or
unreadable proof is unavailable. Agent final messages cannot supply the receipt.
The core only validates registered typed result contracts and their provenance.
The workspace or owning plugin defines release-blocker policy and the
QA/security adapters; release-specific severity rules do not enter Orbit core.

At each mint and again immediately before a bound child starts, re-evaluate the
step and inherited guards against current board state under the owner's normal
admission fence. Merely minting under an empty board does not authorize execution
after other work arrives. Record a sequence-bound admission hold for a newly
minted child; no task-status transition is used to simulate the guard. Reuse or
binding of unrelated open work never imposes new execution restrictions on it;
only accepted evidence can discharge the sequence's obligation.

### Configuration: release-readiness

Illustrative proposed YAML. The `full-qa-sweep` name is a required definition to
resolve during setup, not a claim that every installation already has it. All
syntax here is proposed. The new state reads above extend condition triggers;
they are not silently added to the existing phase-1 implementation task.

```yaml
# .orbit/auto_sequences/release-readiness.yaml
schemaVersion: 1
name: release-readiness
enabled: false
source: { branch: agent-main }
schedule:
  condition:
    scope: workspace
    when: >-
      now >= timestamp('2026-10-31T00:00:00-07:00')
      && board.empty
      && (!has(auto_tasks['release-prep'].last_succeeded_at)
          || now - auto_tasks['release-prep'].last_succeeded_at >= duration('24h'))
    for_minutes: 30
expire_when: "now >= self.started_at + duration('24h')"
steps:
  - id: qa
    auto_task: full-qa-sweep
    when: "board.empty"
    require_result: release-readiness/v1
    reuse_when: >-
      result.status == 'passed' && result.revision == sequence.revision
      && now - result.completed_at <= duration('24h')
  - id: security
    auto_task: security-review
    when: >-
      self.prev.status == 'done'
      && now >= self.prev.end_time + duration('30m') && board.empty
    expire_when: "now >= self.wait_started_at + duration('8h')"
    require_result: release-readiness/v1
    reuse_when: >-
      result.status == 'passed' && result.revision == sequence.revision
      && now - result.completed_at <= duration('24h')
  - id: prepare
    auto_task: release-prep
    when: >-
      steps['qa'].status == 'done' && steps['security'].status == 'done'
      && board.empty
```

Here "after October 30" means October 31 at midnight in Los Angeles, with the
explicit offset for that date. For "on or after October 30", use October 30
instead. The initial 30-minute quiet hold and the later 30-minute spacing are
different policies; remove either independently. To use Daniel's simpler chain,
replace security's condition with `self.prev.status == 'done' && board.empty`.
To make security independently eligible 30 minutes after sequence admission,
use `now >= sequence.started_at + duration('30m') && board.empty` instead.

The example waits up to eight hours from sequence admission for security's
condition, queue and accepted completion, within the 24-hour sequence bound.
This is not a promise that it starts exactly 30 minutes later: board work,
admission and resource limits can delay it. Preparation still performs its own
unchanged unfinished-work and no-release checks.

Freeze the branch SHA when the sequence starts. Reused audits must cover that
SHA and remain within their declared age limits when preparation is admitted,
not just when first bound. A source advance or expired prerequisite evidence
holds the sequence with the exact stale receipt; it never reuses old audits
for a newer release. A fresh bounded execution or audited recovery is needed.
The preparation step does not tag, bump versions, publish or acquire release
authority. A no-diff audit can complete; no Git merge is required to count it.

### Durability, recovery and surfaces

Persist coordination inside the existing job's versioned checkpoint: source
fire, frozen definition digests, source SHA, step conditions, immutable wait
starts, task/run bindings, action keys, accepted receipts and dispositions.
Reserve `(sequence execution, step id)` with its input digest before minting.
Resolve that same key after an ambiguous reply. A generation fence prevents
two clock passes from minting or starting the same child twice.

False means waiting. Unknown means waiting with its unavailable operand shown;
it cannot fire, satisfy a prerequisite or reset a wait clock. A child failure,
rejection, cancellation or human archive holds that step with its identity.
Members whose expressions require it to be done keep waiting; independently
eligible members may still run within the sequence's expiry. The sequence cannot
report success while any required member is unsatisfied. Do not automatically
recreate the task or undo the intervention. Ordinary child
pipeline retries remain bounded by their existing policy. Audited coordinator
recovery preserves successful steps and original evidence, rechecks freshness,
and never resets an expired deadline without an explicit extension/new execution.

Definition edits affect new executions. Active work retains its frozen graph and
conditions, while current authority and enablement are always enforced. Disabling
the sequence pauses new mints/starts, still observes children and consumes its
wait budget. Cancelling the coordinator stops its future actions and preserves
children; stopping a child requires its separate authorized control. Re-enabling
does not create another execution for an already-admitted edge. Retain origin,
chain-depth limits, the one-active-execution rule and normal resource budgets.

Proposed `orbit auto-sequence list|show|explain` and read-only MCP equivalents
show each clause, default ordering, predecessor, wait start, expiry, next known
time threshold, task/run link, reused result, source freshness and outcome. Show
active steps together, including independent ones. The Automation view uses the
same projection. An unrecognized/unknown time expression may require the next
clock tick; observation precision is the clock cadence, not a timer guarantee.
Use existing audited run/control mechanisms for mutations. Old clients must
preserve the new checkpoint schema or refuse before writing. Older binaries
must explicitly refuse sequence definitions; unsupported definitions must never
appear enabled/healthy while doing nothing. Installation compatibility checks
must make that limitation visible before activation or downgrade.

## Non-goals

- New arbitrary actions, nested sequences, loops, dynamic step creation or a
  second workflow engine. Existing jobs own execution and recovery.
- Cross-workspace composition or follower-owned coordination. Followers execute
  normal claimed children; owner receipts settle sequence steps.
- Changes to existing auto-task schedules, automatic approval, bypassing user
  archives, or release/publishing authorization from a configuration file.

## Concerns and honest limitations

- Conditional members are not automatically sequential. Explain must make the
  implicit/default rule and every explicit condition visible before enabling.
- Board emptiness is a snapshot, not a reservation against future user actions.
  Admission is rechecked, and source/evidence freshness protects preparation.
- Existing QA/security templates do not automatically supply a readiness receipt.
  Their deterministic evidence adapters are part of enabling this use case.
- Definition conditions referring to standalone history may not suit sequence
  use. Explain names that guard; composition does not override it silently.
- Requiring exact-revision reuse can cause another audit after even a small
  change. The first version prefers that explicit cost to unproven coverage.

## Phasing

1. **Schema and explain.** Parse disabled definitions, check names, CEL bindings,
   implicit order, literal references and finite outer expiry. Extend condition
   facts explicitly; show plans without minting or provider calls.
2. **Durable composition.** Integrate keyed task binding/minting, owner admission
   guards, observation, expiry, dedupe and recovery with existing jobs/clock.
   Keep definitions disabled on older/unsupported clients.
3. **Release readiness.** Add QA/security result adapters and freshness checks,
   then validate this disabled example end to end with isolated public fixtures.
   Operator enablement and ordinary execution authority remain separate steps.

## Open questions

1. Should a future version allow independently scheduled nested sequences?
   Keep that out until a concrete case needs more than named auto-task members.
2. Should evidence reuse permit a proven covered change range instead of an exact
   SHA? No such equivalence is assumed by the initial release-readiness contract.

## Acceptance (feature level)

- Three steps without conditions execute in order. A later conditional step can
  become eligible before its predecessor finishes; explicitly referencing
  `self.prev.status == 'done'` restores that dependency. Cycles/unknown fields
  fail load with the file and expression position.
- A previous-step-done plus board-empty condition waits through outside work,
  including between mint and admission, without its own bound task deadlocking
  it. Siblings and ordinary work are not silently excluded.
- Time spacing uses accepted step end times. Sequence/step expiry produces an
  explicit timeout across restarts and downtime, never an infinite false wait,
  duplicate task, fabricated completion or implicit worker cancellation.
- Concurrent standalone and sequence mints preserve dedupe and immutable binding;
  lost replies resolve the original task. Reuse verifies definition, source,
  result contract and age; no-diff completion does not require a PR.
- A done audit with blocking findings or unavailable proof cannot unlock release
  preparation. Source advances and evidence ageing are rechecked at preparation.
- Recovery preserves prior step evidence and human interventions. Disabled or
  unsupported definitions/clients cannot admit new work or erase obligations.
- Release readiness can be explained and exercised through isolated public
  CLI/runtime fixtures without enabling a live sequence, dispatching providers
  or performing release actions. Results report skips and unavailable proof.

## Task References

- ORB-19973 — records Daniel's sequence request and design decisions in this session.
- ORB-19990 — scopes condition-trigger language/explain only; this proposal does
  not change that task or authorize sequence implementation.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
