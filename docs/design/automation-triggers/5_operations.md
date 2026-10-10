---
type: design
summary: "Delivery automation operations [ORB-11330]"
tags: [automation-triggers]
last_validated: 2026-10-07
---

# Delivery automation operations [ORB-11330]

Delivery triggers are opt-in. The host clock tick evaluates routines and auto-task
definitions in-process. Routines submit ordinary jobs, and auto-tasks create ordinary backlog tasks for the normal
approval/admission lifecycle. There is no new daemon or coverage submission tool.

Delivery observation reuses recorded provider results, including a confirmed
absence of a PR. A lookup that fails or returns ambiguous identities retries
after one minute, then five minutes, then every thirty minutes. The checkpoint
stores each unresolved lookup's last attempt time and count; older checkpoints
without this information remain readable and get an immediate first attempt.
Grouping and owner evidence still run for recorded associations, so missing
landing evidence can arrive without another provider lookup.

Within one clock tick, routine and auto-task consumers share the fetched head
for the same repository, branch and Git object store, and share each commit's
provider response (or failure). Independent clones fetch their own objects.
The cache ends with the tick, and each observation retains its own command
deadline. Exhausted or failed delivery batches report `needs_attention` before
observation, in both preview and live evaluation, while keeping their retained
debt unchanged until recovery. Reconciliation and definition adoption still
precede that hold.

## Configuration and migration

Existing `schemaVersion: 1` cron and `every_minutes` definitions retain their
behavior. Existing `code-review` and `qa-sweep` defaults are unchanged.
`delivery-code-review` ships disabled. The former `delivery-qa` default is
retired and is not seeded; hands-on QA is `qa-sweep` and `qa-full-sweep`. An
unmodified seeded copy of that retired default is removed by managed-asset
refresh. A locally modified copy stays in the catalog until that refresh, and
`orbit doctor` warns that the default is no longer shipped; refresh then keeps
the operator's bytes under `.orbit/.retired-managed/` instead of deleting them.
Initialization
does not overwrite existing workspace definitions. After-landing review is
`delivery-code-review`'s own `enabled` flag (`orbit auto-task toggle
delivery-code-review on|off`) [ORB-13992]; the deprecated
`operation.review_policy = after-landing` still enables it, without editing its
file, while no operator has configured it. See the [review
gate](../review-gate/2_design.md). The shipped defaults carry a
`__ORBIT_BASE_BRANCH__` placeholder for `branch`; `orbit workspace init` and
`orbit workspace sync` render it to the workspace's registered base branch, so a
`main`-based workspace observes `main` rather than another repository's
integration branch. An unedited seeded definition is refreshed onto a changed
base branch by the next sync; an edited one is preserved and must be updated by
hand.

Use one schedule form. A delivery auto-task uses:

```yaml
schemaVersion: 1
name: delivery-code-review
enabled: false
schedule:
  deliveries_landed:
    branch: main  # rendered from the workspace base branch when seeded
    threshold: 3
    max_wait_minutes: 360
    coverage: landed_code_review_v1
    max_items: 20
    retries: 1
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
same refusal, and so do the two surfaces an operator reads first [ORB-12867]:
`orbit auto-task list` renders such a definition as `skipped` rather than
`enabled`, with the refusal, the resolved owner machine and this host in its
`skipped_reason` (table and `--format json` alike), and `orbit doctor` warns
under `automation-consumers` instead of reporting `ok`. Both stay quiet for a
definition this host owns and for one the operator disabled, where `disabled`
already says why nothing fires. Neither surface changes who may admit: an
unadmittable definition was already refused and stays refused. Fix it by
registering the workspace owner or by setting `owner_machine` explicitly.
Reassigning ownership is a definition change: retain and settle the old owner's
debt and preview the new baseline first.

A delivery review definition mints its tasks with the crew named in its own
template, exactly like any other auto-task, with one exception: while
`operation.review_crew` is set, `delivery-code-review` mints with that crew
instead [ORB-13896]. The crew is applied at mint time and is not part of the
consumer's epoch. While this consumer is enabled, `orbit doctor` also fails its
`review` row when the consumer is missing, unowned, wedged, stalled, held for
an operator, on a branch or crew that does not resolve, or when its observed
commit trails `refs/remotes/origin/<branch>` and the oldest unobserved
first-parent commit has waited at least the batch's `max_wait_minutes`, even
if newer pending commits are recent (or that remote history has diverged, or the remote-tracking
ref is missing after a cursor exists). The row names the observed commit and
the remote-tracking head. It does not fetch; the observation pass is what
updates that ref. The same row, `orbit config show` and the drain probe report
whether it is on and when its next batch is due. See the
[review gate](../review-gate/2_design.md).

The coverage a new delivery trigger may select is `landed_code_review_v1`.
`integrated_qa_v1` is retired. Persisted batches, coverage evidence, automation
state, and a not-yet-refreshed definition still decode it; auto-task add and a
schedule update refuse to select it. Threshold must be
positive and at most `max_items` (maximum 50). Maximum wait is positive and retries
are 0–5. Defaults are 50 items and zero retries; the shipped
`delivery-code-review` definition sets one retry, so a single unusable evidence
file does not hold its batch for an operator. Retries have five-minute backoff
and a captured 24-hour automatic-retry deadline. A task waiting for approval is
still the same action; waiting does not mint another task. A task that closes
(done, rejected or archived) without accepted evidence — none attached, or bytes
that fail validation — settles its attempt: the coverage gap is retained, the
reason (including any parse error) is recorded on the attempt, and the next
attempt is admitted within the captured budget. A spent budget holds the batch
as exhausted for an operator. An action never stays `admitted` behind a terminal
task. Jobs are replaced the same way only once known to have stopped.

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
orbit auto-task show delivery-code-review --preview --json
orbit tool run orbit.auto_task.show --input '{"name":"delivery-code-review","preview":true}'
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

Each delivery's `task_ids` come from the landing record, never commit text. A PR
delivery lists every task carrying its `github-pr:<number>` reference, which
promotion stamps on each bundle member before the merge. It also lists the task
of every handoff the owner accepted for that exact repository, landing branch and
PR number: a distributed-drain follower's PR carries no reference on the owner's
task, so the accepted handoff is the record that names it. A direct landing lists
its run's submitted `task_ids`; a handoff landing that fast-forwards an
owner-local candidate lists the handoff's task. When the record names no task, `task_ids` is
empty and `unattributed` says why (`no_landing_task`, or
`task_records_unreadable` on a checkout that cannot read task records); a task
store read failure defers the pass instead. `auto-task show` displays both
fields. Facts recorded before attribution keep their empty `task_ids` and carry
no `unattributed` reason.

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

`orbit.task.artifact.put` — and any task update attaching
`automation-coverage.json` — refuses a file that does not parse as this schema
and stores nothing, returning the exact parse error (for example `invalid type:
map, expected a string at line 137 column 4`) so the executor can fix and re-put
it inside its run. Parsing is the only check at put time; identity, membership,
completeness and authority are validated when the action settles.

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
and `evidence_unavailable`. A delivery auto-task whose edit the next tick would
adopt automatically reports the reason it will have once adopted; one held at
`definition_changed` also carries `refusals`, naming why it was not adopted. It
does not fetch source or provider evidence: source history failures are reported
by an evaluation run, not fabricated by inspection.
The one source fact inspection does read is whether the configured branch
resolves: a consumer that has no baseline yet and whose branch git cannot
resolve reports that failure in place of `awaiting_baseline`, because no tick
will ever end that wait.

A failed source command never defers with a bare token. The reason carries the
command line and the first line of its stderr, for example
`evidence_unavailable: git rev-parse --verify --end-of-options
refs/heads/agent-main^{commit}: fatal: Needed a single revision`, and the same
text appears in the sweep row, `orbit auto-task show` and
`orbit auto-task recover`. A branch that does not exist is a definition error,
not backpressure: `orbit doctor` reports every enabled definition this host owns
whose branch does not resolve under `automation-consumers`, naming the branch,
git's text and the fix (point `schedule.deliveries_landed.branch` at the
workspace base branch, or create the branch). The same check reports every
enabled definition whose resolved owner is not this host, naming the refusal
(`owned_elsewhere` or `ownership_unresolved`), the owner machine and this host's,
and pointing at `orbit auto-task show <name> --preview` for the coverage debt it
is holding. It also reports a consumer *wedged* on a claimed or admitted action whose task
closed without acceptable evidence, reporting its terminal status and its recorded
validation reason. Evaluation settles such an action even while its definition
has changed, so one still reported
means no evaluation is reaching it; the remediation names `orbit auto-task
recover <name> --reissue-action --reason <why>` and `orbit auto-task reset`.
An unminted retry can retain the closed task's failure reason while its own
action ID is empty. Recovery checks the preceding attempt's durable action
key and terminal outcome; it does not mistake that scheduled retry for a live
task. Adoption alone preserves its attempt, backoff and frozen obligations,
and explicit reissue remains available without discarding any debt. A minted
retry is checked against its own task, and an unknown or open action still
refuses recovery. These checks also apply when settings change on a later
tick, after the original task was reconciled.
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
orbit auto-task update delivery-code-review --waive-batch <batch-id> --waiver-reason "<reason>"
orbit tool run orbit.auto_task.update --input '{"name":"delivery-code-review","waive_batch":{"batch_id":"<batch-id>","reason":"<reason>"}}'
```

Only the current settled failed/exhausted batch may be waived. The archived
disposition removes its landings from threshold eligibility, retains the full
code gap and never advances coverage. A later examination can still cover that
code as neighboring context. The waiver is recorded before the response is built,
so the command exits zero even when the definition's `required_tools` names a
tool that is no longer registered; that problem is reported as a `warnings`
entry, not a failure.

### Recovering a consumer stalled by a settings change [ORB-12295]

Retuning a threshold, wait, batch size, retry count, template, crew or dedupe
moves the definition's epoch. For an enabled delivery auto-task owned here, the
next evaluation adopts such a settings-only edit itself [ORB-14033]: it runs the
same refusals and the same audited recovery as `--adopt-settings`, under a
recovery record attributed to `system:automation` whose reason names the
changed settings, and keeps every covered, pending, unresolved, waived and
excluded landing and every receipt. It logs one warning (`<name>: settings
changed (threshold) — adopted automatically, coverage debt retained`), files one
friction deduplicated on the consumer and its old and new identity, and
continues the same pass, so the consumer keeps admitting and `orbit doctor`
stays healthy. Later ticks see the adopted identity and repeat nothing.

The evaluator never adopts an edit `--adopt-settings` would refuse — a changed
branch, repository, owner machine or coverage class, or an action still claimed
or admitted whose task remains open or whose liveness is unknown — nor one it cannot judge alone: legacy state with no recorded
trigger (`coverage_unverifiable`), a state-member consumer, or a consumer
already stalled for an operator (`consumer_stalled`). Those still report
`definition_changed` and admit nothing while every obligation stays retained.
`orbit auto-task show --json` carries the refusals, and the `review` doctor row
names them (`not adopted automatically: branch_changed`). Delivery routines are
never adopted automatically.

For those cases, `orbit auto-task recover` is the supported way forward for a
delivery auto-task. It never waives a batch, advances the covered cursor,
reopens a terminal task or edits a state file.

Preview first; with neither operation flag the command only reads:

```sh
orbit auto-task recover delivery-code-review --json
```

The preview reports the consumer key, the recorded and configured epoch, the
settings that differ by name, the retained debt (covered/observed boundaries,
pending landings and commits, unresolved evidence, waived and excluded landings,
accepted receipts), the frozen obligations of any stalled action, the refusals
that apply, and the audited recoveries already recorded.

Adopt the retuned settings, then reissue an action that closed without accepted
evidence, in one explicitly authorized request:

```sh
orbit auto-task recover delivery-code-review \
  --adopt-settings --reissue-action \
  --reason "adopt tonight's review threshold and re-examine the unpaid landing"
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
that is claimed or admitted and still live has to settle first. A claimed or admitted action whose task
is already terminal without evidence its settlement would accept counts as
settled: it is reissuable and does not refuse as `active_execution`, even
before an evaluation has run. This includes a task minted before a crash left
its id unrecorded on the claim: the consumer looks up its permanent action key
without replaying creation against the edited definition. Read failures keep
liveness unknown and continue to refuse recovery.
The liveness proof is tied to the inspected consumer generation: a concurrent
admission defers recovery instead of treating the replacement action as closed.

A tick reconciles the closed task before judging a compatible settings edit.
When the frozen retry budget remains, it schedules the next attempt with its
existing backoff and adopts the edit in the same pass. Pending deliveries,
accepted receipts and frozen obligations remain unchanged; terminal tasks are
never reopened. An exhausted batch still requires explicit reissue. Before
that tick, doctor's review row reports the closed task's status and gives
the recover command, adding `--adopt-settings` when the recorded identity is
stale. Reset is not needed to retain the debt.

Only this host, as the resolved owner, may recover its own consumer, and only
delivery auto-tasks are covered: delivery routines and state-member consumers
still follow the restore-the-definition path. A consumer baselined before its
resolved trigger was recorded proves its examination contract from the frozen
batch instead; one with neither is refused as `coverage_unverifiable`.

Verify a recovery from its own response, whose `applied` names exactly what
changed, and afterwards from a fresh preview: `history` carries the audit
record, and `orbit auto-task show delivery-code-review --json` must still report the same
covered boundary and pending membership as before.

### Replaying a consumer after a legitimate branch rebase [ORB-12312]

Use the separate replay mode only when evaluation reports `history_diverged`.
The flag alone is an inert preview:

```sh
orbit auto-task recover delivery-code-review --replay-history --json
```

The preview captures the configured branch head and consumer generation, shows
the unique orphan-to-canonical mapping proof, and lists newly inserted delivery
keys that will remain unpaid. Apply the already-previewed repair with an audit
reason; settings adoption and action reissue cannot be combined with this mode:

```sh
orbit auto-task recover delivery-code-review --replay-history \
  --reason "reconcile the verified Sep 8 content-preserving rebase"
```

Replay never rewrites Git. It preserves the baseline, covered cursor, accepted
receipts, waivers, exclusions, and the complete active batch/action/input digest.
Pending deliveries, unresolved commits, and provider associations are replaced
only through stable delivery keys and exact commit mapping; inserted commits use
the normal provider association path. A mapped unresolved-only orphan keeps its
exact reason on the canonical commit without acquiring a fabricated provider
association. Retained provider associations keep their PR identity and map
their shared delivery anchor through the complete proven commit mapping;
replay refuses an anchor without that proof. State and its immutable recovery
record commit in one generation-fenced transaction. The command also compares the
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
`concurrent_evaluation`, `source_deadline`, `source_budget`, `source_fetch_failed`,
a superseded claim — keeps the silent retry and writes nothing, because marking
a transient deferral would put a fenced state write on the path of the pass
that is making progress. `source_fetch_failed` is that retry: the pass does not
fall back to the local branch, and it does not advance the cursor. A later
doctor read still sees the remote-tracking ref from the last successful fetch,
so a cursor that has fallen behind past `max_wait_minutes` is not reported `ok`.
A fetch that keeps failing while that ref is missing, or still matches the
cursor, is visible on each tick as `source_fetch_failed`; doctor does not
contact the network to rediscover it.

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
orbit auto-task reset delivery-code-review --json
```

The preview names the consumer key, its generation and epoch, the debt that
would be forgotten (pending deliveries and commits, unresolved evidence, waived
and excluded landings, accepted receipts), any executing action, a recorded
stall, and the head the consumer re-baselines at.

```sh
orbit auto-task reset delivery-code-review \
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
abandoned, not cancelled) — an admitted task already terminal without acceptable
evidence is not executing and needs no `--force` — `member_consumer`, `unknown_consumer`,
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

State-member attempts pin their frozen source in a namespace owned by one
Orbit root and workspace [ORB-14164]:
`refs/orbit/pins/v1/<owner digest>/<attempt id>`. Several roots and workspaces
can share one Git common directory while each keeps its consumers and runs in
its own store, so only the owner's state can prove a pin unused. The owner is
the canonical Orbit root, the workspace partition, the machine identity and the
canonical Git common directory; the digest of that record names the namespace,
and the record itself is written once, create-only, under
`refs/orbit/pin-owners/v1/<owner digest>`. A canonical alias of a root resolves
to the same owner; a copied or moved root or repository resolves to a new one
and never adopts the pins it left behind. Attempt ids, receipts and action keys
are unchanged.

The checkpoint that settles, exhausts or retires the attempt releases its owned
pin once it commits, except when the accepted result is a pre-upgrade
`material_v1` assessment whose compatibility check still needs that revision; a
retry keeps it, and a checkpoint that loses the generation fence releases
nothing. Current `material_v2` results release at settlement. A release first
requires the owner record to match, then deletes the ref only while it still
names the attempt's commit; a refusal or failure is logged and leaves the pin.
Admission refuses to pin into a namespace whose record names another owner, and
never rebinds an existing pin to a different commit.

Releases before owner scoping pinned every attempt at the shared top level,
`refs/orbit/automation/<attempt id>`. Those pins carry no owner, and an earlier
client or another root sharing the repository may still read one, so they are
read only as an exact fallback and never deleted by settlement or cleanup.
`orbit doctor --fix-automation-pins` reclaims this owner's leaked pins: under
the routine sweep lock it lists the owned namespace, then inventories this
owner's consumers and this workspace's live pilot runs, keeps every pin an
in-flight attempt, a live run or an accepted assessment names, and deletes the
rest at their listed commit. It reports legacy pins and other owners'
namespaces as retained without listing their contents
([health-checks runbook](../../runbooks/health-checks.md#release-leaked-automation-attempt-pins)).

The source currently understands GitHub PR evidence and authorized local direct
landings. Other/manual direct changes stay unresolved until an authoritative
receipt exists. A history rewrite is replayed only on deterministic proof, and
otherwise stalls the consumer for an operator; nothing resets itself. Automatic
policy migration and complete usage accounting remain separately scoped work.
No review exclusions are inferred from tags or summaries, and QA coverage never
substitutes for review.

## Before-PR coverage exclusions [ORB-11333]

Passed before-PR certificates (see the [review
gate](../review-gate/2_design.md)) are the only exclusion producer.
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
A decoded `integrated_qa_v1` record ignores exclusions entirely. New definitions
cannot select that coverage. A different base
tree, any later edit, an unreviewed conflict repair, task drift, missing
objects, or an external landing race keeps the landing an ordinary
obligation. Inspection surfaces and the dashboard list
excluded landings with their certificate and assurance label.

## State preparation [ORB-11331]

> Failed-run triage is retired ([distributed-drain §7.2](../distributed-drain/2_design.md#72-failed-run-triage)): the `execution_failed`
> trigger kind still parses but has no shipped target and fires nothing. Only `preparation_eligible` schedules work.

The same routine sweep now accepts `trigger.state` with one of two kinds. Core
supplies authoritative task envelopes, pinned source and run/history evidence;
`orbit-automation::members` owns due decisions, material fingerprints, incident
identity, frozen attempts and receipt acceptance. Store uses its existing
consumer/coverage transaction and generation fence. No new database or clock is
introduced. Source retention uses an owner-scoped pin namespace: each admitted
attempt pins its source until it settles, exhausts or is retired (see
[Rollback and limits](#rollback-and-limits)).

Since [ORB-12745] the shipped `task_pilot.yaml` default *is* this form:
`orbit workspace init` renders `owner_machine` from the host's registered
machine id and `branch` from the workspace's registered base branch (the
delivery defaults observe the same branch), and ships it `enabled: false`. The
cron form it replaces is kept as a superseded template shape, so
`orbit workspace sync` refreshes an unmodified — or merely opted-in — cron
`task_pilot.yaml` onto the state form owned by this host, keeping `enabled`; a
cron file whose template-owned fields were hand-edited is preserved and
reported. For any other routine, migration is an explicit edit of that
definition: disable its old temporal owner, settle any existing run, and
replace only that definition's trigger. Preserve the user's policy. Do not run
both old and new definitions; a sweep preview reports
`duplicate_routine_ownership` for enabled definitions sharing the same source
and target when one uses state scheduling.

```yaml
schemaVersion: 1
name: task-pilot-<workspace>
enabled: false
target: job:task_pilot_pipeline
trigger:
  state:
    kind: preparation_eligible
    owner_machine: hm_your_registered_machine   # seed-time: this host
    branch: agent-main                          # seed-time: registered base branch
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    batch_size: 5                               # optional; default 5, never above max_items
    retries: 1
    deadline_minutes: 90
    eligibility:                                # optional; these are the defaults
      statuses: [proposed, backlog]
      exclude_tags: [no-diff-expected, no-diff-needed]
      require_tags: []
      task_types: []
    freshness:                                  # optional; overrides config per key
      material_fields: [title, description, criteria, plan, selectors]
      source_sensitivity: ignore                # ignore | context_files | any
policy:
  overlap: forbid
  timeout_minutes: 90
  retries: {max: 1, backoff_minutes: 5}
```

`eligibility` [ORB-12745] is the predicate a `preparation_eligible` consumer
evaluates. Absent, or with any key absent, it resolves to the rule that was
previously hard-coded: `proposed` or `backlog` tasks not tagged
`no-diff-expected` / `no-diff-needed`, of any type, with no required tags.
`statuses` must be a non-empty subset of those two; `task_types` empty admits
every type; unknown keys, blank tags, and a tag both required and excluded fail
the definition closed, and a non-default block is rejected on any other trigger kind.
One resolved value governs observation (the status filter and the
`task_ineligible` withhold), the admission recheck, the prepare/apply
fingerprint of a claimed run. The resolved
predicate is material input: a non-default value is folded into the
fingerprint, so changing it invalidates assessments accepted under the old
one, while the default adds nothing and keeps the fingerprints accepted before
the block existed. Explicit task-ID runs do not consult it.

`freshness` [ORB-13638] decides which edits make an assessed task due again;
eligibility only decides whether it is fingerprinted at all, so a task that
newly becomes eligible is piloted while retagging an eligible, already-assessed
one is not. Each key resolves routine block, then `config.toml`
`[workflow.task_pilot_freshness]`, then the default: `material_fields`
`[title, description, criteria, plan, selectors]` and `source_sensitivity:
ignore`. The opt-in fields are `tags`, `crew` (with the resolved
model/provider), `tools`, `type`, `complexity`, `relations`, `dependencies`
(resolved dependency status and meaning) and `instructions` (pinned repository
instruction files). `source_sensitivity: context_files` makes a head move
material only when it changes the object at one of the task's selector paths
(a directory selector covers everything under it; at most 50 selector paths
are compared, beyond which the member defers with `selector_scan_budget`);
`any` makes every head move material. An empty `material_fields` or an
unknown field fails closed, and a non-empty block is rejected on any other
trigger kind. Scheduling, the prepare/apply fingerprint check, promotion
readiness and `pilot_fingerprint` resolve the same value for a consumer, and
a non-default one is folded into the fingerprint. Pilots still pin and record
the source revision they assessed against, whichever mode applies.

Assessments accepted under the earlier `material_v1` fingerprint, which
hashed every field and the source revision, are carried forward rather than
re-piloted: a scheduled member stays fresh while the `material_v1` hash
recomputed at the revision its receipt pinned still matches, and becomes due
at the first edit that hash covers. Settlement keeps the pin for a legacy result
that still matches this hash; once its assessment is replaced, doctor cleanup
can release the now-unreferenced pin. The pinned revision is read from that one
attempt's owned ref, falling back to its exact legacy ref, never by listing a
namespace, and a failure to recompute the
hash is logged before the member is assessed again. Assessments accepted since
the upgrade carry the current contract, so their attempt's pin is released at
settlement; `orbit doctor --fix-automation-pins` keeps any legacy assessment
pin still needed for carry-forward.

Cron, deliveries and state triggers are mutually exclusive; state kinds have
fixed pipeline targets, require one owner and forbid overlap. Retry limits are
the minimum of the trigger and routine policy. `max_items` bounds the candidate
admission checks in a pass, not worker concurrency. The source page contains at
most 50 task envelopes and retains a continuation.

Due members are admitted in batches [ORB-12746]. One pass claims up to
`batch_size` due members (default 5; an explicit value must lie in
`1..=min(50, max_items)`; the default is capped by `max_items`) that Core admits
and that share one pinned source and one stored `task.crew` identity, oldest first,
into a single attempt, and
dispatches one `task_pilot_pipeline` run carrying every member's task id as an
explicit `task_ids` entry. A mixed-crew eligible set therefore yields one
run per crew rather than a single rejected bundle [ORB-12761]. Prepare partitions those ids into groups of at most
five, so a burst of *N* same-crew eligible tasks yields one run with ⌈N/5⌉ pilot
partitions and up to five concurrent workers instead of *N* serial runs;
members beyond the batch, or whose crew disagrees with the claimed attempt,
stay pending for the next admission. The attempt's
retry budget, backoff and absolute deadline are per attempt: a run that stops
before any apply output retries the whole batch, while a member whose input
goes stale before acknowledgement leaves the batch without failing its
siblings. Apply outcomes are per member: every member whose partition returned
a valid assessment is certified by the attempt's single receipt (the receipt
evidence lists the applied members and, keyed by member, why each remaining
member did not apply), and each remaining member is recorded failed at its
fingerprint so it does not refire until its material changes. A member whose
partition needed repair settles with the repair apply. Member state persisted
before batching, whose active attempt names a single `member`, still
deserializes as a batch of one and completes through the same path.

Preparation includes populated selectors when their assessment is missing or
stale. The material fingerprint covers the eligibility verdict, the fields and
source sensitivity the consumer's resolved `freshness` names (by default title,
description, criteria, plan and selectors), and the consumer's non-default
resolved eligibility and freshness. Comments,
audit writes, priority and execution summaries never invalidate it. Accepted
apply records certify the resulting fingerprint, retaining the original input
and exact resulting assessment in immutable receipt bytes. A fresh unready result
is an assessment, and does not repeatedly dispatch. Changing a material input
creates new work; the quiet period coalesces edits up to the maximum wait.

Normal stale-owner reconciliation and the existing evidence-gated already-landed
path remain in place. No automatic disposition writes remain: a terminal failure
leaves its task blocked until a human moves it.

Action-key lookup recovers a run admitted before its scheduler acknowledgement.
Retries preserve consumed attempts and an absolute deadline across restarts;
failed inputs stay visible and unchanged exhausted inputs do not refire. An
unrelated pending member can proceed after an exhausted member. Successful apply
step evidence is read independently of wrapper status. Pilot fan-in accepts any
successful partition so apply can retain valid results before the final guard
reports missing or invalid partitions.

A consumer retains at most 1,000 distinct members across its pending, assessed
and withheld entries; a pending member's withheld reason or superseded
assessment does not count again, so recording why a member waits or failed never
needs room. When an observation page does not fit, the evaluator asks the source
by identity, not by page, which retained keys it still observes — a task while
its status is one the consumer queries, an incident while the current inventory
has it — and retires the working state and failed records of the rest. Their
receipts stay durable, and a member that returns is assessed afresh. At
capacity, a retained member's fresh fingerprint replaces its superseded
assessment; a new member waits for room and the pass reports
`source_backpressure`. The scan still advances, and due members are still
admitted.

A consumer also keeps at most 1,000 failed records. A record holds only while it
still withholds its member: one whose member is pending at a new fingerprint is
dropped, and the store refuses dropping one whose member is in flight or pending
at the fingerprint it failed at. When failed records leave no room for a full
batch, the evaluator first retires those of members the source no longer
observes. A member without a failed record then joins a batch only while there is
room to record its failure, so retiring an attempt always commits; when none
fits the pass reports `failure_capacity` until failed members change or leave
the source. A consumer that claimed past the cap before that reservation existed
retires the failed records of departed members when the attempt settles; if
every failed member is still observed, moving those tasks out of the statuses
the consumer queries frees the room.

Each failed record holds only its own member: the exhausted attempt's identity,
attempt number, budget, deadline and action with that one member at the
fingerprint it failed at, so failed state grows with the members retained
rather than with their batches. The store accepts a new record only as the
exact record of the attempt the checkpoint retires, for a member that attempt
carried and its receipt did not certify. The record keeps the single-member
shape every release since batching reads as a batch of one, so a running older
client decodes, honours and commits over it unchanged. A record an older
release wrote with its whole batch still reads and suppresses as before; the
next observation pass compacts it to its own member, a rewrite the store
accepts only when it keeps that member exactly.

`orbit routine show --json`, routine status and the dashboard expose the shared
state projection: pending fingerprints, fresh/unready assessments, withheld
reasons, consumed attempts, absolute deadlines, continuation and immutable
receipt links. `orbit clock tick --dry-run` and `orbit routine show` also list
the batch: each member the pass would admit with why it is due (`settled` or
`max_wait`), or, while an attempt is in flight, each admitted
member, beside the existing `debouncing` / `fresh` / `needs_attention`
reasons (`batch` in the JSON report). Usage stays unknown when no measurement exists. Readiness is
positive evidence only; this trigger grants no promotion, commit, merge or
implementation authority. Operation mode, which once supplied this evaluator a
grant scope and due interval and let the drain promote in-scope tasks, was
removed on 2026-09-21 ([orbit-core decisions](../orbit-core/4_decisions.md)).

Keep definitions disabled for rollout review. Inspect `orbit routine list`,
`orbit routine show <name> --json` (which reports the resolved owner, branch and
eligibility), and the existing `orbit clock tick --dry-run` preview before
deliberate enablement; the seeded disabled definition reports `disabled`, and an
enabled one with nothing to prepare reports `fresh`. Timing and eligibility
edits retain active budgets — an eligibility edit re-fingerprints pending
members and withholds the ones it no longer admits. Changes to trigger kind,
owner, target or branch return `definition_changed`; restore the original
definition to settle it rather than deleting state. (Delivery auto-tasks, by
contrast, adopt a settings-only edit automatically; see
[Recovering a consumer stalled by a settings change](#recovering-a-consumer-stalled-by-a-settings-change-orb-12295).) Rollback disables
new admissions and preserves receipts; a binary without state-trigger support
rejects the unknown configuration key, and one that predates `batch_size`
rejects that key. Automatic host/epoch transfer and automatic promotion are not
part of this implementation.
