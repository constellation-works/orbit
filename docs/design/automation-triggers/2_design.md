---
title: Automation Triggers — Design
owner: codex
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Draft
feature: automation-triggers
doc_role: design
type: design
summary: Proposed trigger contract for immutable delivery batches, separate coverage checkpoints, preparation freshness, and correlated failure triage.
tags: [automation-triggers, scheduling, coverage, pilot, triage]
paths: ["crates/orbit-core/src/application/routines/**", "crates/orbit-core/src/application/auto_tasks/**", "crates/orbit-store/src/contracts/**", "crates/orbit-types/src/workflow/**", "crates/orbit-core/src/adapter/engine_host/v2_host/**"]
related_features: [routines, auto-tasks, operation-mode, activity-job]
related_artifacts: [ORB-11315, ORB-11314, ORB-11316]
---

# Automation Triggers — Design

**Delivery triggers are implemented in [ORB-11330]; bounded state consumers in [ORB-11331].**
The supported configuration, evidence contract and operational limits are in
[Operations](5_operations.md). Later sections retain the broader design intent;
the schema-v2 YAML in section 8 is illustrative and is not accepted configuration.
Historical ADRs are not inputs to this design.

## 1. Current implementation and gaps

- `orbit-automation` owns the extracted routine and auto-task scheduling rules,
  plus delivery and state-member evaluation and their deterministic coverage validators. Core
  supplies source facts, executor authority, ordinary task creation and job submission.
- Existing cron/interval YAML, cursor semantics, manual mint and host placement
  remain compatible. Legacy cursor I/O is owned by Store. Legacy time-triggered
  defaults remain unchanged; the delivery QA/review defaults are separately disabled.
- Store persists consumer checkpoints, immutable batches/receipts and direct
  delivery intents in the existing host SQLite database. The existing task
  allocation authority and job store persist action-key admission. No new database
  or clock loop is introduced.
- PR membership is provider-verified; Git first-parent ranges order obligations.
  Authorized direct intents become deliveries only after verified branch landing.
  Pending evidence survives partial pages. Task status, markers, epic closures
  and no-diff completion cannot manufacture a delivery.
- The existing artifact tool records trusted executor origin in a Store-authored
  artifact. The owner checks assignment, exact frozen input/revisions, completeness
  and provenance before accepting an immutable receipt. Job-only consumers use
  persisted step results. QA and review remain independent.
- State-member pilot freshness and bounded causal triage use the same Store
  checkpoint path [ORB-11331]. Broader multi-member coordination, review exclusion certificates,
  policy-driven waiver/migration workflows and usage accounting below remain
  separately owned proposals. No such certificate currently excludes a delivery;
  missing usage is unknown.

## 2. Shared contract and ownership

A definition has one typed trigger and one action. Supported candidate variants:

| Trigger | Due work | Completion obligation |
| --- | --- | --- |
| `cron` / `interval` | One UTC slot identity; missed slots collapse or skip according to explicit policy. | Accepted action outcome; legacy definitions keep legacy semantics. |
| `deliveries_landed` | Oldest uncovered verified deliveries reach `threshold`, or nonempty pending work reaches `max_wait_minutes`. | Validated coverage of the captured delivery/range obligations. |
| `preparation_eligible` | Proposed/backlog task has no fresh assessment for its material fingerprint. | Per-task accepted assessment, including an unready verdict. |
| `execution_failed` | A settled failure incident crosses into retry-exhausted, diagnosis-eligible state. | Accepted incident disposition or explicit escalation. |

The last two are the v1 state-transition/eligibility vocabulary. They are not
arbitrary `on status = ...` subscriptions: a human block or routine metadata
change must not invoke a model. Evaluation returns `not_due`, `pending`, `due`,
`deferred`, or `invalid`, with stable reason codes and source evidence. It cannot
promote, complete, or diagnose tasks itself.

The internal `orbit-automation` crate owns shared evaluation, discovery and
scheduling policy [ORB-11330]. Core composes explicit source descriptions,
authoritative evidence and narrow task/job lifecycle adapters. Routines retain
job submission; auto-tasks retain the single template-to-task creation boundary.
Their existing sweep entrypoints invoke Automation for their own consumers;
neither additionally dispatches the other's consumers. No second ticking loop.
Store contracts/drivers own cursor I/O, atomic claims and durable checkpoints;
workflow types own serializable contracts; Engine retains execution. Cmd retains
registry/host composition and clock installation. The enforced direction is
Core -> Automation -> Store/Common/Types, with no reverse Core/Engine edge.
The extraction preserves legacy time-trigger semantics; delivery-trigger and
coverage rules now have one implementation; the broader non-delivery contracts below remain proposals.

Consumer identity is `(authority machine, workspace ID, definition kind, name)`.
An immutable definition epoch binds trigger semantics, target/template, and
coverage contract. QA and review have separate consumers and coverage, even when
their delivery inputs overlap. V1 state consumers have one authoritative host;
reject ambiguous multi-host placement rather than imply distributed exactly-once.
Existing multi-pin time routines continue their independent per-host behavior.

Each pass captures `now`, effective definition revision, source head and scan
ceiling. Proposed initial limits: 200 source records per workspace/pass, 20 remote
evidence lookups, 50 batch members, one active batch per consumer, and a 30-second
evaluation deadline. A manifest is also capped at 1 MiB and 5,000 commit IDs;
oversized indivisible deliveries defer with an explicit reason until an operator
selects a supported larger-work path. Limits never silently shorten coverage.
Persist continuations and rotate consumers/sources for
fairness. Existing lower job/provider caps still win. Cap pending materialization
at 1,000 items/consumer; on saturation, stop advancing the source continuation,
retain the durable source range and report backpressure. Do not silently truncate
or repeatedly list all tasks and merely cap the resulting output.

Use repository-owned pagination over stable IDs and bounded reconciliation laps
for existing task/run state. Persist each lap's upper ID and continuation; new
IDs enter the next lap, and updates to already-scanned IDs are caught next lap.
Capture each entity revision under its store boundary. Thus eligibility may lag
one lap but a mutable timestamp cannot skip it permanently. Failure episodes
come from retained run/history records, not only today's task status. An action
rechecks current eligibility before any write. Optional dirty-ID hints accelerate
this path; their loss must not affect correctness.

## 3. Delivery identity and captured coverage

A delivery is a verified landing into the configured repository and branch, with
nonempty resulting content change. For PR delivery the canonical key is
`(repository identity, target ref, provider PR identity)`; store merge/base/head
commits, trees, landed time, evidence source and all included task IDs as evidence.
Provider identity must include the repository, not only a numeric PR number.
For an authorized direct landing, the delivery owner records a stable receipt
with before/after revisions and member commits/tasks. Its receipt ID is the key.
Do not synthesize direct deliveries from every task-marked commit.

| Case | Threshold treatment |
| --- | --- |
| One PR closes five tasks / epic parent and children | One delivery; all task IDs are provenance. An epic closing alone contributes zero. |
| Epic children land through three separate PRs | Three deliveries; no additional count for the parent. |
| Squash, rebase, merge commits, wrapper retries | Resolve to the same PR/receipt key; member commit count does not multiply it. |
| PR open, branch pushed, task set done, no evidence | Zero verified deliveries; retain unresolved evidence and explain why. |
| Side-effect-only/no-diff completion, completed QA/review | Zero code deliveries. A real code change carrying a no-diff tag still needs actual evidence and is counted. |
| Revert or repair lands through a distinct PR/receipt | A new delivery, even if it restores old content. |
| External/manual PR lands outside Orbit | Count once when verified on the configured branch. Missing task IDs do not disqualify code. |

The normal source is a bounded walk from the recorded integration head to a
newly pinned head, supplemented by delivery owner/provider evidence. First-parent
ordering supplies stable coverage ranges, not delivery grouping: a rebased PR
can span several first-parent commits. Record all commits in `(from, through]`,
including unattributed changes and covered neighbors needed for context. Pending
provider lookups have stable identities and remain revisitable after the main
scan moves. Provider failure is `evidence_unavailable`, never a zero-change
success. If a provider cannot prove a rebase span, leave grouping unresolved.

Only verified, consumer-eligible, uncovered delivery keys count toward its
threshold. Coverage range can be broader than that count: all unexamined changes
between the prior accepted boundary and the captured upper revision are disclosed
as obligations, including unattributed commits. Late evidence for content at or
before the explicit baseline retains its baseline exclusion. Later content already
validly examined attaches to that coverage and does not trigger a redundant
batch; otherwise it enters pending work. Force-push/non-ancestral history pauses
range advancement with `history_diverged`; an operator chooses a documented
reset/replay. Never silently replace the baseline with current HEAD.

Batch input contains batch/consumer/epoch IDs, ordered delivery IDs and evidence
digests, exact `from_exclusive`/`through_inclusive` revisions and trees, full
commit membership, required examination class, exclusion certificates, effective
policy/grant reference, and bounded resource settings. It is captured before
mint/submission. Workers inspect that pinned revision, not whatever HEAD is when
they start. Retain source objects and manifests until obligations settle and the
audit retention window ends; missing objects make coverage unverifiable.

Operation-mode's [review contract](../operation-mode/3_vision.md#313-content-specific-coverage-through-delivery)
from [ORB-11316] determines whether before-PR coverage maps to actual landing
content. Trigger code consumes that validated result; it does not infer review
from a tag, patch ID, timestamp, or task status. Exclude proven patch coverage
from redundant review counts, retain neighboring context, and keep QA independent.
For six deliveries with four valid review exclusions, review threshold three sees
two; the next uncovered delivery makes it due. QA can count all seven.
[ORB-11333] implements this: certificates are produced by the before-PR gate
and consumed as `excluded` state and batch `exclusions`; see
[Operations](./5_operations.md#before-pr-coverage-exclusions-orb-11333).

## 4. Observation, dispatch, and successful coverage

Three separate checkpoints are essential:

- **Observed (`O`)**: source continuation plus evidence durably classified or
  retained as pending/unresolved. Advancing `O` never means successful work.
- **Dispatched (`D`)**: immutable batch has an acknowledged job run or minted task
  identity. This measures action creation, not the executor's success.
- **Covered (`C`)**: the highest contiguous boundary that is no longer a
  scheduling obligation — accepted examination receipts, or a proven
  before-PR-excluded prefix that advanced without a consumer receipt. Never
  jump over a coverage hole. Excluded-only progress is not an examination
  receipt and does not count toward a review threshold.

Use member sets, not one timestamp, for pilot tasks and incidents. For deliveries,
range boundaries are Git revisions while observation continuations are source
positions; do not compare them numerically. `D` is a batch record, not a universal
high-water mark. Baseline-excluded history is recorded as such, never labelled
successfully covered.

Observations accumulate while overlap, `skip_if_open`, capacity, missing grants,
or debounce prevent dispatch. Pending membership is never appended to an active
batch. At threshold freeze the oldest pending range, bounded by `max_items`;
the threshold must be positive and at most `max_items`. Later arrivals belong to
the next batch. A nonzero maximum wait optionally flushes a subthreshold batch;
it starts at the oldest pending observation and never creates empty work.

For an auto-task, acknowledgement attaches the batch to the minted task. It may
wait indefinitely for normal approval/admission; do not recreate it merely because
it has not run. `skip_if_open` still includes manually minted open instances of
the same definition. Manual mint stays unconditional and cursor-neutral and
gets no automatic batch claim; it cannot consume scheduled coverage implicitly.

Successful process exit, task `done`, summary prose, or an empty findings list is
not a coverage receipt. Core validates the expected batch identity, exact input
digest/revisions, required work evidence and writer authority before accepting
coverage. Review with verified findings can count as examined while finding tasks
remain open; QA counts exercised obligations, not a promise that defects are fixed.
Unavailable validation is incomplete, with skipped obligations exposed. For v1
code batches accept all required range obligations together; partial progress is
saved as evidence but retries retain the full range. Pilot and triage can accept
independent per-member results, retrying only unresolved members.

Delivery retries are opt-in (default zero), with five-minute backoff and a frozen
24-hour automatic-retry deadline. The captured attempt budget survives restarts;
routine retry limits can reduce it. Ordinary job admission still applies its
existing capacity and grant limits. Broader configurable aggregate deadlines
remain future work.

Failure retains the batch and coverage gap. Retry the same immutable input after
applicable execution recovery settles, within one durable batch budget. An open
blocked task is the same action, not permission to mint a replacement. If a closed
failed action needs replacement, record a new attempt under the same batch only
after its predecessor is known stopped and policy authorizes retry. Exhaustion
leaves `needs_attention`, not covered. A withdrawn/rejected task suppresses further
automatic attempts; an operator must choose retry, replacement, or a reasoned
waiver. Waivers settle scheduling debt separately from successful coverage and
remain visible gaps in coverage telemetry.

## 5. Crash safety, concurrency, and minimal state

Proposed durable records, host-local and store-owned:

| Record | Minimum fields |
| --- | --- |
| Consumer cursor | Identity, epoch/config digest, baseline, source continuations, accepted range boundary, revision, debounce/next retry timestamps. |
| Work membership | Source key/revision, pending/claimed/covered/waived state, evidence reference, first-seen time, exclusion/rejection reason. |
| Batch/attempt | Immutable input digest/manifest, member keys, action key, task/run linkage, attempt counter, claim generation, outcome and retry budget. |
| Accepted receipt | Batch/member identity, input/contract digest, accepted evidence, accepting actor/time; per-task fingerprint or incident disposition where applicable. |

Extend the existing host SQLite scheduler store with namespaced records rather
than another database or user-edited state file. Task/run bundles retain action
input/results and a stable creation key; scheduler records hold coordination and
references. Forward-only store migrations and a recoverable cross-store protocol
are required: task bundles and scheduler SQLite are not one current transaction.

The Automation boundary claims pending members through Store and writes batch intent with compare-
and-swap plus unique keys. It releases database locks before external I/O. A
routine's job submit and the common task creation path must accept a durable
action key `(consumer, epoch, batch, attempt)` and return the previously created
identity on replay. This is a prerequisite change, not a guarantee supplied by
today's file lock, provenance tag, or routine fire intent.

Creation must reserve and persist key-to-ID mapping before launch, or recover
that mapping from an atomically written canonical bundle. Store operations must
define the recovery order for a crash at every file/SQLite boundary. Merely
searching recent task titles after a crash is not idempotency. Poll outcomes by
the returned identity; receipt acceptance and membership/C advancement share a
store transaction and can replay safely.

| Failure window | Required behavior |
| --- | --- |
| Before observation commit | Replay source page; source-key uniqueness removes duplicates. |
| Intent saved, action not created | Recover/resubmit the same action key. |
| Action created, acknowledgement lost | Resolve key to original task/run; never mint or launch a second one. |
| Outcome persisted, coverage not accepted | Revalidate and accept the same receipt once. |
| Evaluator dies or two evaluators race | Store claim generation fences stale writers; uniqueness and action-key admission fence side effects. The sweep lock is an optimization, not the only guarantee. |
| Lease expires / worker liveness unknown | Reconcile identity and owner. A lease timeout alone cannot authorize duplicate execution. Keep ambiguous work held for investigation. |
| Pause, disable, or authorization expiry | Stop new admissions, preserve pending/input, reconcile admitted outcomes. Ordinary expiry does not cancel admitted work; explicit cancellation remains separate. |

A generation token must be checked at action creation as well as receipt writes;
otherwise an old evaluator can submit after losing its lease. Existing routine
timeout reclamation cannot be copied into this protocol as proof of worker death.
This contract promises idempotent action admission and receipt application, not
exactly-once arbitrary worker side effects. Finding creation still requires its
own existing symptom/evidence dedupe. Retention must preserve action-key tombstones
long enough to reject replay; missing state is a diagnostic, not a fresh baseline.

## 6. Pilot eligibility and freshness

Observe proposed/backlog tasks authorized for preparation. No-diff tasks retain
the current exclusion unless an explicitly supported preparation contract needs
them. Missing selectors are one reason to prepare, not the general definition of
staleness. Tasks in progress, review, terminal, withdrawn, or human-blocked are
not automatic pilot candidates. Preparing proposed work does not approve it.

A material fingerprint covers normalized title, description, acceptance criteria,
plan, selectors, type, complexity/crew and required tools, behavior-relevant tags,
dependency identities and delivery state, applicable repository instructions,
pilot contract version, and pinned integration revision. V1 conservatively treats
any source revision change as stale; selective path invalidation needs separate
evidence and is deferred. Resolve dependency evidence, not raw done statuses.
Priority-only edits reorder candidates; comments, summaries, timestamps, run
linkage and instrumentation do not invalidate. Proposed/backlog share an
eligibility class so authorized promotion alone does not cause a pilot loop.

Use proposed defaults of a two-minute quiet period, ten-minute maximum wait and
50 tasks per batch, preserving partitions of at most five. Coalesce repeated
changes to a pending task into its newest fingerprint. One in-flight assessment
per task/fingerprint; changes during execution remain pending and do not mutate
the captured snapshot. Stable ordering by oldest pending time then task ID
prevents repeatedly edited work from starving other tasks.

The prepare/apply domain boundary recomputes fingerprints and eligibility under
task locks. Accepted selectors/assessment fields written by that same apply are
part of the certified *post-apply* fingerprint, with the pre-apply fingerprint
retained for audit. They do not invalidate their own assessment. External edits
still do. A blocked-by-decision, duplicate, or low-utility verdict is a fresh
assessment with `ready=false`; do not rerun it every clock tick. Invalid or failed
assessments retry within budget, then wait for material change or explicit reset.
If a pinned source became stale, retain its evidence but do not certify current
readiness. A continuously moving base may require escalation rather than churn.

The routine passes explicit batch task IDs and expected fingerprints to the
existing pilot job, which today only accepts IDs/source preparation inputs and
needs a small contract extension. A trigger never bypasses domain eligibility.
Mode-driven promotion must consume a fresh accepted readiness record and its
separate grant; populated selectors or a successful wrapper are insufficient.

## 7. Triage incidents, cancellation, and recursion

Observe the transition into **settled execution failure after applicable retry
exhaustion**, not every failed step. Engine retry/recovery and authorized resumes
remain the owners of recovery. A durable episode records attempts consumed,
remaining budgets, scheduled retry, active descendants, and whether the failure
is settled. Triage defers while any applicable retry or causally related child is
active. Do not wait for the entire unrelated workspace coordinator to finish.

Incident identity is `(workspace, execution episode, causal failing step/child)`.
A wrapper failure explicitly caused by that child shares its incident; unrelated
sibling failures remain separate. Run IDs, persisted child dispatches and error
references establish causality; close timestamps or similar messages do not.
Resumes preserve episode identity; a new authorized execution after a diagnosis
starts a new episode, while per-task re-backlog budgets survive both. Missing
lineage yields `incident_unresolved`, not guessed duplicate suppression.

Filter intentional operator cancellation, withdrawal, admissions stop, known
supersession, and cancellation cascaded solely from those causes. Admissions stop
is not itself a run failure. An unrelated child failure that preceded an operator
stop remains diagnosable only if current task intent permits it. Current generic
`cancelled` state is insufficient: require typed cancellation cause/provenance;
unknown cancellations are held for inspection, never auto-rebacklogged.
Interrupted/dead-owner runs take existing deterministic reconciliation/resume
first; a proven exhausted execution failure can later create an incident.

Candidate tasks must still be blocked by that exact episode with no later human
block/withdrawal. Check the failure history event and current coupling, not just
the existence of `job_run_id`. For a bundle diagnose the cause once, then apply
per-task dispositions after rechecking each task's status/revision/coupling and
durable re-backlog budget. A failure before task admission can receive a run-level
diagnosis/escalation; it cannot authorize task lifecycle writes.

Keep deterministic recovery (stopped-owner reconciliation, recorded retry state,
verified stale reservations) before model diagnosis. Preserve the current narrow
evidence-gated already-landed reconciliation; triggers grant no new done/merge
rights. Model diagnosis returns environmental/task/code/unknown findings; only
the existing authorized deterministic boundary may re-backlog environmental
failures within budget. Unknown or unresolved product intent remains blocked
for human/Astra judgment. Moving already-landed work to backlog is prohibited.

Every diagnostic action carries `origin=triage` and a root incident reference;
propagate that ancestry into children and tasks it creates. Exclude triage's own
execution failures and automation-generated diagnostic descendants from automatic
triage. Retry the original diagnostic batch within a small separate budget, then
record one escalation on the original incident. Do not spawn a triage of triage.
Ordinary approved implementation of a resulting repair is a new execution episode
and can be triaged normally; ancestry suppression is scoped to diagnostic work,
not a permanent ban on every descendant task.

## 8. Proposed YAML examples

**Illustrative schema candidates, not loadable definitions.** `schemaVersion: 2`
below proposes a new routine/auto-task schema; it does not refer to Job schema v2.
Replace legacy schedule/trigger only through explicit migration. Unknown keys,
zero thresholds, invalid budgets, unsupported trigger/target combinations, and
both `schedule` and `trigger` are rejected. Provenance fields are omitted here.

```yaml
# Proposed auto-task: QA after five actual deliveries, or one-hour pending age.
schemaVersion: 2
name: qa-sweep
description: Validate a captured integration range hands-on
enabled: false
trigger:
  kind: deliveries_landed
  branch: agent-main
  threshold: 5
  max_wait_minutes: 60
  coverage: integrated_qa_v1
batch:
  max_items: 25
  max_active: 1
dedupe: skip_if_open
template:
  title: Perform QA validation
  description: Exercise the supplied immutable batch and persist evidence and findings.
  acceptance_criteria: [Exercise affected user paths, Record coverage and verified findings]
  task_type: chore
  tags: [qa-sweep, no-diff-expected]
  crew: system
  status: backlog
```

```yaml
# Proposed auto-task: review counts only uncovered eligible deliveries.
schemaVersion: 2
name: code-review
description: Review a captured landed range
enabled: false
trigger:
  kind: deliveries_landed
  branch: agent-main
  threshold: 3
  max_wait_minutes: 360
  coverage: landed_code_review_v1
  exclude_valid_coverage: before_pr_code_review_v1
batch:
  max_items: 20
  max_active: 1
dedupe: skip_if_open
template:
  title: Review recently merged changes
  description: Review supplied obligations and context; persist coverage and confirmed findings.
  acceptance_criteria: [Examine captured obligations, Record range and verified findings]
  task_type: chore
  tags: [code-review, no-diff-expected]
  crew: system
  status: backlog
```

```yaml
# Proposed routine: preparation does not promote or dispatch the assessed tasks.
schemaVersion: 2
name: task_pilot
enabled: false
target: job:task_pilot_pipeline
trigger:
  kind: preparation_eligible
  statuses: [proposed, backlog]
  freshness: material_v1
  debounce_minutes: 2
  max_wait_minutes: 10
batch:
  max_items: 50
  max_active: 1
policy:
  overlap: forbid
  timeout_minutes: 90
  retries: {max: 1, backoff_minutes: 5}
```

Preparation eligibility includes both selector-free tasks and tasks whose
persisted complexity remains `unassessed`. Task-pilot records the bounded
repair's certainty, behavioral change, coupling, validation difficulty,
rationale, confidence, evidence gaps, validation approach, and reassessment
triggers. Its apply step commits concrete selectors, complexity, audit evidence,
and the idempotency receipt at one task-bundle boundary. Automatic admission
then rejects any still-unassessed task, including urgent security work, except
one tagged exactly `no-diff-expected`, which is admitted without an assessment
because an implementation lane sizes no diff for it [ORB-12118]; missing
validation permission is a readiness blocker rather than a complexity or
priority inference. The low/medium/hard examples in the activity contract are
deterministic policy fixtures, not a claim about live-model accuracy.

```yaml
# Proposed routine: one diagnosis per settled causal incident.
schemaVersion: 2
name: task_triage
enabled: false
target: job:task_triage_pipeline
trigger:
  kind: execution_failed
  after: retry_exhaustion
  settle_minutes: 2
batch:
  max_items: 20
  max_active: 1
policy:
  overlap: forbid
  timeout_minutes: 30
  retries: {max: 1, backoff_minutes: 5}
```

Triage cancellation/recursion filters are mandatory domain rules, not disableable
YAML flags. `settle_minutes` is a coalescing delay, never proof of exhaustion.
For both routine examples Core supplies a reserved `trigger_batch` input plus
explicit `task_ids`/incident members; templates or arbitrary run input cannot
override the server-issued batch. Auto-task creation attaches the same manifest
as task input evidence without invoking a job. These input paths need implementation.
Time alternatives are `trigger: {kind: cron, cron: "0 6 * * *", missed_run: catch_up_once}`
or `trigger: {kind: interval, every_minutes: 360}`. UTC slot identity and existing
host-local cron/DST behavior remain; intervals retain a fixed baseline and do not
drift from task completion time. No OR/AND trigger language is proposed.

## 9. Example timelines

| Sequence | Durable result |
| --- | --- |
| Enable QA at H0; D1, D2, D3 land, threshold three | Baseline is H0; observation retains three keys; freeze B1 through H3 and mint T1. C remains unset beyond baseline. |
| D4 and D5 land while T1 waits/runs | O advances with durable pending members; B1 remains D1–D3. No second task while T1 is open. |
| T1 fails, then an authorized attempt succeeds on B1 | Failed attempt advances no coverage. Accepted receipt advances C to H3 once; D4/D5 still pending. |
| D6 arrives / max pending age expires | Freeze B2 for D4–D6 / D4–D5 respectively; never silently include them in B1's receipt. |
| Crash after T1 creation before scheduler acknowledgement | Action-key lookup recovers T1. A second evaluator cannot mint T2 for B1. |
| Pilot captures task fingerprint F1, user changes criteria to F2 | F1 apply is stale; F2 stays pending. Successful F2 selector write certifies its post-apply fingerprint and does not loop. |
| Child fails, wrapper propagates failure, retry remains | One unsettled incident, no diagnosis. Final retry exhaustion settles it; one triage batch includes affected tasks. |
| Operator intentionally cancels instead / triage itself fails | Intentional cancellation is filtered; triage failure retries or escalates the original incident without recursive diagnosis. |

## 10. Mode, compatibility, and migration

[Operation mode](../operation-mode/3_vision.md), proposed in [ORB-11314], supplies
cadence, thresholds, batch/resource defaults and scoped grants. Resolve mode
defaults through its global → workspace → run rules, then explicit definition
values override operational defaults. An explicit operator invocation may provide
an audited override for that invocation, subject to the same hard ceilings. Never
let an unrelated run's override retune global scheduled consumers. Record each
effective value's source, definition hash and policy revision at batch admission.

Authority is an intersection, not a numeric precedence ladder: definition enabled,
host ownership, local pause, applicable grant/window, job availability, capacity,
task approval and repository rules all constrain action. Neither a lower threshold
nor an autonomous preset creates permission to promote/commit/merge. Current
`--complete` is default-off; triggers do not alter it. Orbit task PRs target
`agent-main`; this task's PR is left unmerged, with no auto-merge or scheduled
agent review. A finding task is not authorization to implement it. Routine
enablement remains deliberate and worker tool/sandbox/crew restrictions survive.

Preserve schema v1 bytes and behavior, including user-edited schedules, disabled
defaults, cron timezone/missed-run policies, intervals, manual mint and existing
dedupe. Use managed-asset provenance and an explicit preview to convert selected
definitions; never overwrite edits or automatically enable new triggers. Legacy
code-review summary cursors require validated ancestor/range evidence for import,
not automatic trust in the newest done task. Show old/new owners to prevent a
legacy cron review and migrated delivery review from both running unintentionally.

Cold-start defaults differ by source: time and deliveries record a current
baseline without replaying old slots/landings; eligibility performs one bounded
inventory of current eligible tasks; failures inventory currently unresolved,
retry-exhausted episodes subject to coupling and cancellation checks. These are
explicit initialization choices. Historical replay requires an operator-selected
range and budget; baseline exclusions are never successful coverage.

Threshold/debounce/cap edits retain pending work and apply to future batches;
admitted batches keep captured settings. Trigger kind, target/template, branch,
coverage contract or source identity changes create a new epoch. Reconcile old
actions, then explicitly carry compatible pending obligations or record a waiver/
reset; never drop them because the name is unchanged. Disabling preserves debt.
Renaming/moving hosts requires explicit identity transfer after old work is fenced;
v1 offers no automatic cross-host state migration. Missing/corrupt new-state data
fails closed. Rollback disables new admissions and preserves receipts/legacy state;
older binaries must not reinterpret schema v2 as time-only work.

## 11. Implementation phases and validation

1. Add typed dry-run evaluation, explicit bounded source scans and verified
   delivery/incident evidence adapters. Show due/deferred/unknown reasons without
   minting or invoking. Establish reference fixtures for the contract below.
2. Extend store-owned batch/receipt records and common action-key admission;
   fault-inject every file/SQLite/dispatch boundary. Add immutable routine input
   and auto-task batch attachment. Migrate no definitions automatically.
3. Opt in QA/review delivery consumers with baseline preview, pinned inputs and
   validated coverage acknowledgement. Test failure retries before enabling them.
4. Extend pilot fingerprints and per-member apply, then triage episode/retry/
   cancellation provenance and recursion guards at their existing domain owners.
   Switch their time routines only once semantic checks pass. Optional mode
   defaults and event wakeups can follow independently.

| Validation scenario | Required assertion |
| --- | --- |
| Cron/interval startup, sleep gaps, DST and time rollback | Legacy slot identities/baselines remain stable; no work before baseline. |
| Bundle/epic/no-diff/rebase/squash/revert/manual delivery | Count canonical real landing units exactly once; unresolved evidence stays visible. |
| Arrivals during backlog/running/failed sweeps; delayed provider evidence | Immutable input, preserved pending debt, no false coverage or lost late member. |
| Two evaluators and crashes at each durable boundary | One action per action key, no stale-generation submit, idempotent receipt application. |
| Partial QA/review versus partial pilot results | No partial range advancement; independent valid task results survive wrapper failure. |
| Task/source/contract edits, own apply writes, decision blockers | Correct freshness invalidation, debounce, no self-loop or repeated unready diagnosis. |
| Retry chain, child/wrapper propagation, human cancellation/block, triage failure | One causal incident, exhausted retries only, preserved human intent, no recursive triage. |
| Backpressure, source retention gap, malformed state, host move, edit/rollback | Bounded I/O, fair eventual reconciliation or explicit gap; no silent reset/debt loss. |
| Denied authority, expired window, manual mint, independently enabled legacy definition | Existing controls win; no implicit promotion/completion or duplicated automatic owner. |

Implement tests at the shared domain/store boundaries using existing sibling
test layouts and fake clocks/providers; add no generic harness. Include end-to-end
fixtures exercising actual job/task creation and crash recovery, not only due math.
Required repository gates remain `make ci-fast` and `make ci-lint`; full `make ci`
is the PR merge gate. Documentation validation checks metadata, source/relative
links, index generation, YAML consistency and timeline invariants.

Extend existing routine/auto-task/task/run/audit projections with effective trigger
and policy provenance, baseline, O/D/C, pending count/oldest age, threshold and
exclusion counts, batch/input/revision links, retry budget, coverage gaps, incident
lineage and stable deferral reasons. Distinguish evaluator health from consumer
health; report clock lag, scan continuation/backpressure, unavailable evidence,
unknown worker liveness, and skipped validation. Do not claim implemented CLI/API
fields here. Coverage summaries must distinguish examined, excluded, baseline,
waived and unknown content, with a denominator and observation window.

Compare opt-in cohorts against time-triggered baselines: mechanical orchestrator
turns per accepted change, total pilot/review/QA/triage consumption including
failed attempts, idle fires, coverage latency and repeated-work rate. Track
regressions/reverts, useful findings, human/Astra interventions and unresolved
coverage debt alongside cost. Missing usage is unknown, not zero. Graduation
requires fewer mechanical actions without hiding skipped work or degrading
quality; exact thresholds are an open product decision.

## 12. Concerns & Honest Limitations

Delivery grouping for rebased/manual work, typed cancellation causes, comprehensive
pilot fingerprints, and recoverable cross-store action creation are new work.
They are prerequisites to the claimed semantics, not features inferred from
existing task/run history. Conservative source invalidation may repeatedly stale
pilots on a busy branch; retained evidence has storage cost. Bounded reconciliation
adds clock/lap latency and requires source retention. A missing provider or
ambiguous orphan can stop a consumer until someone resolves it. This is preferable
to duplicate execution or manufactured coverage, but must be operationally visible.
This proposal does not establish that any live host definition is enabled or safe
to change; implementation scope and defaults still require approval.

## Task References

- [ORB-11315] — specifies shared triggers, batch/coverage and pilot/triage semantics.
- [ORB-11314] — proposes operation-mode defaults and authorization snapshots.
- [ORB-11316] — proposes review timing and content-specific coverage exclusions.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
