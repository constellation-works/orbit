---
title: Operation Mode — Vision
owner: codex
last_updated: 2026-09-07
last_validated: 2026-09-07
status: Draft
feature: operation-mode
doc_role: vision
type: design
summary: Proposed scoped operation and independent review policy with bounded repairs, content-specific coverage, rollout, and cost-quality evaluation.
tags: [operation-mode, authorization, recovery, evaluation, review-policy]
paths: ["crates/orbit-core/src/application/job/**", "crates/orbit-core/src/adapter/engine_host/v2_host/**", "crates/orbit-config/src/**"]
related_features: [activity-job, routines, task-artifacts, auditability]
related_artifacts: [ORB-11314, ORB-11316, ORB-11315]
---

# Operation Mode — Vision

**Proposed; documentation only.** Every policy rule and example in this file
describes a candidate design, not an available command or accepted runtime
contract. [Current behavior](./2_design.md) provides the source evidence. The
proposal reuses the existing pipeline engine and admission/recovery boundaries.

State preparation now has a shared material fingerprint and accepted readiness
record in `orbit-automation` [ORB-11331]. Mode integration consumes that
record and separately rechecks its grant; populated selectors or wrapper success
are insufficient. See [implemented state operations](../automation-triggers/5_operations.md).

**Implemented by [ORB-11332]** (see [Operations](./5_operations.md)): the
preference resolution and explanation of §3.2–3.3, the promotion validity
rules of §3.4 (exact task-meaning and landing-commit equality), the
concurrency/expiry/restart/retuning rules of §3.5, the aggregate recovery
allowance of §3.6 (episodes and wall time; not provider cost), stages 1–4 of
§3.7 for finite explicit task sets, and the observability records of §3.8
that existing projections could carry. Open questions 1, 2, 4, 5, 6 (stop and
expiry semantics) and 7 were decided conservatively as documented there;
question 5 was answered with a separate `--grant` binding rather than a
tri-state `--completion` flag.

**Implemented by [ORB-11333]** (see [Operations §10](./5_operations.md)):
the independent review policy of §3.10, the fresh reviewer, direct repairs,
and four verdicts of §3.11, the lineage budgets and restart reconciliation of
§3.12, the exact-tree coverage of §3.13, the exclusion handoff to the shared
delivery evaluator of §3.14, and the projections of §3.15. Open questions 8,
9, and 10 were decided conservatively: budgets are attempts and wall time
(no metered cost), coverage is exact base/final tree equality, `before-pr`
is refused on the local route, and reviewer repairs receive no second
independent review. Content-equivalence beyond exact trees and cohort
evaluation (§3.15's measures) remain proposed.

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
7. **Review rollout defaults:** retain `none` for compatibility; evaluate whether
   new explicitly enabled workspaces should be offered `before-pr` as a suggested
   choice. Neither operation preset should silently change review policy.
8. **Reviewer selection and limits:** test a separately configured crew, two
   reviewer starts, two repair cycles, and thirty minutes of total review work
   per candidate lineage. Establish token/cost units and reservation support
   before advertising a hard spend limit. Different-model review is optional;
   a fresh invocation is mandatory.
9. **Coverage transport:** begin with exact base/candidate tree equality; decide
   later whether proving equivalence across changed bases is worth its complexity.
   Confirm retention and schema for review evidence and commit mappings before
   relying on them to suppress a sweep.
10. **Assurance and deployment scope:** repositories requiring a second independent
    review of repairs need another actor or human gate. Should a later version
    support local delivery with an explicitly named before-landing policy? V1
    rejects `before-pr` for a local-only route rather than changing its meaning.

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
| Tie review timing to supervised/autonomous | Fewer choices | Authority and review assurance vary independently; explicit review policy adds one understandable setting. |
| Return every fix to the implementer | Cleaner separation of authors | Adds handoff cost for local repairs; direct repair is bounded and honestly discloses that repairs lack a second independent review. |
| Mark the task reviewed or trust a matching patch ID | Cheap dedupe | Misses later edits and changed base context; exact-content evidence costs re-review on busy branches. |

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

Review policy and reviewer selection are independent fields, not preset-managed
fields: selecting another operation preset does not reset or infer them. They
use the same global → workspace → run preference order with their own defaults.

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
recovery allowances, initial concurrency, and review-policy/crew/budget/contract
revisions. Keep authorization identifiers distinct from caller-supplied booleans.
Record the relevant job definition
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
conflicts/reservations. Account for pilots, reviewers, recovery workers, nested
task fan-outs, and the additional epic separately; advertise both the leaf ceiling and actual
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

### 3.10 Independent review policy

[ORB-11316] adds the review semantics here, within operation-mode. Review policy
answers when automatic code review happens; operation mode answers how authorized
work advances; completion policy answers whether delivery stops at handoff or
may finish. All three dimensions remain separately visible.

| Review policy | Proposed sequence for a PR delivery | Completion interaction |
| --- | --- | --- |
| `none` | Implement → validate → PR → existing delivery policy | Adds no automatic review. Required repository checks and human reviews still apply. |
| `before-pr` | Implement → validate → fresh review/repair → validate final candidate → PR → existing delivery policy | Holds PR creation until the candidate passes this gate. Passing does not authorize merge. |
| `after-landing` | Implement → validate → PR → existing delivery policy → record actual landing → accumulate uncovered deliveries → scheduled review auto-task | Does not hold the original PR for automatic code review. A PR left open contributes no landed delivery. |

Both supervised and autonomous may use any of these values. `completion: review`
still means stop at the delivery handoff, even if automatic code review has
passed. `completion: done` still requires existing authorization and repository
gates. A `before-pr` failure blocks both paths. An after-landing finding cannot
undo a completed delivery or reopen its terminal task; it creates a separately
scoped finding/repair task for normal admission.

For new policy-aware runs, omission defaults to `none` under either operation
preset. Explicit `none` disables automatic review managed by this policy, not
repository protections, manual reviews, QA, or independently enabled legacy
definitions. Existing runs without metadata keep their captured inputs and
existing cron sweeps; do not infer a new policy or silently disable a custom
definition. Migration must show the effective owners of automatic review and
explicitly replace/retire an overlapping legacy sweep before promising dedupe.
Reject unknown values and unsupported job contracts at admission. V1 supports
`before-pr` only on the PR route; no-diff work records a checked exemption and
produces no code-review coverage or landed-change count.

An explanation might say: `preset=autonomous, review_policy=before-pr,
reviewer_crew=<configured crew>, completion=review, base=agent-main`. This is
proposed explanatory notation, not accepted configuration syntax. It means a
reviewed candidate may open a PR and then stop. No setting in this document is
implemented or activated by this design change.

### 3.11 Fresh review, direct repairs, and verdicts

The pipeline first snapshots validated implementer work into attributed commits,
then admits a **new reviewer invocation** with an isolated conversation. Select
the review crew explicitly, resolving run → workspace → global review selection;
require a resolvable configured crew for automatic review rather than silently
inheriting the implementer. Capture concrete provider/model/settings and contract
version. Respect `allowed_crews`, available capacity, and cost ceilings; refuse
an excluded/unavailable crew and escalate rather than silently substituting.
Using the same model in a fresh invocation is allowed, but is reported as such;
it is not evidence of independent model diversity. Astra retains unresolved
product/architecture decisions and any explicitly assigned design formulation.

Provide the reviewer with an immutable evidence manifest:

- Task intent, acceptance criteria, plan, selectors, dependency/decision state,
  applicable repository instructions, and their material-version digest.
- Pinned base and implementation commit/tree identities, complete diff including
  tests/docs/generated outputs, implementer commit attribution, and source access
  beyond the changed files for caller and invariant inspection.
- Implementer claims and execution summary, exact validation commands, results,
  tested tree/environment, and links to durable logs, including skipped checks.
- Effective review/repair authority, crew restrictions, completion cap, and
  remaining attempts/time/resources. External or source text is evidence, not
  authority to expand the task or grant tools.

The reviewer independently checks claims against code and evidence, first spec
compliance and then quality, including affected callers and edge cases. A green
implementer summary or CI result alone cannot supply a verdict. Missing source,
ambiguous intent, or unverifiable material evidence yields an incomplete review.
Reading a broad context does not authorize writing it.

Permit direct repairs only for concrete correctness, test, or documentation
issues within the accepted intent and scoped write boundary. A tightly coupled
selector addition must be recorded through the task API and pass existing scope
rules before editing. Requirement changes, new dependency edges, substantial
redesign, unrelated cleanup, and unresolved findings stop the gate for Astra or
the human decision owner. The reviewer cannot waive checks, edit acceptance
criteria to fit the implementation, approve lifecycle transitions, or merge.
Explicit task approval and repository commit policy still govern snapshot and
repair commits; the new repair activity needs scoped write authority because
today's review instructions are read-only.

Preserve the implementer's commits. Append separate repair commits with the
reviewer's agent-family author/committer identity and task reference; record
review invocation/model plus finding-to-repair-commit links in durable evidence.
Do not amend repairs into an implementer commit. If repository delivery squashes
or rebases, retain the original commit identities, contents and attribution in
the delivery evidence alongside the resulting mapping. A squash must not erase
who authored the implementation versus repairs.

After repairs, rerun affected validation and all required repository gates on the
final candidate; record commands, results, tested tree and environment. A repair
that fixes one check but leaves another required check failing does not pass.
Treat runner denial as unavailable validation, not a code defect: keep the
candidate for unrestricted validation and show the exact denial. The gate waits
for required evidence unless existing repository policy explicitly accepts that
specific deferral; record any permitted deferral in the verdict, never as passed.
Post-repair validation is not an independent review of the repaired code.

Use distinct proposed verdicts: **passed without repairs**, **passed with reviewer
repairs**, **changes required**, and **incomplete/escalated**. Both pass variants
require all findings resolved or explicitly disposed by an authorized decision
and final validation satisfied under repository policy. The second states which
parts the reviewer authored and **does not claim those repairs received an
independent second review**. Repositories requiring that assurance hold delivery
for a separate actor/human gate; do not recursively launch reviewers without an
explicit bounded policy. These are review evidence values, not task statuses or
human approval. A failed provider run or partially written result is never pass.

### 3.12 Budgets, authorization, and restart

Candidate initial limits are two reviewer starts, two total repair/validation
cycles, and thirty minutes of aggregate reviewer/repair/final-validation work
per delivery candidate lineage, including retries and delivery invalidations.
Expose each limit and permit scoped configuration; large repository validation
may require a larger authorized wall-time budget. Optional metered token/cost
caps must state units, reserve before dispatch, and include repairs and retries.
Unknown metering stays unknown; reject a required hard spend cap that cannot be
enforced. Attempts and wall-time remain enforceable without token telemetry.
No review policy increases the overall run/provider resource ceiling.

Persist consumed/reserved budgets before another invocation. Timeout, crash,
conflict repair, new head, requeue, and provider replacement do not reset lineage
limits. Coordinate with section 3.6's aggregate recovery allowance so recovery
cannot bypass the review cap. Budget exhaustion, repeated failed fixes, denied
authority, or unresolved findings produce an escalation with candidate refs,
findings, partial repairs, failed/skipped checks, and consumed limits. Automatic
delivery stops; resumption needs a recorded decision and any additional authority.

Admission checks review and scoped repair rights separately from completion;
choosing a reviewer grants neither task approval nor merge rights. Already
admitted review follows the captured policy after normal window expiry, bounded
by its own budget. Hard revocation is rechecked before mutations and delivery.
Changing a default affects future admissions; it cannot weaken an active gate.

Use a durable review attempt identity bound to task meaning, candidate identity,
review contract and policy revision. On restart, reconcile its recorded commits,
evidence, and final candidate before resuming. An interrupted repair remains
attributed partial work, not a pass certificate. Compare-and-set candidate and
attempt state under the existing claim/write boundary; only one worker may
repair that candidate at a time. Replayed evidence attachment, budget reservation,
and delivery mapping must be idempotent. Lost acknowledgements trigger evidence
reconciliation, not duplicate repair commits, PRs, or free reviewer attempts.

### 3.13 Content-specific coverage through delivery

Coverage is a verifiable relation between review input, final candidate, and
actual delivered content. Never infer it from task status, `pr_status`, a
`reviewed` tag, reviewer exit code, or a timestamp. Proposed durable evidence
includes repository identity; task-meaning digest; base commit/tree; reviewed
implementation commits/tree; final candidate commit/tree; repair commits and
attribution; review contract/crew/attempt; verdict/findings; validation evidence;
and the actual landing revision, parents/base, and transformation mapping.
The two pass verdicts retain different assurance labels even when both qualify
for automatic patch-review exclusion under this policy.

| Candidate or delivery event | Coverage and gate behavior |
| --- | --- |
| Same task meaning, base and final candidate, with complete passing evidence | Reuse the gate result; delivery still has to prove its mapping. |
| Reviewer repairs during the admitted attempt | Bind the final verdict to the repaired tree and record the self-authored subset; validate before issuing coverage. |
| Any later edit, including tests, formatting, generated files, or conflict resolution | Invalidate the prior gate for the new candidate; review the changed candidate and validate again within remaining bounds. |
| Task criteria, scope, or applicable contract changes | Invalidate even if code bytes match; re-establish review against the new requirements. |
| Commit IDs change but base tree and final candidate tree are identical | Carry coverage only with verified original-to-delivered mapping and preserved attribution; SHA inequality alone need not force another review. |
| Rebase onto a different base tree, even conflict-free or with identical patch ID | V1 invalidates coverage: changed surrounding code can change meaning. Review and validate the rebased candidate. |
| Squash or merge with the same reviewed base and resulting candidate tree | Verify landing parents/base and resulting tree against the certificate, retain original commit mapping, then carry coverage. |
| Unknown mapping, missing evidence, manual edits, or an unreviewed merge/conflict repair | Do not claim coverage. Hold managed before-PR delivery; if it landed outside that gate, record uncovered content for later review. |

Place the initial gate after the final planned base synchronization and before
push/PR creation, preserving implementation snapshots and separate repairs.
Before opening the PR, recheck the exact head and base against the gate result.
Later PR revisions or moving bases repeat the affected gate before managed merge;
the already-open PR stays open with stale coverage disclosed. Completion must
pin/recheck the reviewed head using the delivery provider's supported guard and
verify the actual landing result; merge-queue/base movement is not an exemption.
If those invariants cannot be enforced, do not promise covered automatic landing.
A race detected only after external merge yields uncovered delivery and an
escalation, never a retroactively fabricated pass.

For a bundle or epic, evaluate the final combined diff and retain each task's
criteria/evidence. Child pass flags do not cover sibling interactions. When only
part of a delivery is covered, record its exact covered and uncovered subsets;
conservatively keep the delivery eligible for review unless the full requested
scope is proved covered. No-diff outcomes and wrapper completions are not code
deliveries. Content-equivalence optimization beyond these exact-tree rules is a
later design decision, not a promise that matching file lists or patch IDs suffice.

### 3.14 After-landing review and the trigger boundary

[ORB-11315] owns the proposed shared automation-trigger design: delivery identity
and threshold counting, immutable work batches, observed/dispatched/successfully
covered checkpoints, cold-start baselines, pending accumulation, overlap/dedupe,
definition changes, crash/concurrent evaluation and bounded reconciliation. That
design is not present yet; this task does not create a competing scheduler
contract or activate a new trigger. Operation-mode owns review meaning, eligible
coverage, assurance labels, reviewer/repair policy and authorization. Triggers
decide **when** eligible work is due; auto-tasks still mint tasks and routines
still invoke jobs through their existing owners.

The review-facing handoff to that design supplies distinct landed-delivery
identities and revisions, policy/contract version, exact coverage references,
uncovered subsets/reasons, and authorization/resource inputs. With an explicitly
enabled after-landing review definition, only uncovered eligible deliveries count
toward its threshold. For example, six actual deliveries of which four have
valid before-PR coverage contribute two to the review threshold of three; the
next uncovered delivery makes review due. All seven remain independently eligible
for QA under QA's own trigger policy. `none` creates no automatic review obligation
on its own; a separately authorized workspace sweep may explicitly include such
uncovered deliveries. Policy changes do not silently erase pending obligations.

Exclude deliveries with valid before-PR coverage from redundant automatic patch
review, including coverage honestly labelled as containing reviewer repairs.
An after-landing reviewer may read covered neighboring changes as context for
uncovered interactions, but should not reopen them merely to satisfy a count.
Manual broader architectural review remains available with explicit scope and
budget. This changes the current whole-window sweep semantics, so migration
must disclose the narrower automatic scope rather than claim equal assurance.
QA stays independent: code inspection and candidate tests do not establish the
integrated runtime behavior that QA exercises.

After-landing review operates on a pinned immutable batch and returns findings
plus exact examined coverage. Its default output is verified finding tasks,
not writes onto the integration branch. Direct repair requires separately
authorized scoped work through the normal isolated delivery route and inherits
the same attribution, validation, and budget rules. Successfully examining a
batch may establish review coverage while findings remain open; coverage means
examined, not bug-free or remediated. Record verdict and finding disposition
separately. A partially examined/failed batch cannot be treated as wholly
covered because the sweep task was minted or terminalized. Pass accepted coverage
and pending findings to ORB-11315's checkpoint mechanism; do not implement a
second cursor in prompts or overload today's last-fire slot.

### 3.15 Review observability, evaluation, and staged implementation

Extend existing task/run/audit projections with effective review-policy and its
source, selected/resolved reviewer, candidate/base/landing identities, evidence
links, verdict, repairs/authorship, validation/deferment, consumed limits,
invalidation reason, escalation, and coverage mapping. Trigger diagnostics
consume that evidence to explain each delivery's exclusion, uncovered count,
pending batch and coverage lag. Retain replayable manifests and source objects
under an explicit retention policy; missing objects mean unverifiable coverage.
Keep persistence in existing run/task/store owners, scheduling and coverage
acceptance in orbit-automation [ORB-11330], and authority/lifecycle composition
in Core, with engine mechanics and thin adapters following
[Architecture](../../../ARCHITECTURE.md). Exact fields/migrations require later
review; this proposal adds no persisted artifact or cross-crate dependency.

Compare `none`, `before-pr`, and `after-landing` cohorts at fixed operation mode,
scope/complexity, implementer and reviewer configuration, and follow-up window.
Extend section 3.8's accepted-change denominator with **total implementation +
review + repair + validation + recovery cost/time**, separately exposing each
component, failed attempts, metering coverage, and QA cost. Measure confirmed
findings and severity, sampled missed defects, repair regressions/reverts,
human intervention, PR latency, coverage invalidations/rechecks, redundant review
avoided, uncovered age, and budget exhaustion. Separate reviewer-authored repairs
from independently inspected implementation; fewer escalations alone can conceal
bad approvals. Use baseline/cohort sample disclosures and quality review before
claiming savings; the preset's earlier 25% coordination target is not evidence
that automatic review improves quality or combined cost.

Implement in stages after this documentation-only task:

1. Agree the open decisions and versioned evidence/authority contract. Add typed
   policy resolution and explanation with compatibility `none`; verify custom
   resource handling without running reviewers or changing schedules.
2. Add opt-in before-PR review/repair to the existing PR path for finite approved
   task sets. Validate fresh invocation, separate commits, all verdict/failure
   cases, final gates, crew refusal, budgets, cancellation and restart replay.
   Preserve `completion: review` and existing merge authorization.
3. Add delivery coverage enforcement and retention with tests for later edits,
   same-tree rewrites, different-base rebases, conflict fixes, squash/merge,
   bundles, manual landing races and missing evidence. Keep uncertain coverage
   uncovered; do not enable exclusion until this boundary is trustworthy.
4. Integrate explicitly with ORB-11315's trigger/checkpoint contract. Test mixed
   covered/uncovered batches, failed/partial sweeps, duplicate observations,
   concurrent evaluators and restart without lost pending work. Demonstrate QA
   remains independent and migrate only explicitly selected definitions.
5. Evaluate bounded cohorts before wider enablement. Rollback stops new review
   admissions/trigger ownership, preserves readable evidence and outstanding
   obligations, and drains or escalates active gates using a supporting binary.
   It must not reinterpret an in-flight `before-pr` gate as `none`.

Remaining costs include extra latency, repeated review on busy bases, evidence
retention, incomplete provider metering, and correlated implementer/reviewer
errors. The controls bound and expose those risks; they cannot guarantee defect
absence or independent review of self-authored repairs.

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
- [ORB-11316] — extends review timing, scoped repairs, coverage, and evaluation.
- [ORB-11315] — will define shared automation triggers and coverage checkpoints.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
