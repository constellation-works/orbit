---
title: Operation Mode — Vision
owner: codex
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Draft
feature: operation-mode
doc_role: vision
type: design
summary: Proposed scoped authority and captured policy for autonomous operation, with freshness, bounded recovery, rollout, and evaluation.
tags: [operation-mode, authorization, recovery, evaluation]
paths: ["crates/orbit-core/src/application/job/**", "crates/orbit-core/src/adapter/engine_host/v2_host/**", "crates/orbit-config/src/**"]
related_features: [activity-job, routines, task-artifacts, auditability]
related_artifacts: [ORB-11314]
---

# Operation Mode — Vision

**Proposed; documentation only.** Every policy rule and example in this file
describes a candidate design, not an available command or accepted runtime
contract. [Current behavior](./2_design.md) provides the source evidence. The
proposal reuses the existing pipeline engine and admission/recovery boundaries.

## 1. Open Questions

1. **Names:** adopt supervised/autonomous, or another pair that communicates
   authority without implying speed or replacing engineering judgment?
2. **Standing operation:** support a non-expiring grant in the first release,
   or initially require a bounded window? Recommend bounded windows first;
   an eventual standing grant must explicitly cover future admissions.
3. **Initial limits:** validate the candidate five-minute pilot due interval,
   ten-leaf ceiling, two recovery episodes per task, and thirty-minute aggregate
   recovery allowance against observed costs. Provider budgets require explicit
   units and handling of unavailable usage data before becoming hard limits.
4. **Freshness:** start with exact task-meaning and landing-commit equality,
   then consider a narrower affected-path check only if re-piloting costs
   justify the additional correctness burden. A freshness age limit alone is
   insufficient.
5. **Existing completion syntax:** preserve omission of `--complete` as review
   in compatibility mode. After explicit mode enablement, should the CLI adopt
   an explicit `--completion review|done` override? This needs tri-state input;
   the current boolean cannot distinguish omission from an explicit refusal.
6. **Accounting and revocation:** finalize provider usage coverage, budget
   reservations, and the atomic hard-revocation interface before enabling
   unattended completion. Routine stop and expiry have the softer semantics
   specified below.

## 2. Prior Work

### Existing Orbit mechanisms

Task-pilot provides bounded investigation and validated apply; workspace auto
provides a durable drain; task-auto/PR/local jobs own delivery; triage and engine
hooks own recovery. The [current seam inventory](./2_design.md) links the exact
code. Reuse them so mode selection changes inputs and eligibility rules rather
than constructing a second scheduler or delivery engine. Historical decision
records are not used as authority for this proposal.

### Alternatives considered

| Alternative | Benefit | Reason to prefer the proposal / cost of that preference |
| --- | --- | --- |
| Keep manual orchestration with independent flags | No new authority representation | Repeated coordination remains expensive; presets add a resolution/explanation surface to maintain. |
| Fast/slow naming | Matches the original examples | Conceals promotion/completion rights inside a performance label; authority-oriented names still require cadence/capacity controls. |
| Treat nonempty selectors as prepared | Simple and cheap | Selectors can survive changed intent or unresolved decisions; comprehensive freshness costs pilot work and durable evidence. |
| Let an LLM promote and recover freely | Flexible handling of novel situations | Makes authorization and retry limits prompt-dependent; deterministic enforcement requires explicit result contracts. |
| Global autonomous flag as universal permission | One easy switch | Silently covers new workspaces and future tasks; scoped enablement costs one explicit authorization step. |
| Re-resolve all policy every loop | Immediate retuning | Mid-run completion rights become unpredictable; snapshots require explicit stop/restart or bounded live controls. |
| Generic policy language / resident second orchestrator | Broad extensibility | Adds machinery with no second concrete need; small typed settings may need later extension. |

## 3. What May Be Distinctive

The useful distinction is an accountable boundary between engineering decisions,
worker investigation, and deterministic execution. No novelty claim is needed:
the test is whether this composition removes mechanical Astra turns without
turning ambiguous work into silent permission.

### 3.1 Responsibility and state transitions

| Work | Owner | Durable outcome |
| --- | --- | --- |
| Identify valuable improvements; settle scope, architecture, priorities, ambiguous intent | Astra, with human decisions where required | Task intent, plan, and explicit decision disposition |
| Inspect code, propose exact selectors, identify duplicates/conflicts, diagnose failures | Bounded pilot/recovery worker | Evidence and recommendations; no self-granted lifecycle rights |
| Select due work, validate freshness/grant, promote, reserve/admit, poll, retry within limits | Core application rules through existing jobs | Task/run transitions, reasons, provenance, consumed budgets |

Preparation evidence is not a new task lifecycle status. A proposed task can
have a successful assessment and still await a decision. The mechanical path is
`proposed + eligible evidence + promotion grant → backlog → existing admission
gates → execution → review → authorized completion`. A grant permits a
transition; it does not bypass dependencies, validation, reservations, or PR
checks. An environmental failure may take bounded triage → backlog; a scope or
architecture blocker remains blocked with an escalation. Terminal tasks are
never reopened by this mechanism.

### 3.2 Resolution, enablement, and captured policy

Use a single global default, a workspace override, and explicit run overrides.
For **preferences**, resolve built-in supervised → global → workspace → run.
At each layer, an explicit preset selection resets preset-managed fields to
that preset's defaults, then that layer's explicit fields apply. An omitted
preset preserves inherited fields. Lists replace; they do not silently union
scope. Record the winning source for each field and reject unknown values.
This avoids a workspace choosing supervised while inheriting a hidden global
autonomous completion preference.

These precedence rules are a proposal for the new settings; they must be
implemented explicitly in the config/Core boundary, not assumed to emerge from
today's TOML merge. There is no ambient environment-variable grant.

**Authorization is evaluated separately from preferences.** A global autonomous
default indicates desired behavior. A proposed enable operation must display
and persist the exact workspace identities, selection scope, whether future
matches are included, promotion/completion rights, admission expiry, resource
limits, and actor/provenance. Explicit enablement is the permission to perform
those actions within those bounds. Editing a preference, an agent's task text,
or an arbitrary job input cannot manufacture that permission. Use existing
caller/claim checks; enforce it in Core for CLI, tool, and scheduler callers.
V1 global means defaults on the owning authority, not automatic federation-wide
control or remote workspace enrollment.

Recommend a finite task-ID set for the first rollout. A dynamic selection must
explicitly authorize future matching tasks; crew filters only restrict execution
and do not substitute for product-scope authorization. Scope expansion, crew
reassignment, and architecture changes require a new decision and may invalidate
preparation even when the task ID stays the same.

At run admission, resolve and persist a versioned effective policy in existing
run state/input: preset and field provenance, workspace, task-selection rule,
grant reference/revision, admission deadline, completion cap, preparation rules,
recovery allowances, and initial concurrency. Keep authorization identifiers
distinct from caller-supplied booleans. Record the relevant job definition
identity so later diagnostics can explain what actually executed.

Queued requests have no right to outlive expiry: validate again when they can
be admitted. A coordinator captures its policy once; before each detached child
insert, atomically recheck stop, expiry, grant revocation, task eligibility, and
capacity, then link the child and its inherited policy. The child can narrow
that authority but cannot acquire more by rereading newer global config. Reuse
the existing admission transaction rather than a client-side check/submit loop.

The effective action is the intersection of the resolved request, explicit grant,
and applicable repository/caller constraints. Reject an unauthorized explicit
escalation; disclose any configured ceiling that reduces requested behavior.
Never silently make review become completion. A repository constraint such as
“open a PR to agent-main, do not merge” caps delivery at review even for an
autonomous preference. Human approval to implement a task and permission to
merge its PR remain distinguishable. No preset enables auto-merge implicitly.

### 3.3 Compact effective-policy examples

The following objects illustrate an **explanation view**, not accepted config
keys or commands. `grant` is a conceptual reference to durable authorization.

```json
{"global":"autonomous","workspace":"supervised","run":{"concurrency":8},"effective":{"preset":"supervised","promotion":"separate_approval","completion":"review","leaf_ceiling":8}}
```

Workspace preset selection resets autonomous defaults; high parallelism alone
grants no promotion/completion. Existing explicitly authorized `--complete`
remains usable independently of presets.

```json
{"global":"supervised","run":{"preset":"autonomous","window":"2h"},"grant":{"selection":"explicit task set","promote":true,"complete":true},"repository":{"delivery_cap":"review","base":"agent-main"},"effective":{"completion":"review","leaf_ceiling":10,"pilot_due_seconds":300,"recovery_episodes_per_task":2}}
```

An explicitly enabled run prepares and promotes eligible tasks and opens PRs;
the repository cap prevents merge. Requested ten may still yield only three
admissions if there are three independent ready tasks or only three free slots.

```json
{"preference":"autonomous","grant":null,"explicit_request":"promote_and_complete","outcome":"refused","reason":"scoped_authorization_required"}
```

No grant is inferred from configuration or from the fact that a worker can call
a tool. After successful enablement, later eligible tasks inside the recorded
scope do not require repeated permission questions.

### 3.4 Preparation and promotion validity

Extend the existing pilot/apply path with a durable assessment tied to a digest
of task title, description, acceptance criteria, plan, context selectors, tags,
dependencies/decision dispositions, and execution assignment, plus the pinned
landing revision, pilot contract version, result, and timestamp. Exclude unrelated
history/comment timestamps from the digest so audit writes do not invalidate
their own assessment. Applying selector proposals must bind the certificate to
the resulting task meaning, under the same task write boundary.

Promotion requires a fully successful applicable partition and positive evidence
for this exact task version; failed/partial/missing fields are not readiness.
It also requires a valid disposition, resolvable scope, satisfied dependencies,
no unresolved utility/surface/current-contract decision blockers, and a valid
promotion grant. Duplicate/already-landed findings withhold promotion for a
separate disposition; no-diff or host-operational work needs explicit evidence
and its appropriate gate, not dummy selectors. Initially withhold such special
dispositions from automatic promotion until that gate is specified.

Start conservatively: task-meaning equality and unchanged landing commit are
required at promotion and again before execution admission. A changed source,
intent, dependency result, or decision disposition makes assessment stale and
requires another pilot; a maximum age is an additional limit. A pilot is an
advisory engineering assessment, not proof that the implementation will be sound.
Existing validation/review still applies. A later optimization may check affected
paths, but must account for transitive APIs/configuration instead of trusting
unchanged target files alone.

Schedule only missing/stale assessments in the authorized scope, including tasks
with populated selectors. Coalesce repeated changes for one task version and
share the pilot pipeline's partition/overlap/capacity controls. Five minutes is
a candidate due interval observed by the existing sweep clock, not an extra
resident timer or a guarantee of start latency. Operator-disabled routines,
host pins, and timeouts remain effective; enablement must explicitly identify
which routine behavior it is authorizing. Do not silently rewrite custom cron
files. Prefer a mode-aware due check in the existing scheduling path, with a
single declared owner for cadence and a preview of conflicts with custom routines.

### 3.5 Concurrency, expiry, restart, and retuning

Ten is a leaf-run ceiling, not a batch size or a machine-wide agent count.
Admission is bounded by requested ceiling, catalog hard limits, available
host/provider capacity and budgets, dependencies, workspace claims, and context
conflicts/reservations. Account for pilots, recovery workers, nested task fan-outs,
and the additional epic separately; advertise both the leaf ceiling and actual
active agent total. Never reserve ten task workers while starving the recovery
needed to release their slots. A later implementation must expose the limiting
reason through existing readiness diagnostics.

Store an absolute admission deadline. Normal expiry stops new preparation,
promotion, and execution admissions in that scope; work already admitted retains
its captured policy and may complete after the window. An admitted pilot may
finish and save evidence after expiry, but that evidence cannot authorize a new
promotion. A promoted backlog task is not an admitted execution child. Reject
its late admission unless a still-valid grant covers it. A retry of an existing
failed step can finish within captured recovery bounds; a terminal task run
requeued through triage is a new execution admission requiring a current grant.

Use the existing durable stop control for a drain: it prevents admissions and
does not cancel children. A mode-level stop must additionally disable new
mode-owned preparation/promotion in its scope and explain independent scheduled
work it does not own. Explicit child cancellation remains separate. Hard
revocation is also separate: prevent further privileged actions, including
completion, for admitted work at the authoritative boundary, record the reason,
and escalate any work that cannot safely finish. Do not label ordinary expiry
or a preference edit as hard revocation.

On restart, load snapshots, absolute deadlines, stop/revocation markers, counters,
and linked children. Never restart the clock, reset recovery allowances, or
resolve newer config for an old child. A repeated admission/promotion uses the
same durable task/assessment/grant identity; perform task-state rechecks and
idempotent apply. Before repeating external delivery, reconcile the branch/PR
and checkpoint so a lost acknowledgement does not open a second PR.

Concurrent operators use workspace claims and revision checks at the server.
Grant or scope changes require compare-and-set; a losing update must reread.
Existing absolute concurrency writes retain their documented last-writer-wins
behavior, while automation should supply the expected revision. Stop wins against
future child creation at the existing transaction boundary. Replayed stop is
successful without another transition.

Global/workspace retuning affects future runs only. Live concurrency uses the
existing audited override, preserving run ID, deadline, and authority; lowering
it waits for active workers to finish. Changing scope, completion, or extending
a deadline requires explicit authorization and a replacement admission window
after stopping the old one. Already-linked children remain counted so the new
coordinator cannot double-claim their tasks. Decreasing permissions is immediate
only through explicit revocation, not silent reinterpretation of a snapshot.

### 3.6 Bounded recovery and escalation

Autonomous mode would enable eligibility-driven recovery scheduling, not invent
unlimited retries. Reuse engine hooks for failed steps and triage for terminal
run failures. Preserve existing retry limits, timeouts, task coupling checks,
and the environmental-only automatic re-backlog rule.

Add a durable aggregate allowance shared across those paths and task retry
lineage: candidate defaults are two recovery episodes and thirty minutes per
task, with optional provider token/cost caps and a run-wide allowance. An episode
includes its diagnosis and resulting retry, so nesting/requeueing cannot reset
the budget. Reserve allowance before worker dispatch; count crashes/timeouts
as attempts. For unknown usage, enforce attempts and wall time and report cost
as unknown; do not claim a cost cap was enforced without metering/reservations.

Retry automatically only when evidence identifies an eligible transient or
environmental condition and remaining authority/resources permit it. Back off
and require new evidence before repeating the same failed repair. A denied
operation, unresolved architecture choice, missing authorization, dependency
block, or exhausted allowance produces a durable escalation rather than a
promise of “no blocked tasks.” Recovery cannot expand selectors, change branch
targets, waive CI, assign a more expensive crew, or merge without the relevant
decision and grant. If a delivery repair needs broader scope, record that need.

Record the failing phase, task/run lineage, source revision, diagnosis/evidence,
attempts and consumption, actions already taken, remaining allowance, and the
specific decision or external change required. Deduplicate repeated escalation
for the same failure/version. Route investigation to a bounded worker and scope
or architecture choices to Astra; human approval remains necessary wherever the
task/repository requires it. A genuine blocker stays visible and does not prevent
unrelated eligible work from continuing.

### 3.7 Compatibility and staged rollout

1. **Explain only:** add typed preference resolution and an effective-policy
   preview with source/cap/reason fields. No scheduled work or authority changes.
   Existing installs, custom jobs/routines, and omission of `--complete` retain
   current behavior. Unknown policy versions fail closed for privileged actions.
2. **Evidence first:** extend pilot freshness and record would-promote decisions
   without transitions. Test changed criteria/plans, stale source, warnings,
   duplicates, no-diff work, malformed partitions, and operator edits during apply.
3. **Bounded promotion:** opt in one workspace and a finite task set, short
   window, review-only delivery, low concurrency. Add server-side grant checks,
   admission snapshots, counters, and audit linkage in existing stores. Test
   expiry/stop races, restart, two operators, grant revocation, and partial apply.
4. **Recovery and capacity:** enable shared recovery bounds, then raise toward
   ten only with capacity evidence. Test nested recovery, lost delivery replies,
   provider throttling, and retry lineage that crosses terminal runs.
5. **Completion opt-in:** enable captured completion authority only after the
   repository-specific delivery contract and accounting pass evaluation. Show
   that direct CLI/tool/job routes cannot bypass the same Core checks. Broader
   dynamic or standing grants come last; they are not migration defaults.

Each stage is separate implementation work, with affected current docs and
persisted-format compatibility reviewed in that change. Existing runs without
mode metadata retain their original completion inputs; migration must not grant
promotion or synthesize broader rights. Rollback stops new mode admissions and
retains readable snapshots for admitted work. If an older binary cannot enforce
them, stop/drain with the supporting version before downgrading. No automatic
rewriting of user routines or resource catalogs is part of this proposal.

### 3.8 Observability and evaluation

Extend existing audit/run/task records with policy/grant revision, field sources,
pilot version, promotion/withhold reasons, admission limiting reason, parent/child
lineage, recovery consumption, expiry/stop/revocation, and escalation disposition.
An operator should be able to answer “why did this task start or merge?” and
“what can still happen after stop?” from durable evidence. Reuse run show,
readiness, and audit projections; avoid a second operational state view.

Evaluate comparable supervised/autonomous cohorts by scope, complexity, crew,
repository, and delivery cap. Define an **accepted change** as a delivered change
accepted by the repository's review/validation process within a stated follow-up
window, with rejected/reverted changes and no-diff outcomes reported separately.
Do not count a successful drain or newly opened PR as accepted delivery.

| Measure | Evaluation contract and coverage |
| --- | --- |
| Mechanical orchestrator turns / accepted change | Classify Astra turns as coordination vs engineering decisions using an explicit sampled rubric. External sessions need imported or manually recorded usage; current job telemetry cannot supply this alone. |
| Orchestrator consumption / accepted change | Report input/output tokens and available billed cost for orchestration, separately from workers, pilots, and recovery; also report combined totals so cost is not merely shifted. Missing usage is unknown, with a coverage fraction. |
| Quality and intervention | Track review revisions/rejections, regressions/reverts, validation failures, human/Astra interventions, and escalation usefulness; distinguish needed decisions from avoidable mechanical repair. |
| Throughput and waste | Time from authorized-ready to admission and acceptance, idle slot time, stale pilot reruns, duplicate dispatch, retry amplification, and exhausted budgets. |
| Existing reliability | Use settled-run failure and recovery-engagement rates with their denominators, excluded outcomes, low-sample and truncation disclosures; engagement alone is not successful recovery. |

Collect a baseline before activation and report sample sizes and comparable
windows. A candidate graduation threshold is at least 25% fewer mechanical
orchestrator turns and lower orchestrator consumption per accepted change over
at least 30 accepted changes per cohort, with no unauthorized actions and no
observed increase in serious regressions or required human intervention. These
are proposed pilot thresholds, not statistically proven guarantees; small
samples and case-mix differences need reviewer judgment. Track all attempted
work so excluding failures cannot manufacture apparent savings.

### 3.9 Concerns & Honest Limitations

| Risk | Severity | Mitigation / remaining cost |
| --- | --- | --- |
| A broad grant approves unforeseen future work | High | Explicit scope/future-match semantics, bounded initial rollout, server enforcement and revocation; dynamic scopes still require careful decisions. |
| Stale pilot evidence permits the wrong change | High | Meaning/source binding and atomic rechecks; exact commit equality can repeatedly invalidate preparation on a busy branch. |
| Retry or concurrency amplification exhausts resources | High | Shared lineage budgets and actual-agent accounting; token/cost coverage is incomplete today. |
| Expiry is mistaken for cancellation | High | Show admitted children and their retained completion rights; provide separate stop, cancel, and revocation semantics. |
| Preset/config/custom routine drift produces surprising behavior | Medium | Field provenance, single cadence owner, versioned snapshots and explicit custom-resource compatibility. |
| Fast automation preserves low-value intent | Medium | Astra owns utility decisions; a warning-free pilot is not a substitute for prioritization. |
| Metrics reward shifted or hidden work | Medium | Count total consumption, attempted changes, unknown usage, and external orchestration coverage, alongside quality. |

## 4. References

### Orbit-internal

- [Overview and presets](./1_overview.md).
- [Source-verified current seams](./2_design.md).
- [Architecture and persistence ownership](../../../ARCHITECTURE.md).
- [Design conventions](../CONVENTIONS.md).

### External

None required for this proposal. Its factual baseline is current repository
source and versioned configuration, not historical decisions or product claims.

## Task References

- [ORB-11314] — formulates the proposed policy, alternatives, rollout, and evaluation.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
