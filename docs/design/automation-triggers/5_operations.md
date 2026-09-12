---
type: design
summary: "Delivery automation operations [ORB-11330]"
tags: [automation-triggers]
last_validated: 2026-09-12
---

# Delivery automation operations [ORB-11330]

Delivery triggers are opt-in. The host clock tick evaluates routines and auto-task
definitions in-process. Routines submit ordinary jobs, and auto-tasks create ordinary backlog tasks for the normal
approval/admission lifecycle. There is no new daemon or coverage submission tool.

## Configuration and migration

Existing `schemaVersion: 1` cron and `every_minutes` definitions retain their
behavior. Existing `code-review` and `qa-sweep` defaults are unchanged. New
`delivery-code-review` and `delivery-qa` defaults ship disabled, and initialization
does not overwrite existing workspace definitions.

Use one schedule form. A delivery auto-task uses:

```yaml
schemaVersion: 1
name: delivery-qa
enabled: false
schedule:
  deliveries_landed:
    branch: agent-main
    threshold: 3
    max_wait_minutes: 360
    coverage: integrated_qa_v1
    max_items: 20
    retries: 0
# Retain the normal template, dedupe and attribution fields.
```

`owner_machine` is optional and selects the stable registered machine ID shown by
preview. Omitted, the definition is owned by the machine registered as this
workspace's owner, so an unambiguously owned workspace needs no redundant
per-definition configuration. Cmd supplies both the local identity and the
registered workspace owner through the runtime binding; Core does not read the
registry itself. An explicit `owner_machine` stays authoritative and overrides
that default, and because the epoch is derived from the resolved owner, dropping
an explicit owner that names the same machine keeps the consumer's state, frozen
batches, receipts and coverage.

A machine that is not the resolved owner fails closed as `owned_elsewhere`: it
reconciles work it already admitted and admits nothing new, so a replica cannot
claim a workspace by omitting the field. When no owner can be resolved at all —
no registered workspace owner, or a workspace record and replica checkout naming
different owners — an enabled definition reports `ownership_unresolved` rather
than `disabled`, and `disabled` continues to mean the operator disabled it.
Preview, inspection and real evaluation report the same effective owner and the
same refusal. Fix it by registering the workspace owner or by setting
`owner_machine` explicitly. Reassigning ownership is a definition change: retain
and settle the old owner's debt and preview the new baseline first.

A delivery review definition mints its tasks with the crew named in its own
template, exactly like any other auto-task. `operation.review_crew` is a
different setting: it selects the reviewer for `before-pr` review only and does
not apply to `after-landing` review, which runs through this definition. See
[operation-mode operations](../operation-mode/5_operations.md).

`coverage` is `integrated_qa_v1` or `landed_code_review_v1`. Threshold must be
positive and at most `max_items` (maximum 50). Maximum wait is positive and retries
are 0–5. Defaults are 50 items and zero retries. Retries have five-minute backoff
and a captured 24-hour automatic-retry deadline. A task waiting for approval is
still the same action; waiting does not mint another task. Closed/rejected tasks
stop automatic attempts and retain the coverage gap. Only known-stopped failed
jobs are automatically replaceable within the captured budget.

For job-only routines, replace `trigger.cron` with the same
`trigger.deliveries_landed` object, retain `target: job:<existing-job>`, use
`policy.overlap: forbid`, and pin at most one host. The existing routine placement
and pause controls still apply. Routine retry limits may reduce the delivery
budget. Normal job admission still checks capacity and grants.

The existing CLI supports `orbit auto-task add` / `update` with
`--deliveries-landed '<JSON trigger object>'`, mutually exclusive with cron and
interval flags. MCP add/update accept the same object in `schedule`. Creation
remains disabled. Preview a definition before enabling it:

```sh
orbit auto-task show delivery-qa --preview --json
orbit tool run orbit.auto_task.show --input '{"name":"delivery-qa","preview":true}'
```

Preview reports the owner identity, proposed baseline and existing debt without
creating tasks, submitting jobs or moving checkpoints. Inspect the configured
integration branch, independent of the executor worktree HEAD. The first enabled
observation records an explicit baseline exclusion; it does not certify that old
history was examined. Source repository identity and branch are retained.

To migrate, first inspect and disable the corresponding legacy time-triggered
consumer, finish any existing open sweep, review the delivery template, resolved owner and branch,
preview its baseline, and explicitly enable the delivery definition through the
existing toggle surface. Enable the existing scheduler routine/clock separately
if needed. No shipped change activates live automation or imports prose cursors
as coverage. Manual mint remains unconditional and has no automatic batch claim;
`skip_if_open` prevents new automatic admission while a manual instance is open.

## Delivery identity and source gaps

A provider PR is one landing, even when it closes an epic and several tasks or
spans several rebased first-parent commits. Provider commit associations establish
the span; a task marker never establishes identity. Squash and merge anchors use
the same PR key. Manual PRs need no Orbit task. A distinct revert is a new landing.
A no-diff PR, task completion or epic closure alone contributes zero.

The direct-delivery owner retains its authorized before/after intent before a
fast-forward merge. It counts only after the exact resulting commits and trees
are verified on the configured branch. If the owner stops immediately after Git
merges, the next source observation can recover from that intent. A multi-commit
bundle remains one delivery. Unattributed commits and unavailable/ambiguous
provider evidence remain explicit obligations; they are not zero-change results.

Each pass reads at most 200 new first-parent commits, makes bounded provider
lookups, and revisits unresolved identities with a rotating cursor. Commands have
a two-second limit, a one-MiB output limit and a shared 30-second source budget.
Provider lookup work stops early to leave time for classification. State admits
at most 1,000 pending landings and 5,000 commits; backpressure stops new observation
while already retained debt remains eligible for admission. A batch is at most
one MiB. Missing objects and non-ancestral history fail closed.

## Evidence submission

Every automatically minted task contains the immutable batch input and a complete
JSON evidence template. Inspect its exact `(from_exclusive, through_inclusive]`
range, including unattributed neighbors. Fill in the actual checks and findings,
replace the action ID placeholder with the current task ID, and attach:

```sh
orbit tool run orbit.task.artifact.put --input '{"id":"<assigned task>","source_path":"./automation-coverage.json","path":"automation-coverage.json","model":"codex"}'
```

The version-1 schema has these required fields:

| Fields | Required meaning |
| --- | --- |
| `schema_version`, `batch_id`, `consumer`, `epoch`, `input_digest` | Exact frozen identity; version is 1. |
| `action_id`, `attempt`, `coverage` | This admitted action and examination class. |
| `from_exclusive`, `through_inclusive` | Exact objects, each with `commit` and `tree`. |
| `examined_commits`, `examined_deliveries` | Complete ordered lists from the input. |
| `examination_complete` | True only when all obligations were examined. |
| `checks` | Nonempty array of nonempty `subject`, `method`, `observation` objects. |
| `findings` | Array of finding references/descriptions; it may be empty. |

Unknown fields, changed identities, partial membership, empty checks, unavailable
source objects or false completion cannot advance coverage. Findings may remain
open after complete examination. Skipped required checks mean incomplete evidence.
Task `done`, process success, summary prose and ordinary attachments are insufficient.

Authority comes from the transport-supplied executor run, its persisted task
assignment and the task's assigned run ID. A self-reported model name is attribution,
not authorization. The artifact Store writes the reserved
`automation-evidence-authority.json` with the run and exact content digest under
the existing task lock. Callers cannot upload that reserved artifact. This also
works through the checkoutless hub: the hub retains origin, while the batch owner
verifies its source and run assignment during evaluation. An upload without valid
executor context can be stored, but cannot certify coverage.

For a job-only routine, the assigned job returns a `coverage_evidence` object in
an existing persisted step result. It must match the immutable `input.automation`
batch and the admitted run ID. No additional tool is needed.

## Inspection and recovery

`orbit auto-task show --json`, MCP `orbit.auto_task.show`, routine status/show and
the dashboard expose baseline, observed/covered boundaries, pending membership,
unresolved evidence, active attempt/action and recent accepted receipt metadata.
Open **Delivery coverage** on a dashboard operation card to inspect the immutable
input, gaps and validation reason. **Accepted evidence** downloads the accepted
bytes; replacing the current task artifact does not change that receipt. Usage is
shown as unknown until an authoritative measurement exists.

Every delivery diagnostic also carries `ownership`: the resolved
`owner_machine`, the `authority` that supplied it (`definition`, `workspace`,
`missing` or `conflicting`) and whether it is `owned_here`.

Read-only inspection reports persisted scheduling reasons including
`awaiting_baseline`, `disabled`, `owned_elsewhere`, `ownership_unresolved`,
`definition_changed`, `open_instance`, `threshold_reached`, `max_wait_reached`,
`batch_pending`, `retry_backoff`, `retry_deadline_expired`, `needs_attention`,
and `evidence_unavailable`. It does not fetch source or provider evidence: source
history failures are reported by an evaluation run, not fabricated by inspection.
Validation failures such as `unauthorized_submitter`, `batch_or_attempt_mismatch`
and `incomplete_examination` remain attached to the relevant admission or evidence
operation. State read failures are reported separately; corrupted delivery state is
never treated as a new baseline.

Observation, admission and coverage are separate durable transitions. A claimed
batch replays the same task/job action key after a crash. Job admission atomically
seeds ordinary pipeline state. Accepted receipt bytes and the coverage checkpoint
commit in one transaction, so replay accepts once. Later arrivals remain outside
the frozen batch and become the next obligations.

Disablement retains debt and still permits reconciliation of an already admitted
action. Definition edits retain the old epoch/input and pause new admission; restore
the original definition to resume that epoch, or adopt the new one through the
audited recovery below. Do not delete state to clear an error.
Unreadable existing task bundles are retained for repair rather than erased during
key replay. Restore missing source objects or repair the recorded bundle before
retrying. Exhausted, failed and waived states are distinct from coverage; waivers are explicit through the existing definition update:

```sh
orbit auto-task update delivery-qa --waive-batch <batch-id> --waiver-reason "<reason>"
orbit tool run orbit.auto_task.update --input '{"name":"delivery-qa","waive_batch":{"batch_id":"<batch-id>","reason":"<reason>"}}'
```

Only the current settled failed/exhausted batch may be waived. The archived
disposition removes its landings from threshold eligibility, retains the full
code gap and never advances coverage. A later examination can still cover that
code as neighboring context.

### Recovering a consumer stalled by a settings change [ORB-12295]

Retuning a threshold, wait, batch size, retry count, template or crew moves the
definition's epoch, so the consumer reports `definition_changed` and admits
nothing while every obligation it already holds stays retained. `orbit auto-task
recover` is the supported way forward for a delivery auto-task. It never waives
a batch, advances the covered cursor, reopens a terminal task or edits a state
file.

Preview first; with neither operation flag the command only reads:

```sh
orbit auto-task recover delivery-qa --json
```

The preview reports the consumer key, the recorded and configured epoch, the
settings that differ by name, the retained debt (covered/observed boundaries,
pending landings and commits, unresolved evidence, waived and excluded landings,
accepted receipts), the frozen obligations of any stalled action, the refusals
that apply, and the audited recoveries already recorded.

Adopt the retuned settings, then reissue an action that closed without accepted
evidence, in one explicitly authorized request:

```sh
orbit auto-task recover delivery-qa \
  --adopt-settings --reissue-action \
  --reason "adopt tonight's QA threshold and re-examine the unpaid landing"
```

Adoption replaces only the recorded configuration identity. The covered cursor,
pending window, unresolved evidence, waived and excluded landings, accepted
receipt bytes and the frozen batch all arrive unchanged, and the replaced
identity is retained in an immutable recovery record.

A reissue authorizes exactly one further attempt over the *same* frozen batch:
the next evaluation admits a new task under a new action key, carrying the
identical obligations and naming the action it replaces. The closed task is left
exactly as it settled — nothing reopens it — and the authorized attempt has its
own 24-hour admission deadline rather than a widened retry policy. Coverage
still requires complete evidence from the reissued task's assigned executor;
evidence frozen against the replaced attempt cannot certify it, and a second
failure settles the batch again until another explicit reissue.

Any refusal aborts the whole request before the write, naming every reason that
applied: `unknown_consumer`, `member_consumer`, `owned_elsewhere`,
`branch_changed`, `repository_changed`, `owner_changed`, `coverage_changed`,
`coverage_unverifiable`, `active_execution`, `settings_unchanged`,
`no_settled_action`, `action_evidenced`, `definition_changed` (a reissue that
does not also adopt the identity it would run under) or `missing_authorization`
(no reason or actor). A change of workspace, owner machine, repository, branch
or coverage class is never adopted: those change what the retained debt means,
so settle the old consumer's debt and preview a new baseline instead. An action
that is claimed or admitted has to settle first.

Only this host, as the resolved owner, may recover its own consumer, and only
delivery auto-tasks are covered: delivery routines and state-member consumers
still follow the restore-the-definition path. A consumer baselined before its
resolved trigger was recorded proves its examination contract from the frozen
batch instead; one with neither is refused as `coverage_unverifiable`.

Verify a recovery from its own response, whose `applied` names exactly what
changed, and afterwards from a fresh preview: `history` carries the audit
record, and `orbit auto-task show delivery-qa --json` must still report the same
covered boundary and pending membership as before.

### Replaying a consumer after a legitimate branch rebase [ORB-12312]

Use the separate replay mode only when evaluation reports `history_diverged`.
The flag alone is an inert preview:

```sh
orbit auto-task recover delivery-qa --replay-history --json
```

The preview captures the configured branch head and consumer generation, shows
the unique orphan-to-canonical mapping proof, and lists newly inserted delivery
keys that will remain unpaid. Apply the already-previewed repair with an audit
reason; settings adoption and action reissue cannot be combined with this mode:

```sh
orbit auto-task recover delivery-qa --replay-history \
  --reason "reconcile the verified Sep 8 content-preserving rebase"
```

Replay never rewrites Git. It preserves the baseline, covered cursor, accepted
receipts, waivers, exclusions, and the complete active batch/action/input digest.
Pending deliveries, unresolved commits, and provider associations are replaced
only through stable delivery keys and exact commit mapping; inserted commits use
the normal provider association path. A mapped unresolved-only orphan keeps its
exact reason on the canonical commit without acquiring a fabricated provider
association. State and its immutable recovery record
commit in one generation-fenced transaction. The command also compares the
captured branch head immediately before that transaction and refuses a moved
head. Restore unavailable objects or provider evidence and retry; do not reset
the consumer or treat missing proof as coverage.

### Automatic replay, and the stall that replaces a silent retry loop [ORB-12346]

`history_diverged` no longer defers forever. When observation reports it, the
evaluator runs the same replay proof `--replay-history` previews, in the same
pass:

- **The proof succeeds** — every observed commit maps onto a canonical commit
  with an identical `.orbit` tree and parent-relative patch. The evaluator
  applies the replay itself under a recovery record attributed to
  `system:automation`, files one friction so the rewrite is not invisible, and
  keeps observing. The tick reports `history_replayed`; the next tick is
  ordinary observation. No operator is in the loop for a proven-safe rebase.
- **The proof is refused** — `history_mapping_ambiguous`, `history_debt_lost`,
  `history_contract_drift`, `provider_proof_unavailable`,
  `coverage_unverifiable` or any other refusal. The consumer records a
  `stall` marker naming the orphaned revision, the head, the refusal and the
  obligations it could not map; files one friction carrying that list and the
  two commands that clear it; and stops evaluating. The tick reports
  `stalled: history_diverged`, `recover`'s stall classifier reports
  `history_diverged` rather than `not_stalled`, and `orbit doctor` lists the
  consumer under `automation-consumers`.

Frictions are deduped on repository, branch, reason and the orphaned revision,
so repeated ticks and every sibling consumer watching the same branch share one
record. The eventual recovery or reset links it (`friction_id`).

Deferred reasons are classified rather than treated alike.
`history_diverged`, `repository_changed`, `provider_identity_missing` and
`state_missing` are *stuck*: they read identically on every future tick, so they
stall the consumer instead of retrying. Everything else — `source_backpressure`,
`concurrent_evaluation`, `source_deadline`, `source_budget`, a superseded claim —
keeps the silent retry and writes nothing, because marking a transient deferral
would put a fenced state write on the path of the pass that is making progress.
A recorded stall that outlives `automation.stall_window_minutes` (default 60) is
logged once at `warn` and filed once as friction; before that window it is
recorded but quiet. A stall clears itself only when the orphaned revision is
provably reachable from the branch head again, which moves no debt and needs no
audit.

### Resetting a consumer whose debt cannot be reconciled [ORB-12346]

`orbit auto-task reset` is the audited counterpart to recovery: the only
operation that *forgets* obligations. Use it when no recovery can repair the
consumer — a destructive rewrite, or pre-0.21.0 state with neither a recorded
trigger nor a frozen batch, which recovery refuses as
`coverage_unverifiable`.

Without `--reason` it previews and writes nothing:

```sh
orbit auto-task reset delivery-qa --json
```

The preview names the consumer key, its generation and epoch, the debt that
would be forgotten (pending deliveries and commits, unresolved evidence, waived
and excluded landings, accepted receipts), any executing action, a recorded
stall, and the head the consumer re-baselines at.

```sh
orbit auto-task reset delivery-qa \
  --reason "agent-main was rewritten past the observed commit"
```

An apply writes one immutable record into the same `automation_recoveries`
table — kind `reset`, carrying `by`/`at`/`reason`, the previous epoch and
generation, the forgotten-debt inventory, the abandoned action and the new
baseline — drops the consumer row in the same transaction, and deletes the
`refs/orbit/automation/<consumer digest>/*` pins of the forgotten batches. The
next evaluation seeds a fresh baseline at the configured branch head *with* its
resolved trigger, so a later recovery can always prove its coverage contract.

Reset has deliberately no compatibility refusals: a branch, repository, owner or
coverage class that moved is exactly when it is needed. It refuses
`action_executing` unless `--force` is passed (the admitted task or run is
abandoned, not cancelled), `member_consumer`, `unknown_consumer`,
`owned_elsewhere` and `missing_authorization`. Forgotten debt is not coverage:
the discarded landings never appear in a receipt, and the record is the only
trace they existed.

## Rollback and limits

Disable delivery definitions and allow admitted work to settle before rolling back.
Move unsupported delivery YAML outside the old binary's discovery directories;
retain the existing legacy definitions/cursors and all host database/task data.
Re-enable legacy scheduling only deliberately. Auxiliary admission tables preserve
the existing task/allocator format. The reserved authority file is an ordinary
artifact in the existing manifest format, so older readers can retain it. New host
Store feature records do not replace the old scheduler state or require a new DB.

Source objects are pinned under `refs/orbit/automation/<consumer digest>/<batch>/`.
Retain those refs while any obligation is unresolved and throughout the desired
audit window. Explicit retention cleanup after that window may delete the refs;
no automatic GC policy is added here. `orbit auto-task reset` deletes the pins of
the batches it forgets, and records which ones it released.

The source currently understands GitHub PR evidence and authorized local direct
landings. Other/manual direct changes stay unresolved until an authoritative
receipt exists. A history rewrite is replayed only on deterministic proof, and
otherwise stalls the consumer for an operator; nothing resets itself. Automatic
policy migration and complete usage accounting remain separately scoped work.
No review exclusions are inferred from tags or summaries, and QA coverage never
substitutes for review.

## Before-PR coverage exclusions [ORB-11333]

Passed before-PR certificates (see [operation-mode operations
§10](../operation-mode/5_operations.md)) are the only exclusion producer.
When observation first sees a landing, Core looks up passed certificates
whose final tree equals the landed tree, verifies that the certificate's
objects still exist and every task still has the reviewed meaning, and asks
`orbit_automation::review::exclusion` for the decision: same base tree, same
final tree, no contradicting managed landing record. A `landed_code_review_v1`
consumer moves an accepted landing into its `excluded` list; it does not
count toward the threshold, is absent from `examined_deliveries`, and does
not mint an examination receipt. An exclusively excluded prefix advances the
covered cursor and leaves the pending window so later uncovered landings can
still be observed. Interleaved exclusions travel with the next frozen batch
as readable context (`exclusions`) and retire with that examined range.
`integrated_qa_v1` consumers ignore exclusions entirely. A different base
tree, any later edit, an unreviewed conflict repair, task drift, missing
objects, or an external landing race keeps the landing an ordinary
obligation. Inspection surfaces and the dashboard list
excluded landings with their certificate and assurance label.

## State preparation and failure triage [ORB-11331]

The same routine sweep now accepts `trigger.state` with one of two kinds. Core
supplies authoritative task envelopes, pinned source and run/history evidence;
`orbit-automation::members` owns due decisions, material fingerprints, incident
identity, frozen attempts and receipt acceptance. Store uses its existing
consumer/coverage transaction and generation fence. No new database or clock is
introduced. Source retention uses the existing `refs/orbit/automation/` namespace.

Migration is an explicit edit of a selected routine. Disable its old temporal
owner, settle any existing run, and replace only that definition's trigger.
Preserve the user's policy. Do not run both old and new definitions;
a sweep preview reports `duplicate_routine_ownership` for enabled definitions
sharing the same source and target when one uses state scheduling. Existing
shipped pilot/triage cron definitions remain unchanged and no live routine is
enabled by this implementation.

```yaml
schemaVersion: 1
name: state-pilot
enabled: false
target: job:task_pilot_pipeline
trigger:
  state:
    kind: preparation_eligible
    owner_machine: hm_your_registered_machine
    branch: agent-main
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    retries: 1
    deadline_minutes: 90
policy:
  overlap: forbid
  timeout_minutes: 90
  retries: {max: 1, backoff_minutes: 5}
```

For failure triage use `kind: execution_failed`, `target:
job:task_triage_pipeline`, and a suitable aggregate deadline such as 30 minutes.
Cron, deliveries and state triggers are mutually exclusive; state kinds have
fixed pipeline targets, require one owner and forbid overlap. Retry limits are
the minimum of the trigger and routine policy. Each consumer admits one member
at a time, so a preparation action is a one-task pilot partition. `max_items`
bounds the candidate admission checks in a pass, not worker concurrency. The
source page contains at most 50 task envelopes and retains a continuation.

Preparation includes populated selectors when their assessment is missing or
stale. The material fingerprint covers task meaning, criteria, plan, selectors,
relationships, dependency decisions, task/crew assignment, resolved model/provider,
required tools, tags, pinned repository instructions and source revision. Comments,
audit writes, priority and execution summaries do not invalidate it. Accepted
apply records certify the resulting fingerprint, retaining the original input
and exact resulting assessment in immutable receipt bytes. A fresh unready result
is an assessment, and does not repeatedly dispatch. Changing a material input
creates new work; the quiet period coalesces edits up to the maximum wait.

Triage requires the current workflow-failure history event and coupling. Later
human blocks, cancellations, diagnostic-origin runs, active recovery and missing
lineage are withheld. Retry roots and an explicitly recorded blocking child cause
identify the incident; uncertain multiple-child causality is `incident_unresolved`.
The source adapter follows exact indexed retry-child edges, bounds an episode at
1,000 runs and reports a scan-budget limit rather than guessing when reached.
Incident membership is gathered from at most 1,000 current blocked tasks, including
wrappers sharing a child cause; any unsettled member withholds the whole incident.
An incident can include at most 50 tasks. Larger inventories remain withheld.
Normal stale-owner reconciliation and the existing evidence-gated already-landed path remain in
place. Disposition writes hold the task lock and recheck current failure intent;
only the existing bounded environmental re-backlog rule can move a task.

Action-key lookup recovers a run admitted before its scheduler acknowledgement.
Retries preserve consumed attempts and an absolute deadline across restarts;
failed inputs stay visible and unchanged exhausted inputs do not refire. An
unrelated pending member can proceed after an exhausted member. Successful apply
step evidence is read independently of wrapper status. Pilot fan-in accepts any
successful partition so apply can retain valid results before the final guard
reports missing or invalid partitions.

`orbit routine show --json`, routine status and the dashboard expose the shared
state projection: pending fingerprints, fresh/unready assessments, withheld
reasons, consumed attempts, absolute deadlines, continuation and immutable
receipt links. Usage stays unknown when no measurement exists. Readiness is
positive evidence only; this trigger grants no promotion, commit, merge or
implementation authority. Operation-mode grants [ORB-11332] are separate
records: a valid grant supplies this evaluator a scope and due interval and
lets the drain promote in-scope tasks whose accepted assessment is still fresh;
see [operation-mode operations](../operation-mode/5_operations.md).

Keep definitions disabled for rollout review. Inspect `orbit routine list`,
`orbit routine show <name> --json`, and the existing `orbit sweep --dry-run`
preview before deliberate enablement. Timing edits retain active budgets. Changes
to trigger kind, owner, target or branch return `definition_changed`; restore the
original definition to settle it rather than deleting state. Rollback disables
new admissions and preserves receipts; a binary without state-trigger support
rejects the unknown configuration key. General multi-member batching, automatic
host/epoch transfer and automatic promotion are not part of this implementation.
