# Delivery automation operations [ORB-11330]

Delivery triggers are opt-in. The existing sweep clock evaluates routines; the
existing `auto_task_scheduler_pipeline` evaluates auto-task definitions. Routines
submit ordinary jobs, and auto-tasks create ordinary backlog tasks for the normal
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
    owner_machine: hm_your_registered_machine
    branch: agent-main
    threshold: 3
    max_wait_minutes: 360
    coverage: integrated_qa_v1
    max_items: 20
    retries: 0
# Retain the normal template, dedupe and attribution fields.
```

`owner_machine` must explicitly select the stable registered machine ID shown by
preview before an enabled definition can baseline or admit work. Cmd supplies the
local identity; Core does not read the registry. A different machine fails closed
until ownership is explicitly reassigned. Retain/settle the old owner's debt and
preview the new baseline before that reassignment.

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
consumer, finish any existing open sweep, review the delivery template, explicit owner and branch,
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
orbit tool run orbit.task.artifact.put --input '{"id":"<assigned task>","source_path":"/tmp/automation-coverage.json","path":"automation-coverage.json","model":"codex"}'
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

Read-only inspection reports persisted scheduling reasons including
`awaiting_baseline`, `disabled`, `owned_elsewhere`, `definition_changed`,
`open_instance`, `threshold_reached`, `max_wait_reached`, `batch_pending`,
`retry_backoff`, `retry_deadline_expired`, `needs_attention`, and
`evidence_unavailable`. It does not fetch source or provider evidence: source
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
the original definition to resume that epoch. Do not delete state to clear an error.
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
code as neighboring context. General epoch-transfer/reset workflows remain
separately scoped.

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
no automatic GC policy is added here.

The source currently understands GitHub PR evidence and authorized local direct
landings. Other/manual direct changes stay unresolved until an authoritative
receipt exists. History rewrites pause rather than silently reset. Automatic
policy migration, before-PR exclusion producers and
complete usage accounting remain separately scoped work. No review exclusions are
inferred from tags or summaries, and QA coverage never substitutes for review.

## State preparation and failure triage [ORB-11331]

The same routine sweep now accepts `trigger.state` with one of two kinds. Core
supplies authoritative task envelopes, pinned source and run/history evidence;
`orbit-automation::members` owns due decisions, material fingerprints, incident
identity, frozen attempts and receipt acceptance. Store uses its existing
consumer/coverage transaction and generation fence. No new database or clock is
introduced. Source retention uses the existing `refs/orbit/automation/` namespace.

Migration is an explicit edit of a selected routine. Disable its old temporal
owner, settle any existing run, and replace only that definition's trigger.
Preserve the user's hosts and policy. Do not run both old and new definitions;
a sweep preview reports `duplicate_routine_ownership` for enabled definitions
sharing the same source and target when one uses state scheduling. Existing
shipped pilot/triage cron definitions remain unchanged and no live routine is
enabled by this implementation.

```yaml
schemaVersion: 1
name: state-pilot
enabled: false
hosts: [your-host-id]
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
implementation authority. Operation-mode grants remain separate work.

Keep definitions disabled for rollout review. Inspect `orbit routine list`,
`orbit routine show <name> --json`, and the existing `orbit sweep --dry-run`
preview before deliberate enablement. Timing edits retain active budgets. Changes
to trigger kind, owner, target or branch return `definition_changed`; restore the
original definition to settle it rather than deleting state. Rollback disables
new admissions and preserves receipts; a binary without state-trigger support
rejects the unknown configuration key. General multi-member batching, automatic
host/epoch transfer and automatic promotion are not part of this implementation.
