---
title: Review Gate — Design
owner: codex
last_updated: 2026-10-05
last_validated: 2026-10-05
status: Accepted
feature: review-gate
doc_role: design
type: design
summary: Shipped review contract — captured timing, the before-PR gate whose reviewer fixes its findings as a second commit, what validation records establish, lineage budgets, managed completion, delivery coverage, surfaces, and rollback.
tags: [review-gate, review-policy, automation, delivery, operations]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/review/**", "crates/orbit-core/src/application/automation/after_landing.rs", "crates/orbit-store/src/driver/sqlite/review/**", "crates/orbit-automation/src/review/**", "crates/orbit-engine/src/executor/automation/vcs/review_gate.rs", "crates/orbit-store/src/repository/task/v2/artifacts.rs", "crates/orbit-store/src/repository/task/coordination/lifecycle.rs"]
related_features: [automation-triggers, activity-job, auditability]
related_artifacts: [ORB-11333, ORB-11528, ORB-11545, ORB-13890, ORB-13896, ORB-13989, ORB-13992, ORB-14192]
---

# Review Gate — Design [ORB-11333]

This file describes what shipped. A preference edit grants no authority and
changes no schedule; a verdict grants no merge permission.

## 1. Preferences: two switches [ORB-13992]

Automatic review has two independent switches. Before-PR review is a
`config.toml` boolean; after-landing review is the `delivery-code-review`
auto-task's own `enabled` flag. Config preferences resolve **built-in →
global → workspace**. Unknown keys and out-of-range values fail config load.

| Key | Values (default) |
| --- | --- |
| `review.before_pr` | bool (`false`) |
| `review.minutes` | 1..=1440 (30): wall-clock limit for one candidate's review |
| `operation.review_crew` | crew name (before-PR reviewer; crew of after-landing review tasks) |

`review.before_pr` holds PR creation for a fresh reviewer (§3); it needs an
explicit `review_crew`, and admission escalates `review_crew_unconfigured`
until one is set. Each candidate gets one review, bounded by `review.minutes`
(§5).

After-landing review is carried out by the workspace's shipped
`delivery-code-review` delivery auto-task [ORB-13896], switched with
`orbit auto-task toggle delivery-code-review on|off`. While enabled it freezes
landed base-branch deliveries into batches and mints one review task per batch
at its threshold or maximum wait. When `review_crew` is set it is the crew of
every review task the consumer mints; unset, the definition's template crew
(`system`) applies. Changing `review_crew` changes only future mints, never the
consumer's epoch or its retained debt. The consumer admits only on the machine
that owns the workspace (see [delivery automation
operations](../automation-triggers/5_operations.md)). After-landing review
never affects delivery admission, local or distributed.

`orbit config show` (`review` in `--json`), `orbit doctor` (the `review`
check), the dashboard Config tab and `orbit.drain.probe` render one view of
both switches, each with its source: before-PR on/off with its minutes and
crew, and after-landing enabled with when the next batch is due. While the
after-landing consumer is enabled the view adds its health: whether it is
present, whether this host owns it, whether it is wedged on a closed action
or stalled, whether its branch and review crew resolve, its scheduling state,
and when its last batch was minted or covered. Anything short of healthy — the
definition missing, owned by another machine or by none, wedged, stalled, held
for an operator (`definition_changed` for an edit the evaluator would not adopt
automatically, which the row names; `needs_attention`;
`retry_deadline_expired`), on a branch that does not resolve, or naming a crew
that does not — is an `error`, so `orbit doctor` exits nonzero. So is
before-PR review switched on without a resolvable `review_crew`.

**Migration.** `operation.review_policy` and `operation.review_minutes` are
deprecated: they are translated on load with a warning naming each key, and a
later release makes them errors. `before-pr` sets `review.before_pr = true`;
`none` turns neither switch on; `after-landing` enables the consumer only while
no operator has configured it (its `updated_by` is unset or `system`) — once
`orbit auto-task toggle` or any other edit stamps an actor, its own flag
decides. `orbit auto-task list`/`show` report that as `effective_enabled`. A
`[review]` table in the same file wins over the deprecated spelling.
`operation.review_minutes` becomes `review.minutes`. The budget keys
`operation.review_reviewer_starts` and `operation.review_repair_cycles` are
retired [ORB-13989]: one review per candidate counts neither, so both are
warned about by name and ignored. The resolved-policy version is 2; a
version-1 snapshot fails closed and must be replaced. A snapshot captured
before the retirement still carries `reviewer_starts` or `repair_cycles`; it
reads and the values are ignored.

## 2. Captured timing

Review timing is captured once per delivery run and never re-read. Every
submission in the delivery family (`workspace_auto_pipeline`,
`task_auto_pipeline`, `task_gate_pipeline`, `task_pr_pipeline`,
`task_local_pipeline`) carries a versioned `review`
snapshot in its immutable input: timing and its source, the configured
reviewer crew and its source, the lineage budget, the policy version, and the
owner's `workflow.required_validation_commands` list. A
parent-authorized child inherits its parent's snapshot exactly; any other submission resolves from the
workspace preferences at that moment. A distributed drain
(`workspace_pull_pipeline`) captures the same snapshot, so the `before_pr` it
declares to owners is the value it was submitted with. Ordinary input naming
the reserved `review` key is refused, and a resume keeps its persisted input,
so switching `review.before_pr` off never weakens a gate that is already
active and switching it on never gates a run already admitted. A run captured
under the retired `after-landing` policy value reads as not gated.
`review.before_pr` is refused at submission for `task_local_pipeline` delivery,
with no exemption: epic assembly was the one caller that gated a combined
candidate later, and it is retired [ORB-12491].

## 3. The gate

`task_pr_pipeline` runs the gate after the final base synchronization and
before push/PR creation, as four top-level steps: `review_gate_admit`,
`review` (`agent_review_repair`), `review_gate_settle`, and
`review_validate` (§3.1). One fresh reviewer examines each candidate and
fixes what it finds; there is no second review round [ORB-13989]. With
before-PR review off at capture, or a checked no-diff exemption, the gate reports
`applies: false` and publication proceeds unchanged with no certificate.

Admission pins the candidate (base and head commits and trees, every
implementation commit with the attribution Git recorded), digests each task's
meaning (title, description, criteria, plan, selectors, tags, relations, type;
never comments, summaries, status, or priority), resolves the configured
review crew on this host inside the run's `allowed_crews`, reserves the
candidate's one review against the lineage ledger, and writes `review-manifest.json` on every
task under the run's authority. An unconfigured, unresolvable, or excluded crew
escalates (`review_crew_unconfigured`, `review_crew_unavailable`,
`review_crew_excluded`); the gate never substitutes the implementer. A
retried admission in the same run resumes its open attempt for the same
candidate and task meaning within the same minutes; a different
candidate, or an attempt another run of the lineage left open, is released
as `incomplete` first (§5). The manifest also carries the owner's
`workflow.required_validation_commands` captured when the delivery was
admitted. Each command must appear as a required passing record in the review;
settlement never consults a later mutable config value. A legacy admission
without this snapshot fails closed and directs the operator to dispatch a
fresh delivery run.

The deterministic gate writes manifests, certificates, settlement comments,
and any fix-driven selector changes as `system`, regardless of the
operator identity that opened the runtime. Run ownership still authorizes
artifact writes; it does not identify their author. Reviewer identity and crew
remain in the manifest and certificate, and `review-report.json` retains the
reviewer agent attribution. Any history generated by a system write uses the
same system actor; settlement does not add synthetic history stubs.

The reviewer is a fresh invocation, a crew separate from the implementer's,
with its own instruction, tool allowlist, and 60-minute wall clock, shortened
to what the candidate's `review.minutes` has left (§5). It runs under the same
sandbox as the implementer, with write access to the worktree. It reads the
manifest, verifies claims against code, fixes concrete defects directly in
the worktree — any path a fix requires, each listed on the finding it fixes
— runs validation, and persists `review-report.json` (schema version 1:
verdict, findings with dispositions, the paths and a `change` description
for each fix, validation records with `passed` / `failed` / `denied` /
`not_run`, the `role` each is evidence of, and optional `check` identity,
escalation). It never runs Git writes, changes task lifecycle, approves, or
merges.

The instruction carries the report's exact JSON Schema, and
`orbit.task.artifact.put` validates a `review-report.json` on attach,
refusing a mismatch with the offending field named so the reviewer can fix it
while it still runs [ORB-13890]. Both the validator and settlement read
through `ReviewReport::parse`, which accepts benign shape drift that leaves
the meaning unambiguous — a bare-string `disposition`, enum labels in another
case or with `-`/space separators, `pass`/`fail`/`skipped` outcomes, a single
path string, a numeric finding id or string `schema_version`, a missing
`schema_version`, `summary`, or finding `severity`, and `null` lists — and
refuses anything else.

Admission and settlement retry transient failures (three attempts,
exponential backoff) and the reviewer step retries once; each then gets one
`step_failure_recovery` diagnosis before the run fails. A retried reviewer
continues the same attempt and must verify or revert edits an interrupted
invocation left in the worktree. Decisions are refusals that neither retry
nor recover: a settled `reject` or `incomplete` verdict, a reviewer timeout, an exhausted
budget, an unconfigured, unavailable, or excluded crew, and a local route.
`review_validate` has no retry and no step recovery: its failure is the
review's `reject`. Every such failure is still eligible for the job's final
recovery (one look by `workflow.final_recovery_crews`) before the failure
handoff runs.

### 3.1 Verdicts, the two-commit shape, and revalidation [ORB-13989]

The candidate the reviewer receives is the implementation commit (or
commits), and the implementation is never amended. Settlement commits
everything the reviewer left uncommitted as one second commit on top of it:

```text
<implementation commit(s)>     authored by the implementer
review: <first line of the reviewer summary> [<task ids>]
                               authored <family>-reviewer, Orbit committer
    Findings: F1, F2
    Paths: src/a.rs, docs/b.md
    Orbit-Review-Attempt: <attempt id>
    Orbit-Review-Crew: <reviewer crew>
```

| Verdict | Means | Reviewer commit | Delivery |
| --- | --- | --- | --- |
| `accept` | No defects; every required check passed; nothing changed | none | PR opens on the implementation head |
| `accept_with_fixes` | Every finding fixed; required checks passed on the fixed tree | one | `review_validate` reruns owner validation, then the PR opens on the reviewer commit |
| `reject` | A substantive finding stays open (unfixable, out of intent, a non-release `CHANGELOG.md` edit) | kept if made | Substantive findings block; named external checks alone hold for evidence. Both commits stay preserved, no PR |
| `incomplete` | The review could not establish the candidate | kept if made | Named external checks enter an awaiting-evidence hold; a wall-clock timeout requeues a continuation; other escalations block |

Settlement posts one comment on every task in the bundle:

```text
before-PR review settled attempt `<id>`: verdict **accept_with_fixes** (assurance: …).

Findings:
- `F1` [high, fixed] <summary>
  - Changed: <what the reviewer changed> (<paths>)
- `F2` [low, open] <summary>

- Reviewer: crew `<crew>` (<provider> / <model>)
- Implementation: `<sha>` on base `<sha>` (N commit(s), unchanged by review)
- Reviewer commit: `<sha>` `review: …` by <author>
- Final candidate: `<sha>`
- Selectors widened for reviewer-changed paths: …
- Validation on final candidate: … record(s) […], complete: …
- Owner-required checks: <captured commands, or none configured>
- Not established by this review: <failed diagnostics and their sources, or none>
- Required checks retained from earlier report revisions: <command and outcome, or none>
- Reviewer runtime: …s of … min
- Escalation: …

<what the verdict means for delivery>
```

With a reviewer commit, settlement returns `reviewer_fixed: true`,
`implementation_head_sha` (the head the reviewer examined), and
`review_fixes`, a `## Review fixes` section listing each fixed finding with
what changed and naming the reviewer commit, and a `## Review validation`
section listing the owner-required commands and each record's raw outcome,
role, rationale, control kind and sources. When a diagnostic failed (§4),
`review_fixes` also carries a `## Review validation limits` section naming
each failed diagnostic and its sources, so the PR never reads as a claim that
the whole workspace passed. `pr_open` appends these sections to the PR body,
generated or supplied, before bounding it.

`review_validate` (`candidate_validate`) runs only when `reviewer_fixed` is
true. It first attributes every path changed between
`implementation_head_sha` and the new head to the delivered tasks.
Settlement has already widened the selectors over every repaired path, and
any path still unowned widens the first task's selectors with review
provenance instead of failing ([ORB-13990]). It then reruns
`workflow.required_validation_commands` on the reviewer commit. A failed
command fails the step with no retry and no recovery, and the failure handoff reports verdict `reject` even
though the certificate recorded `accept_with_fixes`: the certificate says
what the reviewer established, and the handoff says why publication stopped.
## 4. What the validation records establish [ORB-11528] [ORB-11545]

An honest reviewer records more than the checks that had to pass, so each
validation record also carries a `role` saying what it is evidence of, and
`orbit_automation::review::validation_evidence` decides what the set
establishes. Settlement and delivery coverage both read that one function, so
a certificate cannot mean one thing when it is issued and another when it is
spent. It also requires every command in the immutable owner snapshot to be
present as `required` and `passed`, or to be a valid superseded record followed
by a passing check with the same command or explicit check identity. Command
identity ignores whitespace and leading `NAME=value` assignments, so a required
`TMPDIR="$PWD/.orbit/tmp" make ci-fast` pass establishes host-required
`make ci-fast`. `make ci-fast-extra` and `FOO=1 make other` do not. A
diagnostic, exclusion, negative control, omission, or later unrelated pass
cannot satisfy a host-required command. Certificates retain that snapshot;
legacy certificates without it cannot be spent as coverage.

| `role` | Meaning | Passing requires |
| --- | --- | --- |
| `required` (default) | A check the final candidate must pass | `passed`; `failed`, `denied`, and `not_run` all block |
| `expected_failure` | A negative control — the superseded assertion, the pre-fix reproduction | `failed`; any other outcome contradicts the claim |
| `excluded` | An action outside the authorized scope, deliberately not performed | `not_run` or `denied`; actually running it contradicts the exclusion |
| `superseded` | A diagnostic attempt a later required check replaced | a later record that is `required` and `passed` and names the same check: the same `command` (whitespace ignored, and a leading `NAME=value` assignment ignored), or the same non-empty `check` identity when the command itself was corrected. A record without `check` is matched by its command, so a missing optional field never downgrades a pass. An unrelated later pass, a corrected command with no shared identity, or a related check that did not pass is not a replacement |
| `diagnostic` [ORB-14192] | A nonrequired observation of the final candidate, such as a workspace-wide suite beyond the task's checks | `passed` or `failed` as observed (`not_run`/`denied` contradict it: an action never taken is `excluded`). A failed diagnostic lists `sources`, every one outside the candidate's scope, and shares no check with a required pass. It supplies no coverage and creates no requirement |

A negative control is bound to more than its label [ORB-14192]: an
`expected_failure` record names its `control` kind — `pre_fix` (the
reproduction on the pre-fix tree), `superseded_assertion` or
`counterfactual` (both run on the candidate) — and the `sources` it
exercises, every one inside the candidate's scope. A control run on the
candidate cannot share its check with a required pass there, while a pre-fix
reproduction may: the fix is what makes it pass. The scope is every bundle
task's selectors plus a `file:` selector for every path the implementation and
reviewer commits change, and the certificate records it as
`validation_scope`. A source is a repository-relative path or a `file:`/`dir:`
selector, judged with the shared selector-overlap grammar. A failed required
check therefore cannot become nonblocking by changing only its role or note:
as a control its sources must be the task's own, as a diagnostic they must
not be, and an honest in-scope failure satisfies neither. An unrelated
failure — ORB-14151's workspace run failing only in engine fixtures the task
never touched — is a `diagnostic`, not an `expected_failure`: the bounded task
still passes on its required checks, and nothing imports a workspace-green
requirement into it.

Report revisions are retained [ORB-14192]. Every time a `review-report.json`
is attached, the artifact store parses it and appends its attempt, digest,
verdict and validation records to `review-report-history.json` under the task
lock, in the same manifest write that replaces the report, so no accepted
revision can disappear before settlement, across a host restart, or through
a same-attempt retry; re-attaching identical bytes adds nothing. A claimed
reviewer's report reaches the owner as claim evidence, and that commit
appends the revision the same way. Only the store writes that artifact. It holds 64 revisions per task: the oldest
revision of another attempt makes room, and one attempt that fills it is
refused rather than losing its own history. Settlement reads every bundle
task's history for the attempt (an unreadable one is `incomplete`), and every
`required` record an earlier revision made stays an obligation: the final
records must name the same check as `required` (and pass) or `superseded`
(and be replaced), or as `excluded` when the retained record never ran.
Omitting it, or relabeling it `diagnostic` or `expected_failure`, is
`validation_incomplete` — ORB-14191's replacement report that silently
dropped a failed required CodeQL run settles `incomplete`. Retained records
the final report does not repeat verbatim are kept on the certificate as
`retained_obligations` with their report digest and observation time. The
obligations are only what reports recorded; the gate never infers
requirements from free-form repository instructions.

At least one `required` record must have passed, so a set of controls and
exclusions alone is never coverage. Every role other than `required` must
carry a `note`; an unexplained reclassification is refused rather than
trusted. A record written before this contract carries no role and is read as
a required check, so older evidence keeps its conservative meaning. The
certificate keeps every raw observation with its classification — a superseded
or diagnostic failure is preserved, never erased — and the verdict comment,
task review projection (`validation_limitations`, `retained_obligations`,
`validation_scope`) and PR body disclose the breakdown and what the review did
not establish. `validation_complete` means every required check passed and
every other record is consistent with its role; failed diagnostics stay
failed and are outside what it asserts. A denied required check keeps its own `validation_unavailable`
reason: the runner refused, which is neither a defect in the candidate nor
evidence about it.

The contract version stays 1: a record carrying no role decides exactly as it
did before, so older role-less evidence is not reinterpreted. A superseded
attempt now requires the later required pass that names the same check; a
certificate that treated an unrelated later pass as a replacement becomes
incomplete when coverage re-reads the records. Coverage re-derives the rules
over the certificate's own `validation_scope` and `retained_obligations`, so
an `expected_failure` without `control` and in-scope `sources`, or a failed
diagnostic judged with no recorded scope, is no coverage: certificates issued
before [ORB-14192] that relied on a note-only expected failure stay uncovered
and are re-established by a fresh review. Candidates already refused under the
old rule recover through a fresh run, not by editing stored evidence.

Settlement rechecks the checked-out head against the admitted candidate,
reads every bundle task's report with its artifact provenance — reports that
predate the attempt are ignored, the rest merge into the most severe verdict
with every distinct finding and validation record — (none, a wrong attempt,
another contract version, or an unreadable report is `incomplete`), commits every
uncommitted change as the one reviewer commit (§3.1) authored
`<family>-reviewer <<family>-reviewer@orbit.local>` with the Orbit committer
and the `Orbit-Review-Attempt` and `Orbit-Review-Crew` trailers, widens the
tasks' selectors with an exact `file:` entry for every repaired path they do
not cover, declared in a finding or not, with one `context_files_widened`
history entry (step `review`), and cross-checks the claim. The reviewer may
change any path the repair requires ([ORB-13990]). An accept with open
findings, a claimed fix that changed nothing, a claimed clean accept that
changed the tree, validation records that do not establish the candidate
(§4), or any task-meaning change other than selectors added through the task
API downgrades the verdict to `incomplete` with the reason recorded.
Verdicts are `accept` (`independent_review`), `accept_with_fixes`
(`independent_review_with_self_authored_repairs`; the fixes were validated,
not independently reviewed), `reject`, and `incomplete`. Certificates and
reports written before [ORB-13989] used `passed_without_repairs`,
`passed_with_repairs`, and `changes_required`; they read as the new labels,
and their retired `repair_cycles` and `rework_requested` fields are ignored.
The certificate (`review-gate.json`, recorded immutably in the host store and
indexed by final candidate tree when passed) binds verdict, reviewer
identity, base, reviewed and final candidate, implementation and reviewer
commits, findings, validation, consumed budget, and escalation. Settlement is
idempotent: a replay reconciles the recorded certificate.

Settlement persists in order — reviewer commit, ledger charge, certificate,
then each task's `review-gate.json` and verdict comment — and a replay
resumes from whichever step last persisted. A head one commit past the
admitted candidate is adopted as the attempt's reviewer commit only when that commit
has the candidate as its sole parent, carries this attempt's trailer, and has
the reviewer author and Orbit committer; its paths are judged and widened as
they were before the commit. A ledger that settled
without a certificate is re-judged against the ledger as it stood before its
own charge, with the reviewer runtime it recorded. The certificate is issued only
when that judgement reproduces the recorded verdict; otherwise the replay
refuses with `settlement_diverged`. Nothing is committed
or charged twice. A recorded certificate is restored onto every bundle task
that lacks it before the replay reports its verdict. A pass still refuses
task-meaning or head drift first.

An accept returns `reviewed_head_sha` / `reviewed_base_sha`; `pr_open`
refuses (`review_gate_stale`, phase `stale-review-gate`) when the checked-out
head or pinned base differ. `reject` and `incomplete` refuse the step. The
failure handoff — for any failure of `review_gate_admit`, `review`,
`review_gate_settle`, or `review_validate` — releases the attempt if it has
no verdict yet (§5), commits leftover reviewer work under the reviewer
identity (`partial_repair_commit`), leaves the implementation and reviewer
commits as they are, pushes the candidate branch, and opens no PR.
Substantive failures block with `review_gate_escalation`. A reviewer wall-clock
failure records `review_timeout_incomplete` and requeues the task; it does not
retry the reviewer within the failing step. The partial report stays attached,
and admission resumes the latest released attempt for the same candidate and
task meaning. A newly admitted candidate receives the earlier report in the
manifest's `previous_report`, as advisory context requiring revalidation.
Finalization preserves the timeout requeue rather than adding a generic block.

For a single-task delivery, an otherwise complete nonpassing report may name
`external_evidence`: a list
of `{kind, name, command, artifact}` requirements, where `kind` is `hosted_ci`,
`native_os`, or `codeql`. Each exact command must have an unavailable required
record (`not_run` or `denied`). All missing checks must be named, all other
validation must be consistent, and no substantive finding may remain open.
The typed requirements classify the hold even when an older reviewer spelled
its evidence-only verdict `changes_required` / `reject`; the certificate retains
that reported verdict and never qualifies as passing coverage. Open defects,
failed checks, dropped earlier obligations, and host-detected candidate or
meaning drift never enter this hold.

Settlement retains its nonpassing certificate and writes
`review-evidence-hold.json`, pinning the attempt, candidate revision, task
meaning, and requirements. The failure handoff records
`review_awaiting_evidence`, keeps the task in progress and its candidate
recoverable, and holds publication. Run finalization preserves this decision.
Admission refuses to continue while the matching hold has missing evidence.
For each requirement, attach its result through `orbit.task.artifact.put` at
its named artifact path, plus the separately referenced nonempty log artifact:

```json
{"schema_version":1,"attempt_id":"<held attempt>",
 "candidate":{"commit":"<held commit>","tree":"<held tree>"},
 "kind":"hosted_ci","name":"Windows CI job","command":"<exact command>",
 "outcome":"passed","log_artifact":"evidence/windows-log.json"}
```

Only results matching the held attempt, exact revision, kind, name and command
count. Stale, unrelated, failed or logless evidence leaves the hold in place.
After all results and logs arrive, an unchanged task still owned by that hold's
run moves to backlog with `review_evidence_received`. This queues fresh review,
not acceptance: the new reviewer verifies the evidence, and every publication
and validation gate still applies. An operator block or later review decision
is never undone by a late artifact attachment.
Passing grants no lifecycle transition; `completion: review` still stops at
the handoff.

## 5. Budgets

The ledger is keyed by workspace, sorted task set, base branch, and delivery
run lineage: the first run of the resume chain (`retry_source_run_id`). A
resumed run shares its source's ledger, so resuming never resets the budget;
a fresh delivery run of the same tasks — the re-admission after a block —
starts a new lineage with a full budget [ORB-13890]. Settlement uses the
lineage its admission named. The first budget written on a lineage is
captured; a later config change cannot expand or replace it. Each candidate
(head commit plus task meaning) gets one review [ORB-13992], reserved before a
reviewer launches: once an attempt on it settles with a final verdict, or its
reviewer runtime reaches `review.minutes`, the candidate is not reviewed again.
A changed candidate — new implementation work or a completion rebase — is a
new review with its own minutes. The PR pipeline checks the same lineage
before `implement_bundle`: a resumed lineage whose latest candidate spent its
minutes without a verdict refuses without invoking the implementer. A
candidate that was reviewed does not block the preflight, since new work makes
a new candidate. This preflight reserves nothing and writes no reviewer
manifest; final admission still atomically checks the candidate.

An operator can renew a selected lineage with
`orbit task review-reset <task-id> --lineage '<exact-lineage-key>' --reason '<decision>'`.
The refusal names this command. The equivalent MCP action is
`orbit.task.review_reset` (served with `orbit mcp serve --operator`), with
`workspace`, `id`, `lineage_key`, and `reason`. Interactive CLI callers have operator authority; noninteractive operators
use the audited `ORBIT_OPERATOR=1` override. Ordinary agents and managed
runs cannot reset budgets. The lineage must contain the selected task.
The decision atomically closes an open attempt, records actor, time, reason,
previous budget and consumption, and starts a fresh allowance. All attempts
and earlier decisions remain; the next attempt uses index N+1. Late
settlement or reviewer events for retired attempts are refused. A reset does
not approve a verdict or a merge. Inspect the returned ledger after a lost
reply before repeating a reset.

By default the captured limits remain. Pass `--adopt-configured-budget`
(MCP `adopt_configured_budget: true`) to explicitly adopt the currently
configured limits. Raising config alone never changes a ledger.
For ledgers poisoned by old timeout wall-clock accounting, copy the exact
lineage key from the original refusal or review manifest, record why the
charge is invalid using the reset command above, and resume the delivery.
This also supports the older task/base keys that predate run-root lineages;
no hand-edit of SQLite or task state is needed.

The lineage is charged reviewer process runtime, not wall time. The engine
reports the start and end of every reviewer dispatch — each retry and the
post-recovery re-attempt included — and the attempt accumulates what they
add up to. The charge recorded at release is a floor, not a freeze: reviewer
runtime recorded afterwards still counts against `review.minutes` before the
attempt is settled, and a later start is bounded by what those invocations
have already spent. Retry backoff, `step_failure_recovery`, the gate's own
steps, and time between a run's end and its resume are never charged. A
reviewer whose end was never reported (its process died with the run) is
charged up to the run's end, never past its start plus its own wall-clock
bound.

No attempt stays open after its reviewer step fails or its run ends. The
failure handoff releases every attempt the run admitted that has no verdict —
for a bundle as for a single task, before any task-shaped check — settling it
`incomplete`, marked released, with the reviewer runtime charged so far. Run
finalization releases whatever a terminating run still holds, whether it
failed, was cancelled, or was found dead and interrupted, so a fresh lineage
never strands the old one open; the ledger indexes the run holding each
lineage (store migration `lineage_holder_run`) to find them. The next
admission of a lineage also releases an attempt another run left open. A
resumed run that reuses the admission checkpoint may still settle a released
attempt with its verdict, keeping the runtime already charged and adding the
reviewer runtime it records; a released attempt that a later start superseded
is stale.

`review.minutes` is a wall-clock limit on the candidate's review. Each
reviewer invocation records its start, and the ledger sets its deadline to the
lesser of its activity timeout and half the seconds the candidate has left; the
engine shortens the reviewer's wall clock to that deadline and refuses the
step (`review_minutes_exhausted`) when nothing is left. An accept that
finished within its deadline settles on its evidence. A reviewer commit costs
no separate budget; there are no reviewer-start counts or repair cycles.
Exhaustion refuses admission (`review_budget_exhausted:
review_candidate_reviewed | review_minutes_exhausted`). Provider token/cost
caps are not enforced; usage stays unknown.

## 6. Managed completion and landing

`pr_complete` under a gate pins the provider-reported PR head against the
reviewed head before merging (`review_gate_stale` on a moved head), then sends
that SHA as the `sha` precondition on GitHub's synchronous REST merge mutation.
A push between inspection and mutation is rejected by the provider. Gated runs
wait locally for pending checks; they never enable auto-merge or enter a merge
queue. Queue-only branches and other unsupported synchronous merges fail closed,
leaving the task in review. The one refusal retried is GitHub's "Base branch was
modified" (HTTP 405), raised when another merge lands first [ORB-14205]: after
the ordinary poll wait, completion re-reads the PR, reapplies its head, branch,
base, check, review and merge-policy gates, and resends the same `sha`, for at
most three requests within the unchanged wait budget. Ungated runs retain
ordinary `gh pr merge` and repository-enabled auto-merge. See the
[provider merge contract](https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request).

A conflicting reviewed PR is never merged as rebased, unreviewed content
[ORB-13890]. `complete_pr` runs with `re_review_on_conflict`: it rebases the
branch locally through the pinned `git_rebase` (a real conflict still reaches
`pr_conflict_recovery`), publishes nothing, completes no task, and returns
`re_review_required` with the rebase checkpoint. The pipeline then reviews the
rebased head as a new review in the same lineage (`re_review_gate_admit`,
`re_review`, `re_review_gate_settle`, and `re_review_validate` when that
reviewer committed fixes), republishes the reviewed final candidate under a
lease on the old published head (`re_push`), and completes it
(`complete_reviewed_pr`). A second conflict there, a caller without the
flag, a non-accept re-review, or a failed revalidation keeps the published
PR and the task in review.
`complete_pr` is skipped on review-only and no-diff routes, and a `when:` may
not read a skippable step's output, so `re_review_gate_admit` and
`re_review_gate_settle` always run: with `re_review_after: complete_pr` the
admission reads that step's checkpoint from the run's recorded pipeline (a
resume inherits it), answers `re_review_not_required` unless it asked for a
re-review, pins the recorded rebase base, and fails when the worktree head is
not the recorded rebased head. Its `applies` gates the remaining steps.

After the verified merge, completion reads the merge commit,
fetches it, and records a landing: `fast_forward`, `squash`, `merge_commit`,
or `rebase` when the landing started from the reviewed base tree and produced
the reviewed final tree, otherwise uncovered with `base_changed`,
`candidate_changed`, `objects_missing`, or `mapping_unknown`. An externally
completed merge (including one observed while polling) is recorded as uncovered
with `external_landing_race`; the landing audit includes `managed_merge`, which
requires a conditional mutation response matching the subsequently observed
merge commit. A landing is never fabricated.

## 7. Delivery coverage

Delivery observation asks the host store for passed certificates whose final
tree equals the landed tree, verifies the certificate objects still exist and
every task still has the reviewed meaning, and lets
`orbit_automation::review::exclusion` decide: same base tree, same final
tree, no contradicting managed landing. Only a `landed_code_review_v1`
consumer excludes; QA counts every landing. Excluded landings leave
`pending`, live in the consumer's `excluded` list, do not count toward the
threshold, and are absent from `examined_deliveries`. An exclusively excluded
prefix advances the covered cursor without an examination receipt so
observation cannot stall; interleaved exclusions travel with the next frozen
batch as readable context (`exclusions`) and retire with that examined
range. Later edits, task drift, a different base, an unreviewed conflict
repair, missing objects, or an external landing race keep the landing an
ordinary obligation. Exclusions apply when a landing is first
observed; a certificate that arrives later does not rewrite pending debt.

## 8. Surfaces

`orbit config show`, `orbit doctor`, the dashboard Config tab and
`orbit.drain.probe` report both review switches with their sources (§1). `orbit task show --json`, the task API, and the
task detail view carry a `review` block: verdict, assurance, reviewer
(including `same_model_as_implementer`), base/reviewed/final candidate,
implementation and reviewer commits, findings, validation, consumed and
remaining budget, landings, and stale-gate reasons. Auto-task inspection and
the automation panel show `excluded` landings with their certificate. Audit
rows `review.gate` cover preflight, admit, settle, release, and landing. A
completed settlement or landing check records `success`, even when its verdict
blocks delivery or its landing is uncovered; read `outcome.verdict` and
`outcome.escalation` for settlement, and `covered` and `reason` for landing.
Admission capability refusals, including exhausted budgets, record `denied`
with an error message. Gate execution errors record `failure` with an error
message. Audit status measures the call, not review acceptance or coverage;
certificates and landing records remain the authority for those outcomes.
Reset decisions are retained atomically in the lineage ledger alongside the normal
`orbit.task.review_reset` dispatch audit. Task artifacts
`review-manifest.json`, `review-report.json`, and `review-gate.json` are the
durable evidence.

## 9. Compatibility and rollback

Existing runs without a `review` snapshot behave exactly as before. The
seeded cron `code-review` auto-task and any custom definition stay untouched;
migrating to delivery-triggered review remains the explicit edit described in
[delivery automation operations](../automation-triggers/5_operations.md).
To roll back, set `review.before_pr = false` (and toggle the
`delivery-code-review` consumer on for after-landing review, §1): future
submissions capture the new value, admitted runs keep their gate, and
certificates, ledgers, and landings stay readable. An older binary cannot settle an
in-flight gate; drain gated runs with a supporting binary before downgrading.

## 10. Concerns & Honest Limitations

- Coverage is exact-tree only: a landing that is semantically identical but
  not byte-identical to the reviewed candidate stays an ordinary review
  obligation. Content-equivalence coverage is not implemented.
- Reviewer fixes are validated, not independently reviewed. An
  `accept_with_fixes` verdict carries the weaker assurance
  `independent_review_with_self_authored_repairs` and says so; owner
  revalidation checks the fixed head's ownership and required commands, not
  its intent.
- Provider token and cost caps are not enforced; only reviewer runtime is
  bounded, so reviewer spend stays unknown.
- `review.before_pr` has no meaning on the local-only delivery route and is refused
  at submission rather than downgraded.
- A denied required check is not evidence either way: it keeps its own
  `validation_unavailable` reason instead of counting as a failure.
- A failed `review_validate` leaves a certificate that says
  `accept_with_fixes` beside a blocked task. The handoff comment names the
  failure as `reject`; nothing rewrites the recorded certificate.
- Final recovery may `resume` a rejected run from a step of the failed
  phase. That is an operator-grade decision by the recovery crew, not a
  second review round the pipeline schedules.

## Task References

- [ORB-11333] — implements independent review policy, the before-PR gate, lineage budgets, and delivery coverage.
- [ORB-11528] — adds validation-record roles to the certificate contract.
- [ORB-11545] — tightens what a superseded validation record may claim.
- [ORB-12491] — retires epic assembly, the one caller that gated a combined candidate later.
- [ORB-13890] — closes failed attempts, keys budgets per delivery run lineage, adds gate retry/recovery, tolerant report reading, and the completion re-review.
- [ORB-13891] — added an implementer rework loop for `changes_required`; retired by [ORB-13989].
- [ORB-13989] — the reviewer fixes its findings as a second commit, comments them, and owner validation reruns on that head; retires the rework loop and repair-cycle budget.
- [ORB-13992] — splits review into the `review.before_pr` switch and the `delivery-code-review` auto-task flag, makes `review.minutes` the wall-clock limit of one review per candidate, and retires `operation.review_policy` and the reviewer-start budget.
- [ORB-13990] — the reviewer may change any path the repair requires; settlement and revalidation widen selectors with review provenance instead of downgrading or failing.
- [ORB-14192] — adds the `diagnostic` role, binds controls and diagnostics to scope-checked sources, and retains report revisions so a replacement cannot drop a required check.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
