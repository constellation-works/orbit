---
title: Review Gate — Design
owner: codex
last_updated: 2026-10-09
last_validated: 2026-10-05
status: Accepted
feature: review-gate
doc_role: design
type: design
summary: Shipped review contract — captured timing, the before-PR gate whose reviewer fixes its findings as a second commit, the before-landing review of the open PR, what validation records establish, lineage budgets, managed completion, delivery coverage, surfaces, and rollback.
tags: [review-gate, review-policy, automation, delivery, operations]
paths: ["crates/orbit-config/src/operation.rs", "crates/orbit-core/src/application/review/**", "crates/orbit-core/src/application/automation/after_landing.rs", "crates/orbit-store/src/driver/sqlite/review/**", "crates/orbit-automation/src/review/**", "crates/orbit-engine/src/executor/automation/vcs/review_gate.rs", "crates/orbit-store/src/repository/task/v2/artifacts.rs", "crates/orbit-store/src/repository/task/coordination/lifecycle/**"]
related_features: [automation-triggers, activity-job, auditability]
related_artifacts: [ORB-11333, ORB-11528, ORB-11545, ORB-13890, ORB-13896, ORB-13989, ORB-13992, ORB-14192, ORB-14849]
---

# Review Gate — Design [ORB-11333]

This file describes what shipped. A preference edit grants no authority and
changes no schedule; a verdict grants no merge permission.

## 1. Preferences: three timings [ORB-13992] [ORB-14849]

Automatic review runs at up to three timings. Before-PR review and
before-landing review are `config.toml` booleans; after-landing review is the
`delivery-code-review` auto-task's own `enabled` flag. Config preferences
resolve **built-in → global → workspace**, each with its winning layer
recorded. Unknown keys and out-of-range values fail config load.

| Key | Values (default) |
| --- | --- |
| `review.before_pr` | bool (`false`) |
| `review.before_landing` | bool (`false`) |
| `review.minutes` | 1..=1440 (30): wall-clock limit for one candidate's review |
| `operation.review_crew` | crew name (before-PR or before-landing reviewer; crew of after-landing review tasks) |

`review.before_pr` holds PR creation for a fresh reviewer (§3).
`review.before_landing` opens the PR first and reviews it while hosted CI
runs, so a clean review costs about max(review, CI) of wall time instead of
review + CI; the PR lands only at the head that review settled (§3.2). Both
need an explicit `review_crew`, and admission escalates
`review_crew_unconfigured` until one is set; both share `review.minutes`, and
each candidate gets one review (§5).

There is one review layer before landing. A resolution with both
`review.before_pr` and `review.before_landing` on fails config load with an
error naming both keys and the layer that set each, and `orbit config set`
refuses an edit that would turn both on in one file. After-landing review is
independent of either and may run beside them.

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
every switch, each with its source: before-PR on/off with its minutes and
crew, before-landing on/off (sharing them), and after-landing enabled with
when the next batch is due. While the
after-landing consumer is enabled the view adds its health: whether it is
present, whether this host owns it, whether it is wedged on a closed action
or stalled, whether its branch and review crew resolve, its scheduling state,
and when its last batch was minted or covered. Anything short of healthy — the
definition missing, owned by another machine or by none, wedged, stalled, held
for an operator (`definition_changed` for an edit the evaluator would not adopt
automatically, which the row names; `needs_attention`;
`retry_deadline_expired`), on a branch that does not resolve, or naming a crew
that does not — is an `error`, so `orbit doctor` exits nonzero. So is
before-PR or before-landing review switched on without a resolvable
`review_crew`, or on a workspace whose automatic delivery ships locally (§2).

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
reviewer crew and its source, the lineage budget, the policy version, the
owner's `workflow.required_validation_commands` list, and its
`review.baseline_commands` list (§4). A
parent-authorized child inherits its parent's snapshot exactly; any other submission resolves from the
workspace preferences at that moment. A distributed drain
(`workspace_pull_pipeline`) captures the same snapshot, so the `before_pr` it
declares to owners is the value it was submitted with. Ordinary input naming
the reserved `review` key is refused, and a resume keeps its persisted input,
so switching `review.before_pr` off never weakens a gate that is already
active and switching it on never gates a run already admitted. The captured
timing is `before-pr`, `before-landing` [ORB-14849] or `none`, with the source
of the switch that decided it. A run captured under the retired
`after-landing` policy value reads as not gated.
`review.before_pr` is refused at submission for `task_local_pipeline` delivery,
with no exemption: epic assembly was the one caller that gated a combined
candidate later, and it is retired [ORB-12491]. `review.before_landing` is
refused there the same way, since a local delivery opens no pull request to
review. Readiness and the drain's wave hold such backlog work before dispatch
(`local_route_before_pr`, `local_route_before_landing`), and doctor fails its
`review` check while either switch is on for a workspace that ships locally.

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
admitted, and its `review.baseline_commands` (empty when the admission
predates that snapshot). Each required command must appear as a required
passing record in the review; settlement never consults a later mutable
config value. A legacy admission
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
with its own instruction, tool allowlist, and a wall clock set to the
deadline the candidate's `review.minutes` allows this invocation (§5); the
activity's 60-minute `wall_clock_timeout_seconds` applies only when the host
does not bound reviews. It runs under the same
sandbox as the implementer, with write access to the worktree. It reads the
manifest, verifies claims against code, fixes concrete defects directly in
the worktree — any path a fix requires, each listed on the finding it fixes
— runs validation, and persists `review-report.json` (schema version 1:
verdict, findings with dispositions, the paths and a `change` description
for each fix, validation records with `passed` / `failed` / `denied` /
`not_run`, the `role` each is evidence of, a stable record `id` and optional
`check` identity, `retired_validation` entries, escalation). It never runs
Git writes, changes task lifecycle, approves, or merges.

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

The shared review contract reserves the canonical `review-*` artifact
namespace, ignoring ASCII case, for the gate. Core's task update path checks names
after task-path normalization and accepts gate artifacts only from its
trusted system writer; actor labels and tool input cannot grant that authority.
This covers local puts, task updates, CLI and dashboard writes. The exception
is the exact `review-report.json` name: a live reviewer may attach a report
that passes the shared version and attempt validator, with its
revision retained by the store. The claimed-worker broker uses the same
namespace classifier and additionally requires the running reviewer's admitted
attempt for report writes. External evidence cannot name this reserved namespace.

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
| `incomplete` | The review could not establish the candidate | kept if made | Named external checks enter an awaiting-evidence hold; a wall-clock timeout permits one automatic requeue per task/implementation tree, then blocks; other escalations block |

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
- Required checks retained from earlier report revisions: <id, command and outcome, with any retirement reason, or none>
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
### 3.2 Before-landing review of the open PR [ORB-14849]

With `before-landing` captured, the pre-push gate reports `applies: false`
(`review_before_landing`) and push, `pr_open` and promotion run as they do
without review. Between promotion and `complete_pr`, `task_pr_pipeline` runs
the same activities again, while hosted CI runs on the published head:

- `landing_review_gate_admit` (`review_gate_admit` with `before_landing`):
  the same lineage, budget, crew, manifest and host-evidence rules. It
  applies only to a `before-landing` admission, and for a checked no-diff
  exemption it does not apply.
- `landing_review` (`agent_review_repair`): one fresh reviewer, given the
  published head as its candidate; it fixes what it finds as one reviewer
  commit (§3.1).
- `landing_review_gate_settle`: the verdict, certificate and findings
  comment.
- With a reviewer commit, `landing_review_validate` reruns owner validation
  and the ownership check on it, and `landing_push` pushes it to the PR
  branch under a lease on the published head (`lease_remote_sha`): the remote
  branch must still be at that head, and the push is conditional on it, else
  the step fails `push_lease_lost`. The new head restarts hosted CI.

These steps run whatever `completion` is, so a review-only run hands off a
reviewed PR. `complete_pr` receives the settle's head as
`landing_reviewed_head_sha` and merges only that head (§6); after a reviewer
fix that head is also the published one. A later DIRTY rebase of it routes
through the `re_review*` steps exactly as a before-PR review's does.

Every step here is completion-stage. Any outcome other than an approve —
a `reject` or `incomplete` verdict, a reviewer timeout, a lost lease, a
failed revalidation or a settle error — fails the run with the PR open and
unmerged, and the failure handoff keeps the task in `review` with a comment
naming the step's typed reason (`review_gate_blocked`,
`review_timeout_incomplete`, `push_lease_lost`, ...) and decision
`landing_review_failure`. No outcome closes the PR. Because the PR is
already published and the task promoted, an evidence-only `incomplete` is
not held in progress (§4): it ends like any other `incomplete`. Final
recovery still gets one look, as for a failed re-review.

**Claimed leaves: the review runs on the leaf.** A claim from an owner with
`review.before_landing` on captures `before_landing` and the owner's review
contract in its ship contract, and the owner admits only an executor whose
leaf declares the review gate on the PR route (`before_pr_unsupported`
otherwise). `task_claimed_pr_pipeline` runs the same three gate steps after
`pr_open`, under the same crew, budget and host-evidence rules as a claimed
before-PR review. `landing_review_validate` (`claim_validate` with `carry`)
returns the pre-publication validation unchanged unless the reviewer
committed a fix; then the required commands run on the new, still
unpublished head, `landing_push` pushes it under the lease, and
`pin_validation` pins that head. `claim_handoff` carries the settled verdict
as `landing_review_evidence` (disposition `before_landing`). The owner judges
it against the claim's captured timing: a handoff without it, with a
before-PR disposition in its place, or for a head other than the one the
review settled is refused at acceptance (`review_evidence_missing`,
`reviewed_head_mismatch`), and `handoff_land` rechecks the pinned evidence
and merges only the handed-off head, so a claimed PR never lands unreviewed.
The review runs on the leaf because the leaf owns the worktree and the
published branch; an owner-side review before `handoff_land` would have to
fetch the PR head into an owner workspace and push fixes to a branch the
leaf published. A leaf whose review does not approve fails with its PR open
and unmerged; its failure settlement blocks the task on the owner.

## 4. What the validation records establish [ORB-11528] [ORB-11545]

An honest reviewer records more than the checks that had to pass, so each
validation record also carries a `role` saying what it is evidence of, and
`orbit_automation::review::validation_evidence` decides what the set
establishes. Settlement and delivery coverage both read that one function, so
a certificate cannot mean one thing when it is issued and another when it is
spent. It also requires every command in the immutable owner snapshot to be
present as `required` and `passed`, or to be a valid superseded record with
a passing required check of the same effective identity anywhere in the report.
A record's effective
identity is its non-empty, trimmed `check`, falling back to its normalized
command. Explicit identities take precedence even when command text matches.
Without a shared record id, a superseded `cargo test -p x` attempt with
`check: "unit"` is therefore not replaced by a required pass of the identical
command that omits `check`: their effective identities are `unit` and
`cargo test -p x`. Preserve `check: "unit"` on the replacement to link them.
The same refusal applies when only the replacement carries that identity.
An explicit identity equal to the other record's normalized command still
matches; omitting `check` is safe only when that effective identity stays equal.
This also applies to retained obligations across report revisions: an earlier
`not_run` record with command `make ci-fast` and no `check` is satisfied by a
passed record with `check: "make ci-fast"` and a shell-wrapped command, and the
reverse order also matches [ORB-14312]. Command
identity ignores whitespace and leading `NAME=value` assignments, so a required
`TMPDIR="$PWD/.orbit/tmp" make ci-fast` pass establishes host-required
`make ci-fast`. `make ci-fast-extra` and `FOO=1 make other` do not. A
diagnostic, exclusion, negative control, omission, or unrelated pass
cannot satisfy a host-required command. Certificates retain that snapshot;
legacy certificates without it cannot be spent as coverage.

Diagnostic contradictions use the same identity rule. A failed diagnostic
with `check: "unit"` and a required pass of the same command without `check`
are different checks, so command equality alone does not trigger
`CheckContradicted`. The diagnostic must still explain itself and name failure
sources entirely outside the candidate's scope; an in-scope failure remains
invalid regardless of identity.

| `role` | Meaning | Passing requires |
| --- | --- | --- |
| `required` (default) | A check the final candidate must pass | `passed`; `failed`, `denied`, and `not_run` all block |
| `expected_failure` | A negative control — the superseded assertion, the pre-fix reproduction | `failed`; any other outcome contradicts the claim |
| `excluded` | An action outside the authorized scope, deliberately not performed | `not_run` or `denied`; actually running it contradicts the exclusion |
| `superseded` | A diagnostic attempt a required check on the final candidate replaced | a record anywhere in the report that is `required` and `passed` and has the same effective identity: its non-empty, trimmed `check`, otherwise its `command` with whitespace and leading `NAME=value` assignments normalized. An explicit identity can match another record's normalized command. Different effective identities, or a same-identity check that did not pass, are not a replacement |
| `diagnostic` [ORB-14192] | A nonrequired observation of the final candidate, such as a workspace-wide suite beyond the task's checks | `passed` or `failed` as observed (`not_run`/`denied` contradict it: an action never taken is `excluded`). A failed diagnostic lists `sources`, every one outside the candidate's scope, and shares no check with a required pass. A failed check the owner trusts — a captured `workflow.required_validation_commands` or `review.baseline_commands` entry, by the host-command identity rule — is never a diagnostic [ORB-14684]. It supplies no coverage and creates no requirement |

Required passes describe the final candidate, so replacement does not depend on
array position [ORB-14322]. Reviewers should still list superseded attempts
before their replacements so the report reads chronologically. Identity stays
strict: a broader check such as "runtime tests and formatting" cannot replace
"runtime tests", even with an identical command. Coverage or command-superset
claims do not establish a replacement relationship.

A pass can still leave a path unexecuted [ORB-14334]. Inside an agent lane a
test of Orbit's own sandbox cannot apply a nested sandbox, so it prints a
`DEFERRED: …` notice past libtest's output capture and returns. The run still
reports success. The reviewer lists each such line in the record's `deferred`.
A record that passed with a nonblank `deferred` reads as `not_run` wherever a
pass counts: it is no required pass (`validation_incomplete`, naming the
notice), it replaces no superseded attempt, it establishes no host-required
command, and delivery coverage does not count it. The reviewer names the
deferred test as `host_sandbox_test` evidence instead. A result from a host
that executed the path, such as owner fulfilment below, stands in for the
record and clears its notices.

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

A counterfactual names the files it temporarily mutated in `mutation_target`,
apart from its `sources` [ORB-14616]. The mutated file is usually the
production code the candidate's tests guard, so for a test-only change it is
always outside the scope: ORB-14521's reviewer proved a repaired test by
deleting a conjunct from an untouched production file, and naming that file
in `sources` settled a correct candidate `incomplete`. `sources` still names
the candidate's checks that rejected the mutation, every one inside the
scope. A mutation target may lie anywhere in the repository, but settlement
confirms each one came back byte-identical: it compares the target's path
between the reviewed and the final candidate with rename detection off
[ORB-14632], so a target left modified, deleted or moved away is caught
however the reviewer commit records it. A target left changed settles
`incomplete` with `validation_contradicted` naming the file, whatever the
verdict claimed. A target that is not a repository-relative path (absolute,
home-relative, climbing out of the repository, inside `.git`, or a `dir:` or
`symbol:` selector) cannot be compared and is refused as
`validation_unevidenced`, never passed for want of a match.
Coverage does not re-judge mutation targets: a certificate that passed
settlement had them restored.

A defect in a record's shape rather than in what its checks observed — a
missing `note`, `control` or `sources`, a source outside the scope such as
an old-shape counterfactual's mutated file, or a mutation target that is not
a repository-relative path, or a diagnostic recorded `not_run` — goes back to the reviewer once
before the verdict settles [ORB-14616]. A `not_run` diagnostic is a skipped check
under the wrong role [ORB-15083], and the correction names the fix for its
class: a `workflow.required_validation_commands` entry is run and recorded
`required` (`excluded` never establishes one); a `review.baseline_commands`
entry is recorded `excluded` or omitted; any other command, such as policy-required
local Rust CodeQL, is run or named in `external_evidence` for the owner to
fulfil, and is never recorded `excluded`, which would drop the coverage. The
validator cannot tell an unlisted policy check from an optional one, so that
rule is contract text only. A report that still carries the `not_run`
diagnostic after the correction settles `incomplete` with
`validation_contradicted`; a denied diagnostic or a control that never ran
stays an observation. When the reviewer step returns
successfully, the engine asks the host (`RuntimeHost::review_report_correction`)
to judge the attached report as settlement would, over the scope settlement
would derive (the task selectors plus the implementation's paths and the
reviewer's uncommitted ones), without committing or settling anything. Only a
passing verdict whose first defect is one of those shape defects is returned.
The engine then dispatches the reviewer once more in the same step, attempt
and lineage, with the typed defect as its `report_correction` input; the
reviewer corrects the report and puts it again, and settlement judges
whatever it then says. Both invocations are charged to the attempt, and no
minutes left means no correction. A required check that failed, a diagnostic
inside the scope, or a mutation target left changed are observations, not
shapes, and go straight to settlement.

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

Required records carry a non-empty stable `id` from their first submission
across revisions [ORB-14370]. The report put refuses a report whose required
records lack ids, naming each with a free id to give it, or whose distinct
records reuse an id. Deploy and land are not atomic, so a reviewer still
following the instructions from before ids resubmits with the named ids in
the same attempt; its earlier id-less revisions are matched by command
identity. One `superseded` attempt and its required replacement may share an id. Matching
an earlier record by command text could not converge: reviewers legitimately
rename, rewrap and concretise commands between revisions, and seven correct
candidates in one shift settled `incomplete` that way (ORB-14360 recorded the
prose name "focused CLI reference invocation verification" `not_run`, then
`python3 .orbit/tmp/verify-reference-examples.py` `passed`; ORB-14260 replaced
a `<workspace>` placeholder with the absolute path). The reviewer gives each
`required` record an id such as `V1` when it first files it. An earlier
required record with an id is accounted for by that id alone, never by
command text: a later record carrying the id as `required` or `superseded`
(whose replacing required pass may share the id), or as `excluded` when the
earlier record never ran, holds the check's current command and outcome. A
check that no longer applies may instead be retired in the report's
`retired_validation` list as `{id, reason}`, restated in every later revision;
a retirement needs a non-empty reason and never clears a record that failed,
which must be rerun under its id. The artifact store applies the same rule when
a report is attached, against the attempt's retained revisions under the task
lock: a revision that omits an earlier id, carries it only as `diagnostic` or
`expected_failure`, or retires it improperly is refused with the record's id,
command and outcome named, so the reviewer corrects the report in-session
instead of the gap surfacing at settlement, when nobody can fix it. The
refused bytes neither replace the report nor enter the history. A claimed
reviewer's put reaches the owner as a live worker update and is refused the
same way. The evidence or failure a claim settles runs after the reviewer
stopped, so it retains the revision instead of discarding the rest of the
evidence. The history marks whether its record ids were checked while the
reviewer could still correct the report. Settlement checks an unchecked
post-session revision before accepting it: a new report still needs distinct
required record ids and must account for earlier ids. A revision already
stored before this marker existed stays on the legacy command-identity rules.
The certificate keeps `retired_validation`, coverage re-derives the
rule from it, and the verdict comment shows each retained record's id with its
retirement reason. Records and certificates written without ids keep the
command-identity rules above unchanged, so they settle and spend exactly as
they did before.

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

A failed required check whose failures lie outside the candidate's scope may
also be the pinned base's own: the integration branch has no merge gates
[ORB-14434]. The reviewer then attaches a `baseline` claim to the failed
`required` record (the `base_commit` it reran the check on, which must be the
manifest's `base.commit`, the `outcome` there and the shared `failures`), with
`sources` outside the scope. Settlement never runs the reviewer's command
text on the host. It reruns a matching `workflow.required_validation_commands`
or `review.baseline_commands` entry on the final candidate and, through the
same per-base cache delivery validation uses, on the base. It believes the
claim only when the exit status and timeout outcome match, every claimed
failure appears in the base output, and the candidate's failed tests and
located errors are a subset of the base's. When every failed required check
is confirmed, no finding is open, no external evidence is pending, and the
records are consistent once those checks count as passed, the certificate
records the holds in `baseline_red`. The step then fails typed
`[baseline_red]`, so the failure handoff keeps the candidate and holds the
task under `baseline_red_hold` until the base passes. The certificate's
verdict stays what the reviewer reported and is never coverage. A candidate
that adds failures keeps its verdict (`baseline_exceeded`). Once the host
refutes the claim, its run on the final candidate decides the check
[ORB-15122]. A pass there, with no open finding and no pending external
evidence, counts the record as passed on the host's run, recorded in the
certificate's `host_overrides`, and a review whose claims all resolve that
way settles `accept` (`accept_with_fixes` over a repair). A pass whose
validation summary shows no counted test ran is not evidence. A failure
there while the base passes a comparable run is the candidate's own and
settles `reject` (`baseline_refuted`). Any other claim the host contradicts
or cannot check, including a pass under an open finding, settles
`incomplete` (`baseline_claim_refused`). A base run that is not comparable
with the candidate's (it tested another selection, or passed without
executing a counted test) neither refutes nor confirms the claim and
settles `incomplete` (`baseline_not_comparable`) [ORB-15131]. Every outcome
but the host pass blocks as before.

Listing a command in `review.baseline_commands` also makes it binding
[ORB-14684]. A failed record of a trusted command (a captured
`workflow.required_validation_commands` or `review.baseline_commands`
entry, matched like the host-required checks) can no longer be filed as a
`diagnostic` on the reviewer's own sources: it passes, or it stays a failed
`required` record whose baseline claim settlement reproduces on the pinned
base, or the review settles `incomplete` with a reason naming the command.
A command the owner does not list stays a legitimate diagnostic. Both lists
come from the run's admission snapshot (§2), the claim contract for a
claimed leaf, so the commands settlement reruns are exactly the commands a
failed diagnostic may not name. The certificate records the baseline list
in `baseline_commands`, coverage re-derives the rule from it, and an owner
accepts a claimed handoff only when that list equals the one the claim
captured. A certificate or admission written before the snapshot reads as
an empty list, so it is judged exactly as when it was issued.

The contract version stays 1: a record carrying no role decides exactly as it
did before, so older role-less evidence is not reinterpreted. A superseded
attempt requires a required pass that names the same check, in either report
order; a certificate that treated an unrelated pass as a replacement becomes
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
failure permits one automatic requeue per task and implementation tree,
recording `review_timeout_incomplete`, `candidate_tree` and
`implementation_tree` in durable task history. The implementation tree is the
candidate head's tree after stepping back over the gate's own partial-repair
commits, so the reviewer's partial work never renews the allowance. Another
timeout on that tree blocks with `review_timeout_requeue_exhausted` and the
exhausted bound in its reason. The bound has no time window and survives fresh
run lineages, rewritten commits, reviewer partial repairs, and changing then
restoring the tree. An implementer change to the tree has its own allowance.
History rows written before `implementation_tree` was recorded are matched on
their `candidate_tree`.
The handoff does not retry the reviewer within the failing step. The partial
report stays attached. Within the same lineage, admission resumes the latest
released attempt for the same candidate and task meaning with the remaining
budget; a fresh drain run starts a new lineage with its captured budget. A
newly admitted candidate receives the earlier report in the manifest's
`previous_report`, as advisory context requiring revalidation. Finalization
preserves the timeout requeue or exhausted-bound block.

For a single-task delivery, an otherwise complete nonpassing report may name
`external_evidence`: a list
of `{kind, name, command, artifact}` requirements, where `kind` is `hosted_ci`,
`native_os`, `codeql`, or `host_sandbox_test`. A `host_sandbox_test` also
names the `os` it needs (`linux` or `macos`); its result counts only with that
`os`, and settlement refuses one without it. Each exact command must have an unavailable required
record (`not_run` or `denied`). All missing checks must be named, all other
validation must be consistent, and no substantive finding may remain open.
The typed requirements classify the hold even when an older reviewer spelled
its evidence-only verdict `changes_required` / `reject`; the certificate retains
that reported verdict and never qualifies as passing coverage. Open defects,
failed checks, dropped earlier obligations, and host-detected candidate or
meaning drift never enter this hold.

Settlement retains its nonpassing certificate and writes
`review-evidence-hold.json`, pinning the attempt, candidate revision, task
meaning, and requirements. Settlement records `review_awaiting_evidence` and
ends the run `held`; no failure handoff runs. Run finalization preserves this
decision. Admission refuses to continue while the matching hold has missing
evidence; that refusal reaches the failure handoff, which keeps the task in
progress and its candidate recoverable, and holds publication.
For each requirement, an accepted writer (below) attaches its result at its
named artifact path, plus the separately referenced nonempty log artifact. An
operator attaches both with `orbit task artifact put`:

```json
{"schema_version":1,"attempt_id":"<held attempt>",
 "candidate":{"commit":"<held commit>","tree":"<held tree>"},
 "kind":"hosted_ci","name":"Windows CI job","command":"<exact command>",
 "outcome":"passed","log_artifact":"evidence/windows-log.json"}
```

Evidence counts only from the writer class Orbit stamps on the artifact's
manifest entry (`writer`) from the write path, never from `created_by` or
tool input. The result and its log must both come from a class accepted for
the requirement's kind:

| Kind | Accepted writers |
| --- | --- |
| `hosted_ci` | `operator` |
| `native_os` | `operator` |
| `codeql` | `operator`, or `system` (owner fulfilment, below) |
| `host_sandbox_test` | `operator`, `system` (Linux owner fulfilment, below), or the claimed host's own settlement (below) |

An `operator` write is a human actor on the bare CLI or the dashboard with no
agent identity, whose process declares no agent envelope or managed run. A
`system` write is Orbit's own deterministic machinery. An agent's
`orbit.task.artifact.put`, a claimed worker's evidence, and an artifact stored
before writer classes have no class and never count. So the agent whose
candidate is held cannot satisfy the check that holds it, including an
implementer that attached a result for its own tree before review. The rule
holds everywhere evidence is read: hold release, the admission manifest's
`satisfied_external_evidence`, evidence carried across a rebase, and
settlement. An agent that re-puts an accepted result replaces its writer, so
the result stops counting.

Evidence identity is kind, exact command and candidate tree. Attempt, commit,
display name and artifact path are provenance or locators, so a new commit on
the same tree does not expire a passing result. Evidence for another tree,
an unrelated check, a failed result or a missing/empty log does not count.
After all results and logs arrive, an unchanged task still owned by that hold's
run moves to backlog with `review_evidence_received`. This queues fresh review,
not acceptance: admission supplies verified result/log pairs in the manifest's
`satisfied_external_evidence`. The reviewer records these checks as passed for
the unchanged tree. Settlement re-reads the artifacts against the final tree
and resolves any repeated unavailable requirement, including a renamed result
path, before judging the verdict. An otherwise complete evidence-only report
settles as passing once all its requirements are satisfied. Reviewer repairs
that change the tree require new evidence. The next delivery run resumes the
held candidate rather than implementing again (`resumed_held`). When that
resume or a completion rebase moves the candidate onto a new base, the
evidence still counts while `git patch-id --stable` of the whole base..head
change is unchanged, recorded as `evidence_carried` on the manifest and
certificate; a changed patch re-requests it with a typed reason. Every publication and validation
gate still applies. An operator block or later review decision
is never undone by a late artifact attachment.
Passing grants no lifecycle transition; `completion: review` still stops at
the handoff.

A claimed leaf's hold reaches the owner. Settlement pushes the held candidate
to `orbit-evidence/<branch>` on `origin`, apart from the delivery branch. The
leaf's settlement then releases the claim with the hold, so the owner keeps
the task in progress under `review_awaiting_evidence`; it does not block it.
A hold whose every requirement is `codeql` or a `linux` `host_sandbox_test`
is fulfilled by a Linux owner whose host can create Bubblewrap namespaces
(`review_evidence_fulfilment_pipeline`, one run at a time). The tick stands
down on another platform, a follower or worker, or a host without namespaces.
The run first admits every command against its kind's allowlist:

- `codeql`: only `scripts/codeql-rust-local.sh` with its own options and one
  query selector;
- `host_sandbox_test` [ORB-14334]: the same grammar as a claimed leaf's
  settlement below, an exact owner-required command or
  `cargo test -p <crate> --test <target> [<filter>]` using only
  `[A-Za-z0-9._/:@+=-]` and spaces.

One command outside its allowlist refuses the whole hold with a typed reason
(`command_not_allowed`, `shell_metacharacter`, `argument_not_allowed`) before
any host gate, so nothing runs and only the refusal's log is attached. The run
then re-checks that the hold is current, gates on free disk, and runs each
command without a shell at the fetched held commit. CodeQL runs in a
standalone shallow checkout holding its own Git metadata, confined by
Bubblewrap. A sandbox test runs in a detached worktree with the validation
environment and a run-local build target, both removed afterwards, outside any
sandbox: the namespaces it tests cannot nest inside another Bubblewrap
sandbox. It is judged as a claimed leaf's run is (below). A complete CodeQL
run with an empty SARIF, or a sandbox test that executed and passed, attaches
the results above as `system`, with `os` for a test; anything else attaches
only the log, and the hold stays with a typed reason (`self_skipped`,
`no_tests_ran`, `test_failed`, `sandbox_unavailable`, …). Every attempt is
audited as `review.evidence_fulfilment` and commented on the task.
The owner already runs its required validation on foreign heads under the
same host trust and with no sandbox, so the fulfilment is no new trust in the
candidate's own script or tests. Details are in the
[CodeQL runbook](../../runbooks/codeql-local.md#owner-fulfilment) and the
[review gate runbook](../../runbooks/review-gate.md).

A claimed leaf fulfils its own `host_sandbox_test` requirements before it
holds. Every agent lane runs inside Orbit's sandbox, where a nested one cannot
apply: macOS `sandbox_apply` fails with `EPERM`, and Bubblewrap cannot create
its namespaces. So the reviewer cannot run Orbit's sandbox tests, nor an
owner-required command that runs them. Settlement on the leaf's host is
Orbit's deterministic worker, outside the agent sandbox. When the report would
otherwise be an evidence-only hold, it runs each requirement whose `os` is this
host's and that no accepted result already satisfies. The command must be
either an exact owner-required validation command or
`cargo test -p <crate> --test <target> [<filter>]`. That form takes no other
option, and the command may use only `[A-Za-z0-9._/:@+=-]` and spaces. A
command that fails these checks, or names another OS, is refused with a typed
reason (`shell_metacharacter`, `command_not_allowed`, `argument_not_allowed`,
`os_mismatch`) and never runs. An admitted command first has its candidate
checked against the held tree (`candidate_changed`). It then runs in a fresh
detached worktree of the final candidate with the validation environment,
sharing the checkout's target directory, and with `ORBIT_REQUIRE_SANDBOX_EXEC=1`
on macOS.

Its log is always attached. A run counts only if it exits successfully, prints
no sandbox-unavailable diagnostic, prints no skip notice (`SKIP:`, `DEFERRED:`,
`skipping`), and, for the `cargo test` form, passes at least one test. Failing
those checks leaves the evidence missing with `sandbox_unavailable`,
`self_skipped`, `no_tests_ran`, `tool_missing`, `timed_out` or `run_failed`, so
the review holds as above. A failing test (`test_failed`) fails the reviewer's
required record, and the review blocks. A passing run attaches a result like
the operator's, with `os`, and settlement counts it on this tree, so the review
passes with no hold. The certificate's `host_evidence` lists every attempt.

These results carry no writer class the owner accepts, so they count only in
the settlement that ran them. A passed verdict's handoff pins each passing
result and its log under `host_evidence`. The owner reads both by digest and
accepts the handoff only if all of the following hold: each passing record is
pinned and no other ref is; the log is nonempty; the result is a passed
`host_sandbox_test` for the record's command and OS on the head tree; and the
certificate holds a passed required record for that command. A held
`linux` `host_sandbox_test` that the leaf's host could not fulfil, or that a
local review deferred, is fulfilled by a Linux owner as above; a `macos` one
waits for an operator's result.

A reviewer may forget to name owed evidence, or report a check as passed
when its host could not have run it. To close that gap, the workspace
declares what a claimed leaf owes, and Orbit derives the requirement instead of
reading it from the report. Each `[[review.host_evidence]]` rule
([config](../../CONFIG.md#owed-host-evidence)) names a `kind` (`codeql` or
`host_sandbox_test`), a display `name`, workspace-relative `paths` globs, an
`os`, the exact `command` and the result `artifact`. The owner captures the
rules in the review contract each claim carries. A rule is owed when a claimed
leaf's candidate changes a matching path and one of these holds:

- a `codeql` rule runs on a host of another OS, and the owner fulfils it on
  `os`;
- a `host_sandbox_test` rule runs on a host of that `os`, which runs it
  outside the agent sandbox at settlement, as above.

A local run owes nothing, because its host is the owner.

Admission lists owed requirements in the reviewer's manifest
(`owed_external_evidence`) and its own output (`owed_evidence`). The reviewer records each one as
a `required` `not_run` check, never attempts it, and copies it into
`external_evidence`. Settlement adds each owed requirement whatever the report
says:

- it replaces a reviewer requirement for the same check;
- it resets the check's required record to `not_run`;
- it excludes a reviewer record that ran the same CodeQL program with another
  command;
- it turns a passing verdict `incomplete`, with an `owed_evidence` escalation.

So a verdict whose only gaps are owed checks holds for them, while an open
finding or any other gap blocks as before. A reviewer that skipped or claimed
an owed check cannot ship the candidate unverified. The certificate records
the requirements in `owed_evidence`.

When owed evidence arrives, a fresh reviewer is not needed. Receipt moves the
task to backlog with `review_evidence_received`. The owner's own next run then
resumes the published held candidate without the implementer (`resumed_held`).
Its admission decides `evidence_received` when all of these hold:

- the hold names only checks its certificate recorded as owed;
- every one of them has arrived;
- the certificate made no repair commit;
- the rebuilt candidate has the held tree on the held base, under the same
  task meaning.

On that decision the review step is skipped. Settlement takes the findings,
records and verdict from the held certificate, which `resumed_hold_attempt`
names, and the arrived evidence passes it. Every publication and validation
gate still applies. A different tree or base, a reviewer repair, or other
evidence sends the candidate to a fresh review as above.

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
reviewer invocation records its start, and the ledger sets its deadline to
the seconds the candidate has left; the engine sets the reviewer's wall clock
to that deadline, longer or shorter than the activity's own; the manifest's
`remaining.seconds` advertises that same deadline. A reviewer that runs to its
deadline spends the review's minutes; a continuation exists only for an
invocation that ended earlier with time left. The engine refuses the
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

After a before-landing review (§3.2) `complete_pr` also receives
`landing_reviewed_head_sha`. When set, that head is the reviewed head the PR
must report and the `sha` the merge sends; when a reviewer fix moved the PR
there, it is the published head and the head it replaced the previous one.
A head pushed after the review settled is therefore refused before any merge.

A conflicting reviewed PR is never merged as rebased, unreviewed content
[ORB-13890]. `complete_pr` runs with `re_review_on_conflict`: it rebases the
branch locally through the pinned `git_rebase` (a real conflict still reaches
`pr_conflict_recovery`), publishes nothing, completes no task, and returns
`re_review_required` with the rebase checkpoint. The pipeline then reviews the
rebased head as a new review in the same lineage (`re_review_gate_admit`,
`re_review`, `re_review_gate_settle`, and `re_review_validate` when that
reviewer committed fixes), republishes the reviewed final candidate under a
lease on the old published head (`re_push`), and completes it
(`complete_reviewed_pr`). The base can move again under that re-review
[ORB-14332], so `complete_reviewed_pr` also runs with `re_review_on_conflict`
and `pr_conflict_recovery`, and one more round of the same steps
(`re_review_gate_admit_2` with `re_review_after: complete_reviewed_pr`,
`re_review_2`, `re_review_gate_settle_2`, `re_review_validate_2`, `re_push_2`,
`complete_reviewed_pr_2`) reviews, republishes and completes the head it
rebased. A conflict at `complete_reviewed_pr_2`, a caller without the flag, a
non-accept re-review, or a failed revalidation keeps the published PR and the
task in review. When the base advances again between a step's conflict
recovery and its retry, the retry carries the certified recovered head onto
the new base, or recovers a conflicting advance once more, before handing the
head to re-review; a step that has already followed its base twice fails as
`base_chase_exhausted` [ORB-14393].
`complete_pr` is skipped on review-only and no-diff routes, and a `when:` may
not read a skippable step's output, so `re_review_gate_admit` and
`re_review_gate_settle` always run: with `re_review_after: complete_pr` the
admission reads that step's checkpoint from the run's recorded pipeline (a
resume inherits it), answers `re_review_not_required` unless it asked for a
re-review, pins the recorded rebase base, and fails when the worktree head is
not the recorded rebased head. Its `applies` gates the remaining steps of
its round; the second round reads `complete_reviewed_pr` the same way.

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
`orbit.drain.probe` report every review switch with its source (§1); doctor
prints a before-landing line beside the before-PR one. `orbit task show --json`, the task API, and the
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
To roll back, set `review.before_pr = false` or `review.before_landing =
false` (and toggle the
`delivery-code-review` consumer on for after-landing review, §1): future
submissions capture the new value, admitted runs keep their gate, and
certificates, ledgers, and landings stay readable. An older binary cannot settle an
in-flight gate; drain gated runs with a supporting binary before downgrading.
External evidence attached before writer classes has none, so an in-flight
hold waits for an accepted writer to attach it again; owner fulfilment does
so for a `codeql` or `linux` `host_sandbox_test` hold on its next tick.

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
- `review.before_pr` and `review.before_landing` have no meaning on the
  local-only delivery route and are refused at submission rather than
  downgraded.
- A before-landing reviewer's fixes reach the open PR as a commit, not as a
  "Review fixes" section in its body: the body was written at `pr_open`. The
  findings comment on the task carries them.
- A before-landing evidence-only gap is not held for its evidence: the PR is
  already published, so it leaves the PR open in review like any other
  `incomplete`.
- The resolved-policy version is 5 with `review.before_landing`, and the
  distributed pull request schema changed with the ship contract's
  `before_landing`: owner and followers must run matching builds.
- A denied required check is not evidence either way: it keeps its own
  `validation_unavailable` reason instead of counting as a failure.
- A failed `review_validate` leaves a certificate that says
  `accept_with_fixes` beside a blocked task. The handoff comment names the
  failure as `reject`; nothing rewrites the recorded certificate.
- The `operator` writer class rests on the same process identity as other
  operator decisions: an agent envelope or managed-run marker in the
  environment excludes it, but the environment is not authentication. A
  process that removed both and could still write the task store directly
  would be classed as an operator.
- An owner-fulfilled `host_sandbox_test` runs the candidate's tests with no
  sandbox around them, because Orbit's sandbox tests cannot run inside one.
  The allowlist bounds the command, not what the candidate's test code does;
  the trust is the same as required validation's.
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
- [ORB-14370] — gives required validation records a stable `id` and `retired_validation`, compares retained obligations by id, and refuses a dropping revision at attach.
- [ORB-14192] — adds the `diagnostic` role, binds controls and diagnostics to scope-checked sources, and retains report revisions so a replacement cannot drop a required check.
- [ORB-14434] — a reviewer's host-verified claim that a failed required check fails the same way on the pinned base holds the task for the red base instead of blocking it.
- [ORB-14334] — a Linux owner fulfils held `host_sandbox_test` evidence that nested agent sandboxes cannot produce, and a pass that deferred its sandbox-confined path no longer counts as executing it.
- [ORB-14849] — adds `review.before_landing`: the reviewer reviews the open PR while hosted CI runs, completion merges only the head it settled, any other outcome keeps the PR open in review, and a claimed leaf reviews its PR before handing it off.
- [ORB-14684] — a failed check the owner trusts (`workflow.required_validation_commands` or `review.baseline_commands`) can no longer be filed as a `diagnostic`; the baseline list is captured with the admission and recorded on the certificate.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
