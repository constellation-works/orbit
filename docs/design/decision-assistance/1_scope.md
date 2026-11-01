---
title: "Scope: bounded decision assistance"
owner: codex
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Draft
stage: parked
next: "Improve rubrics/packets, independently adjudicate permitted data and register an unopened holdout before any activation; advisory rollout remains parked."
feature: decision-assistance
doc_role: scope
type: design
summary: "Scope: optional typed model judgments, parked rollout and a proposed advisory delivery-quality grader."
tags: [decision-assistance]
---

# Scope: bounded decision assistance

Status: **advisory rollout is parked** after both preregistered development
screens failed. The offline evaluation contract and optional generic service
are implemented; this delivery adds the already admitted private task-quality
shadow consumer as repository code. The
[task-quality contract](specs/task-quality.md) documents owner-clock recovery,
semantic freshness and private feedback. All consumers default off. The failure
consumer and human-facing advisory surfaces are archived/deferred; implementing
them is no longer the next step. Private collector code completion is separate
from effectiveness and adoption: this delivery authorizes no deployment, paid
replay, prompt tuning, configuration change or API activation.
The [service contract](specs/service.md) documents the host-only configuration,
durable ledger and public application boundary. A new
[delivery-quality grader proposal](specs/delivery-quality.md) defines a separate
offline evaluation candidate. Its offline contract and a default-off owner-clock shadow
runtime are in the repository; this design does not activate it or authorize adoption.

Bearing: builds on [activities and jobs](../activity-job/2_design.md),
[auto-tasks](../auto-tasks/2_design.md), the shipped
[task-pilot](../../../crates/orbit-core/assets/activities/task_pilot.yaml),
and the [human decision channel](../human-decision-channel/2_design.md).
Model assistance produces evidence annotations. The existing task, run, review,
admission and human-decision contracts continue to own actions and authority.

## Development evidence and disposition

The operator's 2026-10-29 development screens provide negative evidence for the
current rubrics and packets, not an accuracy or rollout pass:

| Consumer | Development provenance | Result |
|---|---|---|
| Failure categorization | 30 bounded historical diagnostic packets compared with prior investigator/agent categories; no independent human ground truth. | 26.7% agreement with the agent baseline and 60% substantive coverage; both predeclared quality screens failed. |
| Task quality | 24 wholly invented tasks and Sonnet-authored synthetic labels, followed by Codex packet review with disagreements frozen before answers; no independent human labels or representative holdout. | 20 fully answered packets and four mixed predicate/refusal packets. Strict whole-result validation and predeclared confidence bands gave 119/212 consensus-cell matches (56.13%), excluding four predeclared uncertain label cells. Outcome agreement was 75%, scope 78.26%, criterion observability 20% and conflict 75.56%; every question group failed its predeclared screen. |

The operator's development-study task retains the protocols and readouts as
`decision-assistance-development/{protocol,readout}.json` and
`decision-assistance-quality-development/{protocol,readout}.json` (see Task
References). Their protocol SHA256 values are
`766a42cb2e1f7f7b685c22682c1a84c0dab885c7d5f07ee2d8e38e6b5a3f45d3`
and `a8d2baa0582058f51513c9976715582837e5d2464f0bdd963c5397cfe81948d8`
respectively. The task-quality protocol was registered at
2026-10-29T08:59:29.189830Z, before the first request at 09:00:40.190483Z;
no prompt or threshold adaptation followed the readout. Only invented tasks
were sent in that screen, with no additional historical diagnostics or chats.
No real diagnostic content is reproduced here.

These small development samples do not establish calibration, production
accuracy, operator time savings or utility. Recorded latency and usage-based
cost estimates do not supply that missing evidence; actual billed costs remain
unknown without a billing ledger. Failure-screen numbers do not describe task
quality, and synthetic labels are not adjudicated human ground truth.

Before **any activation**, improve the rubrics and evidence packets, obtain
independently adjudicated permitted data, and register targets, sample sizes and
the protocol for a fresh, unopened family/time holdout. Evaluate that holdout
without adapting to its answers and establish useful coverage, error burden,
latency and cost against the existing workflow. An explicit operator evidence
and adoption decision remains required. Until then rollout stays parked and
the implemented foundation and private collector stay off by default.

## Problem

Orbit's agents repeatedly make small judgments inside larger investigations:
classify a failed run, identify an ambiguous acceptance criterion, compare two
reports, or choose a severity from a rubric. Today those judgments are embedded
in agent-loop instructions alongside searches, source inspection and writes.
There is no independently evaluated, reusable result for a bounded question.

Two practical starting points are:

- [Run failure patterns](../../../crates/orbit-core/assets/auto_tasks/run-failure-patterns.yaml)
  asks an agent to categorize evidence into product defects, misclassified holds,
  provider/capacity problems, upstream task quality, environment/flake, or true
  one-offs. This is a small classification inside a much larger investigation.
- [Task authoring](../../../crates/orbit-core/assets/skills/orbit/references/task-authoring.md)
  requires a concrete outcome, coherent scope and observable acceptance criteria.
  The [pilot](../../../crates/orbit-core/assets/activities/task_pilot.yaml) later
  inspects source contracts, footprints and validation feasibility. A lightweight
  quality assessment immediately after creation could catch unclear wording
  earlier, without pretending to establish that the implementation is feasible.

OpenAI's [Decisions API](https://developers.openai.com/api/docs/guides/decisions)
returns predicate probabilities, choices and rubric scores over supplied evidence.
As checked on 2026-10-28, it supports `gpt-6-luna` through `POST /v1/decisions`.
It does not browse a repository or execute the actions it classifies. No Orbit
accuracy, latency or cost benefit has been measured; this proposal makes those
measurements a prerequisite for consuming its answers.

## Goal

Make bounded judgments reusable, inspectable and measurable. The original two
consumer screens remain negative evidence and their rollout remains parked.
The delivery-quality grader adds a separate proposed evaluation of programming
quality alongside observed outcomes; it does not reactivate an older consumer.
Keep the six original later candidates recorded for independent evaluation and
Orbit's contracts provider-neutral, with explicit host-owned enablement for
sending evidence outside the host.

## Shape

### Candidate inventory and order

The original priority labels below describe the initial scope, not an active
execution queue. These are opportunities to evaluate, not approval to automate
every action; current implementation and parked status are recorded above.

| Order | Candidate | Evidence and typed question | Consumer and limit |
|---|---|---|---|
| First | Failure-pattern categorization | Bounded failure evidence; choose one category from the existing taxonomy plus `insufficient_evidence`. | Failure-pattern report suggests categories; investigator verifies causes and dispositions. |
| First | Task-creation quality checks | Title, description and individually numbered criteria; independent predicates for unclear outcomes, non-observable criteria, contradictory requirements and mixed outcomes. | Author and pilot see advisory flags; no task rejection or approval. |
| Proposed; offline contract and default-off shadow runtime | Delivery-quality grader | Frozen task/diff/contracts and validation evidence; independent anchored 1–5 choices for scope fit, robustness, structure, standards, verification and maintainability, with abstention. | Separate advisory quality profile; no scorecard term, routing change or review authority. See [proposal](specs/delivery-quality.md). |
| Later | Task tag suggestions | Task text and an approved workspace vocabulary; one predicate per tag because several may apply. | Suggest descriptive tags; separately evaluate automatic application and every tag with execution effects. |
| Later | Backlog duplicate/landed candidates | Search-selected task pairs, scopes and available delivery evidence; choose same issue, partial overlap, distinct, or insufficient evidence. | Rank candidates in backlog hygiene; commit inspection establishes landed coverage. |
| Later | Friction grouping and rejection classification | Retrieved report pairs and current rejection rationale; classify same mechanism and administrative versus terminal rejection. | Curator inspects current behavior and records canonical mappings; similarity cannot close a friction. |
| Later | Regression nature/escape classification | Regression task, culprit task, landed diff and available gate evidence; choose from existing `nature` and `escape` enums. | Propose relation annotations to escape-to-guard; preserve `unclassified` and existing severity/sites. |
| Later | Pilot pre-screen | Criterion text and bounded source/tool evidence; flag possible native OS, owner-host, UI or post-deploy needs. | Point pilot at clauses to inspect; never infer admission requirements from wording alone. |
| Later | Review severity suggestion | A confirmed finding and verified impact; choose one existing severity level or insufficient evidence. | Second opinion for the verifier; no defect confirmation, rejection or scorecard write by this service. |

Existing instruction anchors for the later candidates are
[friction curation](../../../crates/orbit-core/assets/auto_tasks/friction-curation.yaml),
[escape-to-guard](../../../crates/orbit-core/assets/auto_tasks/escape-to-guard.yaml),
[finding verification](../../../crates/orbit-core/assets/activities/verify_finding.yaml),
and the task-pilot and authoring instructions above.

### Proposed consumer: delivery-quality grading

A [delivery-quality profile](specs/delivery-quality.md) would describe programming
qualities that completion/review/regression outcomes do not directly measure.
Assess the exact attributable delivered change against its scope and repository
contracts, using six independent anchored dimensions rather than one overall
number. Missing context must produce an explicit gap, not an average over the
most convenient files. Grade identity follows the actual implementation attempt,
not a task's later crew assignment; reviewer repairs need separate attribution.

This proposal does not change the current agent score or task-type routing.
The existing two development screens do not evaluate delivery grading. Require
its own human-adjudicated calibration and fresh holdout, then a separately
authorized private shadow study before considering an advisory surface.

### Shared request and result contract

Each consumer defines a versioned question set, allowed answers, rubric and
bounded evidence builder. The host supplies evidence references and provenance;
the model cannot invent references, task IDs, commands or action arguments.
Predicate questions are independent and do not assume mutually exclusive flags.
Use choices for categories, including an explicit insufficient-evidence option;
an ordinal score is not a substitute for several independent quality checks.

The proposed durable result contains:

- Subject identity and workspace/owner, immutable evidence digest, source revision
  or task-content digest, collection timestamp, selected evidence references,
  omissions and truncation indicators.
- Question-set, rubric, evidence-builder and result-schema versions; adapter,
  requested/returned model identity when available, request ID and attempt ID.
- Per-question typed answer and the provider's probabilities when supplied;
  local disposition `suggested`, `uncertain`, `refused`, `unavailable`, `invalid`
  or `stale`. Store no fabricated explanation or chain of thought.
- Recorded input/output usage, observed latency and estimated cost with the
  pricing version. Missing usage or pricing is unknown, not zero.

Accept only exactly the requested question names and expected types, allowed
values and finite probabilities within their documented ranges. Refusals,
missing answers and malformed responses remain explicit. A complete, valid
response can still be uncertain. Thresholds are per question and policy version,
chosen from labeled evaluation; the model's probability is not assumed calibrated.

The reusable boundary should sit behind an application port: evidence selection
and consumers in their existing application/workflow owners, typed request/result
contracts in the appropriate shared layer, and vendor HTTP/auth translation in an
adapter. Confirm exact crate placement against `ARCHITECTURE.md` when implementing;
do not introduce a crate or cross-crate dependency in this documentation change.
Keep task-quality and failure taxonomies in the owning consumer, with narrow
toolchain-specific evidence parsing in workspace configuration or plugins.

### First consumer: failure-pattern categorization

Keep collection, run identity deduplication, numeric aggregation, exact error-code
handling and Pareto selection deterministic where the data has typed fields.
Do not send already-known typed categories to a model to rediscover them.

The initial consumer is the recurring run-failure-patterns report, rather than a
new recovery hook on every failed step. Its collector supplies one bounded packet
per normalized failure class: job/activity and failing step, typed errors,
representative redacted excerpts, occurrence counts, source revisions, available
review-report fields, and evidence references. Record the collection window and
sample selection; label incomplete histories. Distinct machines and attempts keep
their existing identities even if display strings match.

Ask which existing category best describes the evidence:
`product_defect`, `misclassified_hold`, `provider`, `upstream_quality`,
`environment`, `one_off`, or `insufficient_evidence`. The category is separate from
the disposition (fix, covered, investigate, counted only). In particular, a
provider error is no exemption from investigation, and a single observed
occurrence alone cannot establish that a failure will never recur.

For example, a typed rate-limit error plus a log showing the failing request may
support a provider classification. It cannot establish that fallback would have
succeeded. The report shows that category as suggested alongside evidence links;
the investigator decides whether an admission/resilience defect exists and files
any deduplicated repair task under the current instructions. Changing input or
question versions invalidates reuse. Counts and operational failure rates never
change because of a suggestion.

### First consumer: task-creation quality checks

After the owning store commits a successful task creation through any supported
entry point, schedule a bounded assessment via a durable work item. Publishing
the creation event and its work item must be atomic or recoverable by reconciliation;
a crash between them must not permanently lose the assessment. The create response
does not wait for network inference. Disabled workspaces enqueue nothing.

Snapshot the title, description and acceptance criteria; retain stable criterion
references using their index plus text digest. Run deterministic shape validation
first: missing required fields, enum errors and malformed selectors belong to the
existing write validator, not this service. Assess semantic questions separately:

| Question | Unit and example | Expected annotation |
|---|---|---|
| Outcome unclear | Whole task: “Improve reliability” without an observable result. | `outcome_unclear` |
| Criterion not observable | Each criterion: “Code should be clean.” | `criterion_not_observable` with criterion reference |
| Requirements conflict | Task plus criterion pairs: “Use the existing schema unchanged” and “remove a required stored field.” | `requirements_conflict` with pair references |
| Outcomes need scope clarification | Whole task contains unrelated deliverables without a common outcome. | `scope_clarification` |

These examples illustrate wording, not automatic findings: an investigation can
validly deliver evidence instead of code, a multi-component change can be coherent,
and a task may own the capability its criteria require. Do not flag a future
implementation merely because the current source lacks it. Checks of source
existence, tool transport, dependencies, platform evidence and feasibility remain
the pilot's work. Model flags do not enter `pilot_warnings` or readiness gaps.

Before publishing, compare the current semantic-input digest to the snapshot. An
edit of title, description or criteria makes the result stale and schedules the
latest content with bounded coalescing. Comments and unrelated field edits do not
need a new assessment. Terminal tasks retain history without fresh automatic work.
Deduplicate by subject, content digest and question-set/policy versions. A repeated
task-create request that returns the same task does not produce duplicate work.

Publish structured advisory annotations with stable rule IDs and templated wording
such as “Criterion 2 may not identify an observable outcome.” The model selects
answers only; it does not rewrite criteria, fabricate advice, add comments on each
retry, or change tags, lifecycle, priority, crew, footprint or approval. The author
can dismiss an annotation with a reason; preserve that feedback for evaluation
without treating it as ground truth automatically.

### Execution, freshness and failure behavior

Owner-side workers use bounded concurrency, queue age, request timeout, retry
count and token/cost budgets. Claim work atomically; never hold a store lock during
network calls. Retries keep the logical assessment identity and record separate
attempts, including outcome-unknown calls that may already have incurred charges.
Only one matching result becomes current. Cached results require an identical
evidence digest, question set, policy and model configuration; a later provider
model revision must not silently reuse an older result as newly evaluated.

A disabled adapter, queue overflow, exhausted budget, timeout or malformed response
leaves existing creation and reporting behavior available. Persist a bounded
unavailable/uncertain result or coverage gap, without reporting a clean bill of
health. Backpressure may coalesce task edits and defer low-priority packets; reports
state coverage and do not count deferred packets as assessed. Switching the feature
off stops new calls; late results cannot mutate workflow state.

### Configuration and data boundary

Proposed host-owned configuration selects enabled workspaces, per-use-case mode
(`off`, `shadow`, `advisory`), adapter/model, credential reference, request/queue
budgets and retention. Defaults are off. Exact key names and command syntax are
documented in [the configuration reference](../../CONFIG.md); this scope
document describes the design boundary.
Shadow mode persists results for comparison without inserting suggestions into
the normal author or investigator view. Advisory mode displays evaluated results.

Use a host-side API key for the intended OpenAI project/organization, obtained
through the supported credential mechanism. Do not start a Codex/Claude crew or
pass the credential into an agent process to make these calls. Resolve credentials
only within the adapter and never persist their values. Operator enablement must
identify the destination and what evidence is sent. Workspace content cannot choose
a credential, endpoint or broader data scope. The human decision channel cannot
be answered by model assistance.

Send only permitted, bounded evidence after value-based redaction at the external
boundary. Run logs and task text are untrusted evidence, never instructions that
can change questions, allowed outputs or authority. Exclude credentials, arbitrary
attachments and ambient environment data; preserve evidence-access controls and
retention rules for results and samples. Record omissions that could affect a
judgment. Live evaluation requires explicit activation and budget; ordinary CI uses
recorded/synthetic responses and makes no external calls. Do not assume a personal
subscription's promotional credits cover this endpoint.

### Surfaces

Task detail and task-show output distinguish pending, current advisory, stale and
unavailable results from authoritative readiness and pilot warnings. Machine-readable
surfaces expose typed rule IDs, evidence references, provenance and disposition.
The failure-pattern artifact and summary add suggested category, coverage and
investigator disposition, while keeping the original taxonomy and counts.
Expose enablement, assessment counts, uncertain/stale/unavailable counts, correction
rates, latency and actual/estimated spend to operators. Exact CLI/API/dashboard
fields are implementation contracts still to be designed.

## Non-goals

- **Automated authority.** No task approval, defect verification, lifecycle change,
  hold clearance, review verdict, human answer, recovery command or dispatch.
- **General agent replacement.** Repository exploration, root-cause investigation,
  source-bound feasibility and repair remain in their existing activities.
- **New deterministic classification.** Existing typed codes, validation, identity
  matching, age thresholds and arithmetic do not need model judgment.
- **Immediate automatic tagging.** The eight original candidates and the new
  grading proposal are recorded, but the first implementation covers the two
  priority consumers. A later proposal must specify
  which tags may be applied and audit their downstream effects.

## Concerns and honest limitations

- A plausible category can conceal missing evidence. Unknowns, sampling and
  refusals must be visible; no probability threshold supplies missing proof.
- Model/schema validity and classification quality are different. Deterministic
  validators cannot certify that a well-formed answer is correct.
- Quality flags may create author fatigue or penalize valid investigation work.
  Measure usefulness, dismissed flags and missed defects, not just flag volume.
- Source and logs may contain sensitive text even after redaction. Each use case
  needs evidence minimization and an explicit external-data decision before use.
- OpenAI currently documents one supported Decisions model, up to 400 questions per
  request, and input-based charging with no output-token charge. Those are vendor
  facts, not Orbit guarantees. Recheck the official guide and pricing at activation;
  neither its speed claim nor projected savings has been validated here.

## Phasing

This records delivered foundations and conditional re-entry requirements, not
an instruction to resume the archived/deferred work.

1. **Evaluation foundation — implemented.** Versioned question sets, evidence
   contracts and offline scoring exist. The development screens above failed;
   independently adjudicated permitted data and a fresh holdout remain owed.
2. **Service and private collector — implemented, default off.** The durable
   bounded service, credentials/data controls and private task-quality collector
   have synthetic public-boundary verification. This repository-only delivery
   supplies no live effectiveness or adoption evidence. The failure consumer is
   archived/deferred rather than awaiting immediate implementation.
3. **Evidence and advisory integration — parked.** Improve rubrics/packets and
   independently adjudicate permitted data, including disagreement and ambiguous
   cases. Split by family and time to prevent near-duplicate leakage and register
   a protocol before opening a fresh holdout. Review per-class precision/recall, abstention coverage,
   false-positive burden, corrections, latency, cost per useful assessment and
   investigator/author time saved against the existing workflow. Choose and record
   per-question thresholds and sample-size requirements before opening a fresh
   holdout. Enable advisory mode only for consumers that meet those declared targets;
   keep measured regressions reversible by switching the consumer off.
4. **Later candidates.** Evaluate the six original remaining candidates independently.
   The [delivery-quality grader](specs/delivery-quality.md) is an additional proposed
   evaluation, not a restart of the archived consumers. It needs an explicit
   evidence/labeling protocol, ordinal-scoring support and its own adoption
   decision. Any automatic application of annotations or descriptive tags needs its own evidence
   and action contract. Keep execution-bearing tags (`ui`, `os:`, `crew-purpose:`,
   approval/review skips and instruction mappings) outside that automatic scope.

## Open questions

1. Which historical failure artifacts and task revisions can supply independently
   labeled examples, without using later outcomes as evidence available at intake?
2. What false-positive burden and minimum useful coverage justify author-visible
   quality flags? Select targets from the baseline before the held-out evaluation.
3. Resolved for the generic service: use an independent additive v72 ledger.
   Automation checkpoints own workflow coverage, review ledgers own reviewer
   authority, and invocation rows cannot atomically claim work or reserve charges.
   The [service contract](specs/service.md) records retention, queries and mixed-version
   compatibility; collectors still own atomic event publication/reconciliation.
4. Should failure judgments assess representative occurrences independently before
   class aggregation when a normalized signature contains several causes? Evaluate
   class purity first; never infer that one sample describes every occurrence.

## Acceptance (feature level)

- With the feature off, task creation and failure-pattern reporting retain their
  existing behavior and make no external requests.
- A created task receives one current assessment for its semantic-input digest;
  duplicate delivery/retry and concurrent edits cannot publish duplicate or stale
  current annotations. Failed or refused assessments are distinguishable from clean
  assessments, and task creation succeeds during provider outages.
- Every failure suggestion identifies its bounded evidence and version, preserves
  occurrence counts and keeps the investigator's disposition separate. Missing or
  mixed evidence can produce insufficient evidence instead of a forced category.
- Advisory results cannot change readiness, pilot warnings, review outcomes, human
  answers, workflow authority or task fields. Later tag application is separately scoped.
- Credential, redaction, budget, timeout, queue and stale-result behavior is verified
  through the affected public boundaries with isolated fixtures; live measurements
  are separately labeled and retain usage, latency and quality evidence.
- Both consumers have a reproducible labeled baseline and fresh held-out readout,
  including class-specific errors, abstentions and costs. Advisory activation names
  the question/policy versions and the previously declared targets it met.

## Task References

- ORB-20000 — propose an independently evaluated advisory delivery-quality grader.
- ORB-19718 — author this design and open its review PR; no implementation authorized.
- ORB-19720 — offline evaluation contract and protocol.
- ORB-19721 — optional generic service and synthetic public-boundary fixtures.
- ORB-19723 — private task-quality shadow consumer and reasoned feedback.
- ORB-19722 — failure consumer, archived/deferred after its development screen.
- ORB-19724 — advisory surfaces, archived/deferred after the development screens.
- ORB-19771 — operator development-study protocols, receipts, readouts and parked disposition.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
