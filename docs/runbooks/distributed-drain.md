---
type: runbook
summary: Set up, migrate, run, and recover a single-owner distributed drain — follower pull with owner-only landing, no follower merges.
tags: [operations, distributed-drain, multi-host, recovery]
paths:
  - "crates/orbit-tools/src/builtin/orbit/drain/**"
  - "crates/orbit-core/src/application/distributed/**"
  - "crates/orbit-cli/src/command/task/lint.rs"
  - "crates/orbit-core/src/application/task/context_repair.rs"
  - "crates/orbit-web/src/api/distributed.rs"
related_features: [distributed-drain, federated-mcp, host-registry, remote-access]
related_artifacts: [ORB-15639, ORB-14260, ORB-14194, ORB-13908, ORB-13941, ORB-14149, ORB-13663, ORB-13642, ORB-13625, ORB-12968, ORB-12516, ORB-12515, ORB-12500, ORB-12495, ORB-12564, ORB-12491, ORB-12490]
last_validated: 2026-10-25
---

# Set Up and Recover a Single-Owner Distributed Drain

Use this runbook to collapse two independent owners, match follower
prerequisites, inspect the owner's drain surface, start a follower's pull
drain, migrate leftover epic/child/review state, and recover a claimed
attempt. Installing matching binaries is not a rollout: starting a follower's
drain is a separate, explicit operator action (step 8).

The owner serves probe, receipt reconciliation, task admission, bind and
settlement over a deterministic internal RPC selected by Orbit's runtime SSH
launch. A replica runs `orbit run auto --pull <selector>` to use it. The five
operations are absent from public MCP discovery and refused by public
`tools/call` under both canonical and formerly advertised names. Client names
and initialize metadata cannot enable internal access. Both endpoints must
support the internal transport revision; preflight fails closed with no public
fallback. CLI diagnostics below retain their authority requirements, and
completion approval, revocation and recovery remain owner-dashboard actions.
The follower never merges.

Internal worker reads and typed claim updates use host-owned session
provenance. The local runtime supplies it directly; the remote runtime selects
the hidden `--worker-host` SSH server mode. Managed worker processes cannot
launch that mode, even after removing their activity environment. Neither
initialize metadata nor tool arguments can grant it. Ordinary agent calls
containing `_worker_read` or `_worker_update` are refused before routing and
again at the owner. Allowed worker updates still pass through input redaction
and record any redaction audit against the claimed task.

Ordinary worker MCP calls to task and friction tools route to the bound remote
owner. Omitting `workspace`, sending `null`, or sending an empty or whitespace-only
string uses that owner destination. A non-string selector is refused as invalid
input; a string naming another workspace is denied as a worker binding mismatch.
Explicit selectors may name the bound owner destination, its logical workspace
ID, or the worker's current checkout path.

Cancellation, admission-stop, window-extension, worker-limit, pull-settlement
and receipt-lookup reports use typed outcome enums in the Rust API. Their JSON
`outcome` values remain the documented snake_case strings. These response
enums reject unknown tokens on deserialization; persisted audit and admission
records retain their existing representations.

## Prerequisites and safety

When upgrading a development build that wrote inline handoff settlements, stop its
handoff writers and replace them with the repaired build before resuming settlement.
The new binary's normal store open applies v57 (`handoff_settlement_sidecars`): it
moves attribution into a separate immutable row and restores lifecycle readability
for v0.30.0 clients. Already-running v0.30.0 clients can keep performing safe audited
operations; their handoffs predate attribution and
do not write the incompatible value. The database rejects inline handoff writes
from an already-running intermediate development client, with a diagnostic to
restart on the upgraded binary, and rolls back its deciding transaction. New
older-client opens are read-only under v57's `ReadCompatible` classification;
their audit appends and mutations are refused. Restart them on the repaired binary
to resume normal operation. Upgrade admission observes the v57 writer floor.

The move and ledger stamp are one transaction. An interrupted upgrade can be
retried; a reported attribution conflict refuses the open without partial repair.
Preserve both records and investigate their provenance before restoring consistent
evidence and retrying. Do not delete receipts, clear settlement evidence, relabel a
handoff as a release, or bypass a claim's authority checks to make the store open.
New inspection results retain handoff crew/model attribution, while the lifecycle
row itself contains only failure/release settlements. No release metadata or
layout-version change is involved.

- **One control plane per repository.** Two independently initialized owners of
  the same repo mint overlapping work. Collapse to one owner before any replica
  pull is even a future option.
- **No live host migration in this procedure.** Copy no private hostnames,
  credentials, or inventory. Use placeholders.
- **No automatic reclamation, automatic review, fleet registry, or follower
  merge.** Age, reservation TTL, and a missing local run are diagnostics, not
  death. `review` means a delivery handoff is waiting; it does not mean a
  reviewer ran.
- **Managed agents never become operators.** A client inside a managed run, or
  with an agent envelope, does not propagate `--operator` or `ORBIT_OPERATOR`.
- **Seeded schedules stay as they are.** This procedure does not enable
  `ship_sweep`, `workspace_ship_pipeline`, or the independent
  `orbit run ship-sweep` CLI.

Placeholders:

| Placeholder | Meaning |
|---|---|
| `<owner-machine-id>` | Owner `machine.id` from `orbit config get machine.id` on the owner |
| `<workspace-id>` | Logical `ws_*` id from `orbit workspace show` |
| `<selector>` | Host-qualified selector from federated `orbit_workspace_list` |
| `<ssh-alias>` | An SSH config alias already able to log in as the owner |
| `<request-id>` | Durable ID of one intended admission request |
| `<archive>` | Path to a `tar.zst` from `orbit task export` |

## Inspect before mutating

On **every** participating host:

```bash
orbit --version
orbit config get machine.id
orbit workspace show
orbit doctor
```

Confirm:

- binary versions and distributed-drain protocol revisions match;
- each host has a distinct task prefix;
- exactly one checkout of this repository reports role `owner`;
- `orbit doctor` `mcp-callers` is `ok`, or a leftover
  `~/.orbit/mcp-callers.toml` / `~/.orbit/mcp-ssh-acceptance/` warning that
  those files **grant nothing** (delete them; deny access by removing the
  caller's key from `~/.ssh/authorized_keys`);
- when before-PR or before-landing review is on at the owner
  (`review.timing` is `before_pr` or `before_landing`), the owner sets a
  reviewer pool, the workspace ships through the PR route, and every follower
  can run that crew. Claims draw from `workflow.purpose_crews.pre_landing_review`,
  or from `workflow.purpose_crews.review` while it is unset, which keeps
  serving the owner's after-landing review tasks either way. When that pool
  is a pool (`["sol", "grok"]`), the owner resolves it to one member each time it offers
  a claim, and that crew is what the claim captures, so every follower must
  be able to run each member it may be offered.

```bash
orbit config get review.timing
orbit config get workflow.purpose_crews.pre_landing_review
orbit config get workflow.purpose_crews.review
orbit config show --all # the Review lines report the timing, the reviewer pool and their sources
```

The owner's `review.timing` is captured on each claim with its review crew,
minutes, `workflow.required_validation_commands` and `review.baseline_commands`; the follower's own
setting does not define review requirements. With `before_pr` captured, each
claimed PR leaf runs the before-PR review between base synchronization and
push: one reviewer with the captured crew fixes what it finds as the
candidate's second commit and comments a summary on the owner's task. A
`reject` or `incomplete` verdict fails the leaf before anything is pushed, and
the owner blocks the task with the findings already on it. An evidence-only
`incomplete`, such as a macOS reviewer owing the Linux CodeQL run, holds
instead. An owner `[[review.host_evidence]]` rule makes that hold
deterministic: the leaf owes the rule's check whatever its reviewer reports. The leaf pushes the held candidate to `orbit-evidence/<branch>` on
`origin` and ends `held`. Its settlement releases the claim with the hold, and
the owner keeps the task `in-progress` under `review_awaiting_evidence`. A
held leaf is not a failure for the breaker, and the task is not pulled again
while held. A Linux owner then fulfils `codeql`-only holds itself
([owner fulfilment](codeql-local.md#owner-fulfilment)). Receipt of the
evidence queues the task for a fresh review, which a later pull takes. A
passed verdict
travels in the handoff, and the owner checks the certificate against its own
copy before it accepts, including the captured owner check list. The owner also
verifies exact-run and exact-head validation logs using its own required
commands; a command-list mismatch fails closed. A same-OS log reporting known
red is accepted only after a comparable base run fails every test it names. The
owner's own run is comparable only when the follower was admitted on the same
host OS as this owner. That OS is the validation environment the drain
records, and a different OS is a different validation environment. The base
must also be in the history of the base branch this owner captured at
admission, for both same-OS and cross-OS settlements. An off-branch base
refuses settlement without running a validation command. Settlement gives all
owner base comparisons one ten-minute deadline within the routed call's
fifteen minutes.
A cut or other inconclusive attempted run is retained separately from conclusive
base results; retries reuse its refusal and partial failures never confirm the
report. A later conclusive base result can replace that refusal; a moved base
or different replay selection gets its own check. When that same-OS base run
contradicts the report (passes the reported tests or fails without one), the
owner releases the claim with a `known_red_contradicted` reason and the
follower closes its settlement obsolete; no revoke is needed. When the follower
was admitted on a different OS, after checking that same base-branch
membership the owner does not read a cached result or
start a base run for the report, and it does not release the claim as
`known_red_contradicted` or return the task to the backlog. It accepts the
follower's base evidence for that OS, records the known-red tests, and accepts
the handoff. The pull request the leaf opened stays that delivery for review
and landing, rather than sitting open while a later attempt starts without it.
A follower that cannot resolve
or run the captured crew stops pulling with `before_pr_reviewer_unavailable`
rather than claiming work it cannot review. A local-only ship workspace with
before-PR review on is refused (`before_pr_unsupported`).

With the owner's `review.timing` at `before_landing` instead, the follower's
own value is still ignored. A claimed PR leaf then pushes and opens its
PR first and runs the same contract on the published head after `pr_open`,
while hosted CI runs. A reviewer fix is revalidated on the leaf and pushed onto
the PR under a lease (`push_lease_lost` if the branch moved to a commit that
is not the merged pull request). When the pull request was merged at that
published head, including a squash, and the branch was deleted while the
repair was still local, the push fails as `review_repair_after_merge`
instead: the repair commits are pushed to `refs/heads/orbit-evidence/<branch>`
(or the durable candidate ref if that push fails) and the task is blocked
with the commands to land them as a follow-up pull request. The handoff
carries before-landing evidence for the settled head, and the owner refuses a
handoff without it, with before-PR evidence instead, or for any other head, so
an unreviewed head never reaches `review` or a merge. A leaf whose review does
not approve fails with its PR open and unmerged. An evidence-only `incomplete`
holds as before PR, and its release also names the open PR
(`held_pull_request`): the owner keeps it with the held candidate, and the run
that resumes that candidate after the evidence arrives publishes on its own
branch, then its `pr_open` closes the earlier PR as superseded. The claim's ship contract carries the field, so owner and
followers need matching builds. Readiness, refusals and the reviewer-crew
requirement are the same as for before-PR review, with the reason codes
unchanged.
After-landing review (the `delivery-code-review` auto-task) never affects
admission: the owner reviews landed deliveries whatever host implemented them.
A follower's landed PR reaches the owner's review batch under the claimed
task's id, which the owner reads from the handoff it accepted.

Match toolchains and required validation commands the same way you would for
a second owner-local executor. Crews may differ: a follower only receives
tasks whose crew it can run. A follower runs a pulled task on the crew the
owner's task names, and uses its own `workflow.default_crew` only when the
task names no crew. Each drain declares the crews its host can run (see step
8), and the owner skips a task whose crew is not among them, leaving it in
the backlog for the owner or another follower. Use the same crew names on
both sides: crews are matched by name.

Hosts may run different operating systems. A task tagged `os:linux`,
`os:macos` or `os:windows` runs only on a host of a named OS (several tags mean
any one of them; no tag means any host). Each pull request declares the
follower's OS, and the owner admits a tagged task only to a follower whose OS
it names; the owner's own drain applies the same rule to itself. A task no
current host can run stays in the backlog, never claimed, with the wait named
(`waits for a macos host (os:macos)`) in the owner's readiness, `orbit run
show`, the Drain card, and the follower's idle receipt (`os_unavailable`). So
an `os:macos` repair filed on a Linux owner waits for a macOS follower instead
of blocking the owner's worker. The owner's readiness (`tasks[].host_fleet`)
and the `orbit run show` waiting line of its local drain also name each
registered host of that OS and whether it is pulling, meaning it holds a claim
in this workspace from the last seven days (`registered macos host(s): mac-mini
pulling (claimed here within 7 days)`), or say `no registered host runs macos`.
A readable claim store with no recent claim reports that host as `not pulling
(no claim here within 7 days)`. If claim inspection fails, the fleet note lists
the registered hosts with `claim records unreadable`; it cannot determine
whether those hosts have pulled recently. Unreadable evidence also leaves only
the owner's own OS serving creation-time crew draws.
A task whose dependency is still open also shows that OS wait in readiness
(`tasks[].host_os_wait`), after its dependency. Retag with `orbit.task.update` to reroute a
backlog task; running or claimed work is not moved.

A task tagged `ui` needs a host whose headless browser kit is verified
(`orbit browser-kit status`). Each pull request declares whether the
follower's kit is verified (`browser_kit`, read for every request), and the
owner hands a `ui` task only to a follower that declares one; the owner's own
drain checks its own kit. The task otherwise stays in the backlog as
`browser_kit_missing`, with a wait that names `orbit browser-kit install`, in
the owner's readiness, `orbit run show`, the Drain card and the follower's idle
receipt (`browser_kit_unavailable`). It combines with `os:` tags: a host must
satisfy both. Owner and followers must
deploy the same protocol revision (currently 10, as defined by
`DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA`; OS matching arrived in revision 4). Empty
`workflow.required_validation_commands` means no required check, as on an
owner's own delivery: a claimed leaf runs nothing and records that, and the
owner accepts its handoff without validation logs. Every other handoff check
(footprint, protected paths, candidate and base integrity) still applies.
`orbit doctor` reports the empty list, and `orbit run auto` notes it.

Within a dispatch band, the owner orders each admission against its live
executor census: every follower that pulled in the last ten minutes or still
holds a claim, plus the owner while its local drain runs. Work only the
requesting executor can run leads, ahead of priority: its `os:` tags or its
crew's executor `host_os`, its crew, or a `ui` tag only that host's browser
kit serves. Among the rest, work fewer live executors can run comes first
within a priority. A Mac that alone runs the `opus` crew therefore takes an
opus task before a higher-priority task the Linux hosts can take. Critical and
corrective work still leads, and the owner's local drain orders the same way.
Set `workflow.admission_affinity = "priority"` when an exclusive task should
not pass higher-priority work, for example while the other hosts are
saturated, or `"off"` for the plain order. A pull receipt's `affinity` names
the admitted task's class and the census it was ordered against.

The latest applied task-pilot disposition `host_operational` identifies work
that requires an operator-side action no managed lane can perform. Human
approval to backlog does not clear that finding: local drain and ship
selection report `host_operational_handoff`, workflow admission refuses it,
and owner pull admission defers it without a claim. The pilot's audit and
`host_operational_held` history event retain the disposition and its evidence.
Admission backfills older receipts once. The hold is scoped to the assessed
material; status, priority and unrelated comments do not clear it. Older
receipts without a snapshot hold conservatively until admission or a trusted
operator decision records one; a generic update event cannot prove a re-scope.

An operator can release the hold through the trusted human task-update path
with `task-pilot-admission: evaluated`, `clear`, or `approve-anyway` as the
comment's first line and evidence below it (`evaluated` must reference a
non-empty attached evaluation artifact). This records a `host_operational_resolved`
history event that carries the decision as a typed field. Admission reads only
that field: the same header in any other comment, an agent's included, decides
nothing. A newer assessment supersedes the decision; a `selectors` or
`verified_no_diff` assessment without another hold admits as before.
`no-diff-expected` does not exempt host-operational work, even if an auto-task
lane grants relevant tools. An operator decision releases admission only; task
prose never grants tools.

The same trusted path releases a `pilot_duplicate` or `pilot_already_landed`
hold with `clear` or `approve-anyway`, recorded as a typed decision on a
`pilot_admission_decided` history event. Two text forms from before typed
decisions are still honoured: an operator comment carrying that header alone,
and an auto drain's approval hold recorded as a
`[drain-approval-held:<classification>]` marker in the pilot's history note.
Each counts only when written before its Orbit store's typed-decisions
boundary, which the first writable open by a binary that records typed
decisions writes once to `tasks/typed-decisions.since` in the Orbit root. Text
an older binary wrote before that host upgraded therefore keeps its effect, and
the same text written later is not a decision. Do not delete that file:
the next open would record a later boundary and honour text written in
between. A file an interrupted write left empty or cut short counts from its
modification time, with a warning naming it, and the next writable open
writes that instant into it. Until a writable open records it, text written
from 2026-10-12 on is not a decision.

An `--approve-proposed` drain and the CI sweep share the pilot promotion
rule. Each `blocked_by`, `adr_conflicts`, or deterministic
`validation_tool_warnings` finding holds promotion. Surface and utility findings
use `{"text":"finding and evidence","holds_promotion":true}` for a defect,
contract conflict, or validation gap that prevents admitting the task as written.
Use `holds_promotion: false` for advisory observations such as adjacent work
outside the task's scope. These remain in the pilot assessment comment returned
by `orbit task show` and in the implementing worker's brief, but do not withhold
promotion, mark the assessment unready, or appear in held findings. Existing
plain-string surface and utility warnings continue to hold unless they name only
recorded dependencies or done tasks, as below. Missing severity or
malformed structured findings are rejected rather than silently approved.

Applied task-pilot `blocked_by` findings that name an unfinished task in the
same workspace add a dependency in the assessment's atomic write. Proposed,
backlog, in-progress, blocked and review targets qualify; done, archived,
rejected, missing and other-workspace targets do not. Existing dependencies
are preserved and repeated findings add no duplicate edge. Apply drops a
task's own ID from prerequisite findings before graph validation, recording a
warning in the run's assessment and the task's atomic application history;
other prerequisites and metadata still apply. A self-only finding is removed,
while a mixed finding retains the other IDs and evidence. Dependency cycles
between different tasks still refuse the assessment. Human approval cannot release an unfinished
dependency: readiness reports `unmet_dependency` until the prerequisite is done.

A `blocked_by`, utility or surface finding whose named tasks are all recorded
dependencies of the task (or already `done` in this workspace) is not a promotion
warning: the dependency edge already gates admission, so the task promotes to backlog
normally and readiness reports `unmet_dependency` until the prerequisite is
done. It does not appear in `pilot_warnings` or the dashboard's pilot-warning
readouts or warning-triage candidates. Both `dependencies` and `blocked_by`
relations record the same dependency edges. Pilots should report waiting for
another task's delivery only in `blocked_by`, and omit holding findings for
prerequisites already recorded on the task. Utility and surface warnings never
create dependency edges. Prerequisite prose that names no task, a missing or
other-workspace task, or an unfinished task that is not a recorded dependency remains a
promotion warning, because no edge enforces it. The dashboard shows holding
prerequisites in the pilot comment's **Blocked by** list, and
`orbit task update --approve` prints the latest assessment's blocker warnings
to stderr and includes them in JSON `warnings`. A newer clean assessment clears
the warning text; ordinary comments do not.

**Pilot warning holds.** A holding warning finding waits in `proposed` until
an operator decides it. The seeded `pilot-warning-triage-<workspace>` routine
(state trigger `pilot_warnings_held`, job `escalation_triage_pipeline`) ships
disabled; enabled on the owner, it reads each held assessment once with a
hard-tier crew other than the one that wrote it, and records either a decision
proposal (two or three options and a recommendation) or the human action that
settles the hold. Each is one system comment headed `pilot_warning_triage`,
quoting the holding findings, plus a `pilot_warning_triaged` history marker
naming the assessment's operation id. Nothing else changes: the lane never
approves, promotes, clears the hold or edits the task, and an agent's
`task-pilot-admission:` comment decides nothing. Inspect before acting:

```bash
orbit routine list                       # find pilot-warning-triage-<workspace>
orbit tool run orbit.task.block_triage \
  --input '{"action":"candidates","subject":"pilot_warning"}'
orbit task show <task-id>                # latest assessment and the triage comment
```

`candidates` lists the qualifying holds with their findings and pilot run, and
names why each other held task is excluded: `operator_decided` (a trusted
admission decision answers that assessment) or `triaged` (it already has a
marker). To accept a disputed finding after reading the proposal, approve
through the trusted path: `orbit task update <task-id> --approve --note
"<evidence>"`. Otherwise edit the task as the chosen option says; the next
pilot pass writes a new assessment, which is triaged afresh only if it still
holds. A run that ends without a marker for a member records it failed
(`no_triage_marker`) and retries within the routine's budget. A triage marker
is not a drain-approval decision, so it does not make an `--approve-proposed`
drain re-pilot the task.

Task-pilot checks acceptance criteria for native host evidence requirements.
It records each one as a typed finding, committed with the applied assessment
as `native_os_hold`: `required_os` (the one-based criterion and the OS) for an
OS requirement, `required_machine` (the criterion and the owner machine) for
evidence only the owner's own store, services or data can produce. A
requirement stated only in warning prose routes nothing. Apply rejects
a `required_os` entry whose criterion text names no OS or native facility
(mac, macos, darwin, seatbelt, sandbox-exec, linux, bwrap, bubblewrap,
windows, launchd, systemd, keychain, cgroup), unless its evidence cites one
`#[cfg(target_os = "<os>")]` gate at a repository-relative `path:line` that
apply confirms in the pinned source, on an item the criterion names, and the
entry's `os` is that OS. A portable criterion, a gate on an unnamed item, an
`os` that differs from the gate, an unconfirmed path or line, and a
`cfg(unix)` or `cfg(not(windows))` gate that does not determine one OS are
still rejected. The rejection tells the pilot to leave `required_os` empty,
and the repair pilot re-answers. When every
`required_os` entry names one OS and the task carries no `os:` tag, the pilot's
atomic apply adds that tag, so tag routing sends the task to a host of that OS
and a follower of another OS sees it as `host_os_mismatch`. An `os:` tag the
task already carries is never removed or replaced; a conflicting one is
reported in `utility_warnings`. Platform mentions, cross-compilation, mocked
checks, negative admission tests and measurements any host could repeat do not
require a native host. If evidence is required on every named OS, use separate
host-scoped validation tasks: multiple tags allow any one OS, so the pilot adds
none.

A `required_machine` finding has no tag. The owner refuses a pull claim from
every other machine (`deferred_conflicts`, shown as an `owner_hold` whose
reason begins `Machine requirement:` and names the machine), while its own
local drain may start the task. A pilot may name only the owner (by its
registered machine name or id); a requirement on another machine stays a
`utility_warnings` finding for an operator. A re-scope, a newer assessment without
the finding, or an evidenced operator decision clears it.

This also covers a scan whose brief requires the authoritative owner-workspace
audit and a completed predecessor's cursor artifact or execution summary, even
if its criterion names no host. Preparation supplies `owner_machine`; the pilot
reads the evidence contract alongside the criteria and records the affected
criterion in `required_machine`. Supported tool names are insufficient evidence
of portability: owner listing/search is bridged, but `task.show` and
`task.artifact.get` remain scoped to the claimed task. A follower's audit is a
different host's data. Preserve the selected predecessor and window; an
unreadable cursor is never permission to guess one or begin a new baseline.

The initial pull selection and its transactional candidate recheck both honor
the typed hold, before any claim, leaf or worktree exists. Local backlog
selection, explicit ship selection, drain classification and readiness allow
the owning machine. Routing grants no audit/run capability: those surfaces
retain their operator restrictions. Portable scans whose reads fit the bridge
remain eligible on followers. Independent operator or re-home/re-authoring
holds still apply; a scan title or tag releases none of them.

For a workspace-authored definition such as `tool-failure-patterns`, an operator
can adopt the following evidence contract on the owner after deploying the
updated pilot guidance. This is an adoption procedure, not a bundled asset or
an instruction to change a live definition during a repair task. Coordinate a
window without concurrent definition edits or scheduled admission; use the
registered owner workspace selector, never a replica or linked-worktree path.
The tool's `template` replaces the whole template, so copy the current one
and modify only its description and criteria. Do not send a partial template.

```bash
OWNER_WORKSPACE='<registered owner workspace>'
mkdir -p .orbit/tmp
orbit --workspace "$OWNER_WORKSPACE" auto-task show tool-failure-patterns --json > .orbit/tmp/scan-before.json
python3 - "$OWNER_WORKSPACE" <<'PY'
import json, pathlib, sys
scratch = pathlib.Path('.orbit/tmp')
before = json.loads((scratch / 'scan-before.json').read_text())
template = before['template']
contract = ('Execute on the owning machine using the authoritative owner-workspace '
            'audit and the selected completed predecessor cursor artifact, with '
            'its execution summary as the existing fallback. A follower audit '
            'or guessed cursor/window is not evidence. Preparation must record '
            'required_machine for this owner evidence contract before admission; '
            'routing grants no additional tool scope or operator capability.')
if contract not in template['description']:
    template['description'] += '\n\n## Owner evidence requirement\n\n' + contract + '\n'
if contract not in template['acceptance_criteria']:
    template['acceptance_criteria'].append(contract)
payload = {'workspace': sys.argv[1], 'name': before['name'], 'template': template}
(scratch / 'scan-owner-evidence-update.json').write_text(json.dumps(payload, indent=2) + '\n')
PY
# Review the payload against scan-before.json before applying it.
ORBIT_OPERATOR=1 orbit tool run orbit.auto_task.update --input-file .orbit/tmp/scan-owner-evidence-update.json
orbit --workspace "$OWNER_WORKSPACE" auto-task show tool-failure-patterns --json > .orbit/tmp/scan-after.json
```

Compare the snapshots: only the template description, appended criterion and
update provenance may change. In particular retain all existing criteria,
required tools, tags, crew, status, schedule, dedupe, enabled/effective-enabled
state, settings and cursor. The payload omits every settings field. Re-read and
rebuild it if the definition changes before application. Do not mint, dispatch,
advance a cursor or change existing task lifecycle state as part of adoption.
Have the owner run normal preparation/apply for pending scans before their next
admission, verify the persisted `native_os_hold.machine` names the owner and
that followers report `Machine requirement:`. An already-minted task does not
inherit template edits; assess its existing owner-evidence contract separately.

The shipped `qa-full-sweep` roll-up already carries this kind of contract in
the brief the coordinator copies onto the roll-up task: area-task status and
`qa-progress.json` reads are owner-task-store evidence, so preparation records
`required_machine` for that criterion before admission. Area tasks stay
portable. The coordinator sets the roll-up's dependencies with owner-local
`orbit.task.update` and, when that write is unavailable, returns a
`qa_rollup_dependencies_unwired` blocker instead of treating a comment as the
wiring. Do not re-apply the manual adoption above to the bundled default, and
do not edit a live workspace definition to copy it.

Admission honours an OS finding while the task's `os:` tags do not name the
required OS. A local drain, ship discovery or `orbit run ship` on a host of
another OS leaves the task in `backlog` as `native_os_required`, and the owner
defers a pull from a follower of another OS (`deferred_conflicts`, shown as an
`owner_hold`). Readiness, `orbit run show` and the Drain card name the criterion
and the tag to add. A host or follower of the required OS may still take the
task, and a task whose own `os:` tags exclude a host keeps `host_os_mismatch`.
The wait clears when the tag is added, the acceptance criteria are re-scoped, a
newer assessment carries no finding, or an operator records an evidenced
`task-pilot-admission: clear` or `approve-anyway` decision through
`orbit.task.update` (history event `native_os_requirement_resolved`, carrying
the typed decision).

A trusted native-host decision remains valid across later pilot passes with the
same acceptance-criteria digest, OS requirement set and machine requirement;
requirement order does not matter. A pass with changed criteria or requirements
supersedes it. Operator-validation and host-operational decisions still apply
only to the assessment they answered; a new assessment supersedes them.

Per-host compiler capacity is independent. Keep the shared build-budget
defaults (two heavy slots, four Cargo jobs) unless you deliberately raise them;
see [build-budget.md](./build-budget.md).

## Procedure: owner and follower setup

### 1. Choose the owner and stop competing drains

On the host that will remain owner, keep its checkout as `owner`. On every
other host that currently owns a copy of the same logical workspace:

1. Stop owner-side admission: cancel or wait out `orbit run auto` /
   `orbit run ship-sweep` windows on that host. Stopping a drain stops new
   admissions; it does not cancel live children.
2. Reconcile in-flight runs, pull requests, and reservations (migration
   section below) **before** changing the role.
3. Move tasks that must keep their IDs, or drain them on the demoted host
   first.

Do not delete the `[machine]` table in `~/.orbit/config.toml`, `workspaces.json`, or the store to force
the switch.

### 2. Re-register the demoted checkout as a replica

From the follower checkout, using the **owner's** machine id:

```bash
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica --owner <owner-machine-id>
```

A checkout that is registered as an owner cannot be rebound in place: both
commands refuse with "refusing to rebind". Once its drains are quiet, drop the
registration (registry only; `.orbit` and its tasks stay) and register it again:

```bash
ORBIT_OPERATOR=1 orbit workspace remove <workspace-id>
orbit workspace init --role replica --owner <owner-machine-id>
```

`workspace init` and `workspace role` print the recorded role and owner
(`--format json` carries `role` and `owner_machine_id`). A replica role always
needs `--owner`; the refusals name the flag and say the id is the owner's
`machine.id`. `workspace role` validates or reasserts; it is not a takeover. A replica must
not originate owner-only task mutations or publish/restore the owner's live
task set. Route those operations to the owner. See
[multi-host setup](../../crates/orbit-core/assets/skills/orbit-setup/references/multi-host.md).

The role change also keeps the host's local friction corpus. List it on the
replica with `orbit friction list --status open`, then close each legacy report
with `orbit friction resolve <ID>` or an audited `orbit.friction.update` with
`status: resolved` and disposition evidence. These operations resolve only
existing records in the replica's local workspace partition; the owner's
records are untouched even when IDs collide. New reports, reopening, and actual
re-homing still require owner authority. No owner connection or store migration
is needed to close local legacy reports. See
[friction lifecycle](../../crates/orbit-core/assets/skills/orbit/references/friction.md#closing-legacy-records-on-a-replica)
for tool inputs and audit behavior.

### 3. Transfer remaining tasks from evidence, not by copying the store

On the demoted host, after drains are quiet:

```bash
orbit task export --ids <task-id>,<task-id> -o <archive>
# or, when the whole remaining set should move:
orbit task export --all -o <archive>
```

On the owner:

```bash
orbit task import <archive> --on-conflict=renumber
```

Use `--on-conflict=owner-wins` only for a repeatable mirror of IDs the owner
already minted. Do not rsync `~/.orbit/orbit.db` or task bundles between
machines as a substitute. Publication restore is same-authority recovery, not
cross-host ownership transfer; see [state-and-backup.md](./state-and-backup.md)
and [task-publication.md](./task-publication.md).

### 4. Restore pruned selectors only from recorded history

Filesystem-missing selectors are valid declarations. Do not delete them, and
do not guess replacements. Inspect first:

```bash
orbit task lint <task-id>
orbit task lint
```

Description path checks expand simple comma brace groups (including multiple
groups), up to 64 resulting paths, and report each missing member separately.
Malformed, nested or larger patterns are checked literally. A path immediately
introduced by `create`, `create a`, `create file`, `create a file`, `new file`,
`e.g.`, `for example` or `such as` is treated as a creation target or example.
A path after `to` is a destination when `move` or `rename` appears in the
preceding six words on that line. Matching ignores case and surrounding Markdown
quotes, parentheses, commas and colons. These local markers exempt only that
occurrence: move sources and unmarked missing references still report errors.
This description heuristic does not change declared selector validation.

Re-declare selectors an **older prune recorded in that task's history**:

```bash
orbit task lint <task-id> --restore-pruned
orbit task lint --restore-pruned
```

`--restore-pruned` never invents scope. Unrestorable entries stay unrestorable;
supply own `context_files` yourself. Empty context holds no context lock.
Distributed pull admission, an explicit ship and a single-slot local drain
admit selector-free backlog tasks on the next pass. A local drain or ship with
more than one slot waits for this host's task pilot to prepare one
(`awaiting_footprint`), then runs it only alone (`awaiting_exclusive_slot`). Undeclared edit conflicts are handled at
landing by rebase and conflict repair. Operator task-scope reservation still
requires a declared surface. A live task-pilot preparation (its reservation
from prepare, then its checkpoint) still holds its tasks until that run settles.

Reservation TTL on a pulled claim is 14,400 seconds (four hours). Expiry does
**not** revoke the claim, admit another worker, or shrink the frozen
non-pruned footprint. Status-derived locks on `in-progress` and `review` tasks
keep the declared selectors, including files that do not exist yet.

A claimed implementer may add files anywhere the work requires; the frozen
footprint is a scheduling hint, not a delivery gate. Every added path the
footprint does not cover becomes a widening request, derived from the final Git
candidate rather than the implementer's reported selectors. The owner
independently checks the diff and accepts the widening even when another live
claim, in-progress/review selector or reservation names the path. Acceptance
records exact file selectors, a `context_files_widened` history entry and the
enlarged live claim; the original receipt stays immutable. Only Git or `.orbit`
metadata, environment files, symlinks and malformed paths are refused, with
exact paths. Protected metadata names and environment patterns (including `.envrc`)
ignore ASCII case on every host: `.Orbit/`, `.GIT/`, `.ENV` and `.Env.local`
are refused even on Linux. Both peers require the same protocol revision (currently 10;
widening arrived in 3).


### 5. Establish SSH owner access (no destination callers file)

SSH login to the owner **is** owner access. There is no
`~/.orbit/mcp-callers.toml`, forced-command acceptance, KeyBound proof, or
replacement destination identity registry. `--remote-caller-machine-id` is an
attribution label, not a credential.

On the follower, register the owner and serve federation from the caller:

```bash
orbit host add <owner-ssh-target>
orbit host list            # owner reachable, same binary_version and protocol_fingerprint
orbit mcp init --federated --client <client>
orbit mcp serve --mode federated --operator
```

Without `--operator` on the **calling** side, remote sessions hold `agent`.
Managed agent sessions cannot acquire operator by changing tool input or
launching a privileged child. To deny a caller, remove its key from the
owner's `~/.ssh/authorized_keys`. Details:
[remote-access.md](../../crates/orbit-core/assets/skills/orbit-setup/references/remote-access.md).

### 6. Read-only probe (never a health check for admission)

Run the diagnostic CLI on the **owner**, locally or through an operator SSH
shell. Both `orbit.drain.probe` and `orbit.drain.receipt.lookup` require an
identified caller (`agent` or `operator`). A non-interactive shell uses an
agent envelope or `ORBIT_OPERATOR=1`.

```bash
orbit tool run orbit.drain.probe --input '{
  "caller_version": "<this-binary-version>",
  "caller_schema": 9,
  "caller_before_pr": false
}'
```

The probe reports owner machine, binary version, distributed-drain protocol
schema `8`, this session's capabilities, diagnostic caller machine,
owner-resolved ship configuration (`ship.before_pr`, `ship.before_landing`),
and `review`: the review switches with their sources — before-PR and
before-landing on/off and minutes, after-landing enabled and its next batch
due. Declaring version, schema, or `caller_before_pr`
also reports the **first refusal admission would raise**, in admission order. It creates no receipt, reservation, claim, or
task. A replica destination refuses the tool instead of answering about
itself, naming its owner. Run the diagnostic CLI there; the follower runtime
uses its internal owner selector. Do not call pull as a health check: a pull is an admission, and an
admitted claim is real work the owner holds until it settles.

Expected refusals you may see (and must not work around):

| Error | Meaning |
|---|---|
| `capability_refused` | Destination is a replica, or the session lacks agent/operator identity |
| `version_mismatch` | Caller binary version differs from the owner |
| `protocol_skew` | Caller and owner request fingerprints differ (or the owner predates fingerprints); refused before pull, with both fingerprints in the diagnosis |
| `protocol_mismatch` | Legacy probe report for differing integer revisions; current followers surface typed `protocol_skew` |
| `ship_mode_unsupported` | A remote caller targeted a local-only ship workspace |
| `before_pr_unsupported` | Owner's `review.timing` is `before_pr` or `before_landing` and it ships local-only, or the executor's leaf does not run the review gate (an older binary) |

### 7. Receipt lookup after uncertainty

A receipt is historical evidence, not current execution authority.
`not_found` is not proof an earlier request cannot still arrive, and it never
licenses a replacement request under a new ID.

```bash
orbit tool run orbit.drain.receipt.lookup --input '{
  "request_id": "<request-id>"
}'
```

Returns `found` (original receipt plus current claim phase), `expired`
(non-reusable tombstone), or `not_found`. Naming another machine's namespace
requires operator capability:

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.receipt.lookup --input '{
  "request_id": "<request-id>",
  "machine_id": "<other-machine-id>"
}'
```

Idle receipts compact to permanent tombstones in v1. Unsettled claims keep
full receipts. Do not delete request rows by age. At the shipped 60-second
idle poll (`idle_sleep_seconds`), one drain leaves 1,440 request identities per
day even after compaction. Stop a refill pass after the first idle response; the
next poll uses a **new** request ID. The follower's own copy is bounded: when it
allocates a new request it prunes its idle and refused rows to the newest
10,000 (about a week of one drain's polls). Settled claims are never pruned.

### 8. Start the follower's pull drain

Matching binaries, a replica role, a working probe, and, when the owner's
`review.timing` is `before_pr`, the owner's review crew on this host are **installation**. Starting a drain is the rollout, and it is explicit. On
the follower, from the replica checkout:

```bash
orbit run auto --pull <selector> --for 8h --concurrency 3
```

Owner and pull drains accept windows up to 24 hours. CLI `--for`, HTTP
`POST /api/workflows/auto` (`for_duration`), and MCP `orbit.workflow.auto`
(`for_seconds`) refuse a longer window before creating a run, with
`for_seconds must be a whole number from 1 to 86400 (24 hours)`. The CLI
keeps its one-pass shorthand when `--for` is omitted or zero; HTTP asks for one
pass explicitly with `one_pass: true`, and MCP requires a positive window.

The dashboard's **Drain** › **Start a window** offers the pull on a replica:
**Pull from the owner** posts `pull` (and `host`, resolved as `--host` is) with
`for_duration` or `one_pass`, `concurrency` and `allow_crew` to
`POST /api/workflows/auto`, and refuses the options `--pull` refuses in the
command line's words. On a replica running a pull window, the Drain page's
**Pulling from the owner** card shows the claim budget, the claims by stage and
any refused settlement from the run's detail. Its budget uses the allocator's
workspace-wide `pull_occupancy` reading (`occupied` and `reviewing`), counting
launching admissions, terminal leaves awaiting settlement and other workspace
leaves once each. Review credit is capped at the live concurrency ceiling:
`occupied − min(reviewing, N) < N`. An unavailable reading shows an unknown
budget. The claims-by-stage table separately lists only this drain's running
claimed leaves; an empty table does not mean capacity is free.

`<selector>` is the owner's host-qualified selector from federated discovery
(`orbit_workspace_list`, e.g. `hm_owner/ws_orbit`); an owner with no host entry
(`orbit host add`) is refused as an unknown selector, and the message says so. Before anything is
submitted, the command refuses unless:

- this checkout is a **replica**, and the selector names **its** owner machine
  and **its** logical workspace;
- the owner answers the probe **as that machine** and would admit this
  executor now (binary, protocol schema, before-PR review, ship mode);
- this host can resolve the owner's before-PR review crew, when the owner's
  `review.timing` is `before_pr` (`before_pr_reviewer_unavailable` otherwise);
- every `--allow-crew` name is a crew this host configures (none blank).

Without `--for` (or with `--for 0s`) the drain makes **one** admission pass:
it requests up to `--concurrency` claims once, admits no replacement as they
settle, and ends when they have. Over an empty backlog it asks once and ends.
A stop or cancel before that pass takes it away; a retried or resumed drain
never makes a second one.

`--allow-crew sol,luna` restricts which crews this drain declares to the owner
for its whole life, resume included: the owner hands it only tasks on those
crews (or on a crew resolving to the same provider and model), and other tasks
stay in its backlog on their own crews. It changes no configuration, task crew
or owner pool. The owner's before-PR review crew is not restricted by it but
must still run here. If nothing allowed is runnable, each pass reports
`no_runnable_crew` and requests nothing; restart without the flag or with a
crew that runs here.

A pull drain never approves `proposed` tasks: `--pull` conflicts with
`--approve-proposed`, and only the owner approves work. The replica's dashboard
Drain card follows the same rule: readiness reports `replica: true` and the
**Proposed tasks** control is disabled with that reason. Start an
approve-proposed window (`orbit run auto --approve-proposed`, or **Approve
qualifying** in the owner's Drain card) on the owner.

Readiness uses one shared snapshot across `orbit run readiness`, MCP
`orbit.workflow.auto` with `action: "status"`, and
`GET /api/workflows/auto/readiness`. MCP status accepts `task` (one id or a
list), `concurrency`, `limit` (1–500, default 50), and `allow_crew` (configured
crew names). HTTP accepts `task` and `allow_crew` as comma-separated query
values, plus the same concurrency and limit. Omitted crew selection uses the
live drain's restriction, if any. All three JSON responses include `replica`
and `validation_env_warning` (null when no warning applies). CLI text prints
that warning when present. Selection never reserves work or starts a drain.

Every admission receipt captures the owner's
`workflow.required_validation_commands` and `review.baseline_commands`, whether
or not review is on. A claimed leaf validates against those captured gates,
replays its implementer's sandbox deferral against them, and its review
confirms baseline claims against them. It does not use this host's lists.
A deferral or replay refusal names the gate list it checked and its source.
This host should still declare the same lists as the owner. `.orbit/` config
is untracked, so this host's copy can drift. A claim admitted by an older
owner build that captured no gates falls back to this host's lists, and the
leaf logs a warning. At submission the drain records the lists the owner's
probe reports. `orbit run auto --pull` prints a `Warning:` line naming each
list that differs, and the `pull-gates` row of `orbit doctor` reports the same
comparison for the latest drain. Copy the owner's values (`orbit config get`
on the owner) into this host's `.orbit/config.toml`. An empty required list is
not a refusal: the drain starts with a `Note:` line saying no required
validation runs here.

The drain is an ordinary durable run of `workspace_pull_pipeline`:

- On its first iteration the drain runs a provider preflight over every crew
  this host configures. A crew is runnable when it is enabled, its provider's
  executor resolves, and that executor's CLI is found where a leaf would
  launch it. The preflight starts no provider and checks no login; a provider
  with no signed-in user is caught by its first claimed leaf (below). The
  result is kept for the drain's window. Every pull request declares the
  runnable crews and this host's OS, and the owner admits only tasks this host
  can run. Tagged `no-diff-expected` work is claimable: the follower's leaf
  hands off `NoDiff` instead of opening a PR, needs no `no-diff.json` from a
  review, runs no required validation command or replay on its clean base
  (the owner accepts that skip without logs while its copy of the task keeps
  the tag), and refuses a change as `no_diff_expected_changed` (remove the
  tag to ship code). A skip from a leaf built before this exemption still
  carries the logs, which an upgraded owner accepts; a newer leaf's skip
  refused by an owner not yet upgraded as `required validation missing` stays
  pending on the follower and settles once the owner upgrades. Pin work that must stay on the
  owner with an `os:` tag or a crew. If no
  crew is runnable, the drain requests nothing and reports
  `no_runnable_crew`. After you install a missing CLI, start a new drain to pick it up.
  Auth failures may recover in the same window through a declared probe (below).
- Under `machine.auto_concurrency.mode = "enforce"` on the follower, each
  pull request names the work its host load forecast has room for, and the
  owner skips the rest as `capacity_unavailable`; they stay in its backlog.
  A recurring `auto-task:` template the follower has a profile of is judged
  by that profile and stays refused while it does not fit. A template the
  follower has no profile of yet is judged by its base class (`no-diff` when
  the task carries `no-diff-expected`, otherwise `change`), so a host that
  has never run a template can still claim it. Owner and follower must run
  builds with the same pull request fingerprint, so neither applies a rule
  the other does not (`protocol_skew` otherwise). See
  [`machine.auto_concurrency`](../CONFIG.md#machineauto_concurrency--host-forecast-admission).
- Each iteration first carries earlier admissions forward — retries an
  unanswered request under the **same** ID, binds, launches, and delivers a
  finished leaf's settlement — then, while the window is open, tops free slots
  up with new pull requests, each persisted before it is sent.
- Each claim runs as one local `task_claimed_pr_pipeline` leaf: implement,
  validate on the exact candidate, push, open the PR, hand off. The owner
  observes the PR itself and moves the task to `review`. **Nothing lands until
  the owner approves the handoff** on its dashboard.
  A leaf whose implementation proves no change is needed hands off `NoDiff`
  instead. The implementer writes `no-diff.json` (or `already-landed.json`)
  and its validation logs beneath `.orbit/tmp/`, returning their artifact paths
  and scratch `source_path`s as `no_diff_artifacts`. Commit imports these
  bounded files through the claim and verifies them before the leaf skips PR
  preparation and publication. Required task artifacts (such as ledgers and
  screenshots) use a separate `task_artifacts: [{path, source_path}]` output
  on both diff and no-diff deliveries, including `no-diff-expected` tasks.
  Sources must be regular files beneath the leaf's `.orbit/tmp/`, at most
  1 MiB each. Commit validates the entire batch before attachment and refuses
  missing files, scratch escapes, invalid or duplicate artifact paths, and
  names colliding with either clean-tree report or its declared logs, naming
  the refused path. The engine's `no-diff-handoff/<claim>.json` checkpoint
  and `validation/<claim>/` logs are reserved too, and so is the `review-`
  namespace (any ASCII case): only the review gate writes there, so a path in
  it is refused before attachment, naming the namespace and the path.
  `no_diff_artifacts` still admits only the report and its logs. Owner-mode
  agents continue using `orbit.task.artifact.put`.
  The handoff pins that verifier report; the owner rechecks it against its live
  base and completes without a PR under the same completion authority. A moved
  base or changed report requires fresh validation; a skip flag alone is refused.
  When the tests that show the base already resolves the task run only
  outside the agent sandbox, the implementer also returns `already_resolved`
  `{"selectors": ["cargo test -p <crate> --test <target> [<filter>]"]}`.
  `claim_validate` replays each selector on the base, outside the agent
  sandbox in a detached worktree, after the required commands, and pins each
  passing log beside theirs. A failing replay fails the claim with its output
  (the owner blocks the task); an empty list, a selector outside that
  grammar, or a run that executes no test or prints a skip is refused.
- Before a follower receives a claim, the owner excludes tasks whose immutable
  `required_tools` cannot be satisfied in a remote claimed implementer. The
  pull receipt names the required tool and restriction in `deferred_conflicts`:
  `orbit.task.update` is denied in claimed mode; owner coordination outside
  the closed bridge is unavailable. `orbit.search` and `orbit.task.list` are
  inside the bridge and satisfiable, so they do not hold a task. These tasks
  stay in backlog without a claim, worktree or failed leaf,
  and the follower may select another eligible task. Plain and auto-task
  records follow the same rule. The owner's ordinary local activity admission
  still decides its own requirements; no tool grant or requirement is changed.
- The implement step runs in **claimed mode**. The agent sandbox denies
  `~/.ssh`, so a sandboxed agent on a follower has no route to the owner; it
  does not need one. It works from the injected task envelope, is not granted
  `orbit.task.update` (nor is any recovery agent the leaf launches, such as
  `step_failure_recovery`). Its read-only `orbit.task.show` is scoped to the
  claimed task through the run's coordinator. It returns its execution summary
  in the step output. `claim_handoff` carries that summary in the typed
  handoff, and the owner writes it as the task's `execution_summary` when it
  accepts. The leaf's delivery gate judges that same summary, so a retry is
  not refused on the `Outcome: failed` a previous attempt left on the owner;
  do not clear it by hand. Do not loosen the sandbox or add SSH credentials to it to "fix" a
  leaf; an agent that reports an unreachable owner store is a prompt or
  binary mismatch, not a transport problem (check the follower's binary is
  current).
- An agent in a claimed leaf reaches the owner only through the run's
  coordinator (the step runner's broker, outside the sandbox), for a closed
  list of calls: `orbit.task.show` of the claimed task,
  `orbit.search` and `orbit.task.list` against the owner workspace the claim
  is bound to (a federated scope is refused, and neither is carried to any
  other workspace), `orbit.task.add` of a task `spawned_from` the claimed task
  and related to nothing else (a claimed `delivery-code-review`, `code-review`, `qa-sweep`
  or `qa-full-sweep` auto-task's finding may also name its culprit as
  `regression_from`; area tasks qualify through `no-diff-expected` together
  with `full-code-review` or `qa-full-sweep`. The owner checks that the culprit
  belongs to the claimed workspace and the claim is active),
  `orbit.friction.add` (during the claimed task, if it names
  one), and `orbit.task.artifact.get`/`put` on the claimed task. Its nested
  `orbit`, from the CLI or MCP, hands those calls over; the coordinator takes
  the task and claim from its own records, applies the activity's tool
  policy, and carries them to the owner over this follower's SSH route, where
  the claim fence refuses them once the claim is no longer active
  (`stale_claim`). Inside the sandbox, any other owner call is refused
  (`claimed_owner_bridge_refused`) and none is tried over SSH. The claimed
  implement step is still not granted `orbit.task.update` (above). A claimed
  task that must record a `regression_from` relation (a CI-failure sweep
  repair) returns the culprit task IDs as its implementer output's
  `regression_from`, and the owner records the relation when it accepts the
  handoff. A settlement refused with `regression_from attribution was
  refused` names a target the owner cannot resolve. Retries cannot change
  that, so recover the claim and requeue the task.
  A claimed review runs the duplicate `orbit.search` its brief requires
  through the broker, in the owner workspace, before filing a finding; the
  owner's triage remains the backstop. A search that asks for a federated
  scope (`workspaces`, `workspace_scope` or `all_workspaces`) is refused.
  A finding the owner still refuses arrives as the claimed task's
  `unfiled-findings.json` artifact: file each entry on the owner, keeping its
  relations. The implement step refuses `unfiled_findings` that is not an array
  of `{title, description}` objects, before the commit, push or PR-open steps
  run; its retry and recovery are the repair attempt. A candidate already
  published with plain-string entries (or a replay of its failed
  `claim_handoff`) still hands off: the handoff rewrites each non-blank string
  as `{title: <first sentence>, description: <the whole string>,
  normalized_from: "string"}`, sets `normalized_string_entries` in the
  artifact, and says so in the execution summary. Any other malformed entry, a
  blank string or a non-array field, is still refused.
- If the coordinator is missing or gone, the call fails as
  `owner_route_unavailable` and the agent ends its step on that code. The
  run skips step and final recovery, and the leaf releases its claim with the
  `owner_route` class (below): the task goes back to `backlog` on the owner
  and the drain requests no more work for the rest of its window. Check that
  the follower's binary and launch pass `ORBIT_PLUGIN_BROKER` to the agent's
  `orbit` before starting a new drain.
- The before-PR reviewer's `review-*` reads (`review-manifest.json`, its
  prior review evidence and the evidence the owner's hold names) and its
  `review-report.json` write take the same route and are also checked against
  the running review attempt. Refusals and their recovery are in the
  [claimed-review artifacts runbook](./claimed-review-artifacts.md); none is
  fixed by loosening the sandbox.
- An owner that refuses a request is checked against its receipt first: a
  committed claim is carried forward, and only a request the owner holds no
  receipt for is closed (`Refused`) and its slot returned.
- An unreachable owner is reported in the iteration output and retried; the
  drain never fails over to its own store.
- The owner's own drain survives the same waits. An admission pass that
  times out on the task commit lock or the database (`lock_busy`) admits
  nothing, releases any `enforce` gap-fit reservations it wrote for its
  wave (any it cannot release expire with their TTL), records `last_pass_error_code: lock_busy` with the lock error,
  which names the holder's pid and section, on
  `pipeline_state.drain_last_pass`, and waits the drain's poll interval
  before the next pass retries; `orbit run show` reads `Admission pass
  deferred: consecutive_failures=N`. Ten deferred passes in a row fail the
  drain with that error, and any completed pass resets the count. Other
  admission failures still fail the drain at once.
- An owner that times out waiting on its task commit lock or its database
  answers `lock_busy`. So does an owner whose `orbit` exits on its locked store
  before it answers the follower's SSH session (`session ended before answering
  'initialize'; ssh stderr: ... error: store error: database is locked`): the
  follower reads only the owner `orbit`'s final error, the last stderr line,
  and only when ssh carried the remote command's exit. An ssh failure (exit
  255) or a lock error quoted only by an earlier warning stays an unreachable
  owner, and a `tools/call` whose session ended that way stays
  `outcome_unknown`, because it may already have run. A
  leaf's owner reads retry it twice before failing, and a step it still fails
  releases the claim as `transient` and marks it `owner_busy`. The owner was
  reached and decided nothing, so the release blames neither the follower's
  host nor its crew. The owner honours the mark only on a `transient` release,
  and only for a task's first three busy releases within the release budget's
  24 hours: those do not count against the task's release budget, and later
  ones count like any other `transient` release, so a task the owner keeps
  failing is eventually blocked rather than cycling across drains.
  The drain backs its claims off instead: for 5 minutes from when the leaf
  ended it requests nothing (`owner_busy_backoff:` refusal,
  `crews.owner_busy_backoff` with `until`, `episodes` and `releases`), then
  requests again in the same window, and the owner may hand the same task
  straight back. Releases that end within 5 minutes of the previous one, such
  as several leaves that were running when the store locked, are one episode.
  A third episode in one window suppresses the host for the rest of the window
  (`host_suppressed:`, whose reason names the episode and release counts, and
  `crews.host_suppressed_by_busy_owner`), because an owner that stays busy
  across backoffs needs an operator; the follower host is not at fault, and
  the refusal points at the owner's lock holders instead of the host. Upgrade
  the owner before its followers: an owner that predates the mark drops it
  when it reads the release, records an ordinary `transient` release that
  counts against the task's release budget of two in 24 hours (so the third
  such release blocks the task), and holds the task from that drain for the
  rest of its window, while the newer follower still backs off rather than
  excluding the crew. When these recur,
  read the owner's `orbit.jsonl`: the `still waiting for advisory file lock`
  warning names each holder of the lock by pid, section and call site, and
  `advisory file lock held past its threshold` names a section that held it
  for 2 s or more. Its `slowest_step` and `slowest_step_ms` name where the
  hold went, and `steps` lists the largest steps by their own time: `pending
  recovery`, a lock wait such as `.task-commit.lock wait` (the partition) or
  a bundle lock's, and the operation's `index freshness`, `index snapshot`,
  `index query`, `envelope stamp write`, `index repair`, `bundle read`,
  `envelope read` and `search docs read`. A claim mutation's admission
  section splits into `claim receipt`, `claim authority`, `claim decision`
  and `claim commit`, and any commit into `commit intent`, `commit prepare`,
  `commit decision` (the SQLite transaction) and `commit apply` (the bundle
  writes and their syncs). `operation` is whatever of the
  operation those steps leave over, and `unattributed` is time no step
  claimed. A section queued behind a waiting admission or recovery
  names it as `queued exclusive waiter`, followed by the holders that waiter
  is waiting on.
- Each leaf settles itself when it ends ([ORB-13663]): its worker records
  the handoff (success) or a failure, then delivers it to the owner, retrying
  for a few minutes if the owner is unreachable and no live drain carries the
  claim. Every drain pass, a cancel or `orbit run auto --stop` delivers
  anything the leaf could not, and a drain pass also reconciles a launched
  leaf whose worker died so its settlement is recorded and delivered. A leaf
  that was cancelled before it launched releases its claim instead: the task
  goes back to `backlog` on the owner with a comment naming the drain. A
  pre-spawn launch failure also cancels the queued leaf and releases its claim
  with class `environment`, suppressing further pulls on that host for the
  window. If a child was spawned before registration or observer handoff
  failed, the supervisor stops and reaps it when possible; launch intent stays
  recorded until the run is reconciled. A pending run alone does not establish
  that the worker never executed, so this case is not released as unlaunched.
  A launched leaf that ends without its handoff settles with a typed failure
  class. Only `candidate` (the work failed) and `task_input` (final recovery
  rejected or archived the task) move the task to `blocked` on the owner, with
  a summary naming the leaf run, its failed step and that step's error. Every
  other class releases the claim: the task goes back to `backlog` with a
  comment naming the class and the reason. These classes are
  `operator_cancel` (`orbit run cancel <leaf-run>` or the dashboard's cancel,
  with its reason; `--block` fails the claim and blocks the task instead), `provider` (the CLI failed authentication or its model was
  at capacity), `environment` (validation lacked a tool), `owner_route` (the
  leaf could not reach the owner), `baseline_red` (required validation fails
  on the base exactly as on the candidate; the owner holds the task until the
  base passes. When the follower has already recorded that command passing
  on the base branch's current tip, the release carries no hold and keeps
  the candidate, which the task's next claim resumes on that base), `transient` (validation could not reach the network after its
  reruns, the forge kept refusing the leaf's push past its retry window
  (`[forge_unavailable]`), an Orbit upgrade refused the leaf's agent mid-step
  (`[upgrade_pending]`, see [in-flight agent steps during a binary swap](upgrades.md#in-flight-agent-steps-during-a-binary-swap)),
  or the leaf's worker died) and `base_conflict` (the committed
  candidate could not be synchronized onto a base that moved). The failure
  breaker does not count a release. When the forge refuses a claimed PR
  leaf's push for a server-side reason (`Internal Server Error`, `Service
  Unavailable`, `missing necessary objects`, and the like), the leaf keeps its claim and retries the push
  of the same reviewed head, past the usual backoff budget, for up to two
  hours from the first refusal (the pipeline's `forge_retry.window_ms`). The
  claim stays live and the leaf's runtime stays busy, so an upgrade waits
  for it as for a long agent step. Once the forge accepts, the same leaf
  pushes that head and opens the pull request without implementing or
  reviewing again. When the window closes first, the leaf releases the claim
  as `transient` and the release names the held head, its target ref, the
  attempts and the first refusal; that candidate then stays only on the
  follower. A forge release blames neither the crew nor the host: the drain
  keeps offering both, and the owner may hand the task straight back to the
  same drain, whose next claim continues the kept candidate. After
  `operator_cancel`, the owner withholds only the cancelled task from that
  drain for the rest of its window. The release still counts against the
  task's release budget and keeps its candidate and settlement evidence;
  unrelated work and required reviewers on the same crew remain eligible.
  Any other `transient` release stops the drain offering that crew for the
  rest of its window. After
  `provider`, an authentication failure stops every crew of that provider
  (an `anthropic` crew is the same provider as `claude`); a capacity failure
  stops only the crew the leaf ran, and only for a backoff of 10 minutes
  from when the leaf ended, doubling with each repeat for that crew in the
  window up to an hour, after which the drain offers it again. A usage limit stops no crew by itself:
  the reading the leaf recorded on this host excludes the provider's crews as
  `provider_limit` until it lapses. The leaf itself ends `held`, not
  `failed`, with a `provider_limit` step naming the provider and the reset,
  so it stays out of `orbit run history --state failed` and Incidents lists
  it as `deferred`.
  After `environment` or `owner_route` — failures of the host itself — it requests
  no more work at all for its window (`host_suppressed:` refusal,
  `crews.host_suppressed`); fix the host and start a new drain. An owner whose
  store stayed locked is not one of these: its `transient` release backs the
  drain off briefly instead (`owner_busy_backoff`, above), and only repeated
  episodes suppress the host, with guidance to clear the owner's lock holders. In either
  case (forge releases, and busy-owner releases within the task's allowance of three,
  aside) the owner does not hand the released task back
  to that drain, so it is not pulled straight back; another drain may still
  take it. When the leaf had
  committed a candidate, the release or block names it, and the task's next
  claim continues it rather than starting over, unless the task's spec
  changed or `orbit task update --discard-candidate` discarded it since. A
  claimed PR leaf that fails after its commit and before its push carries
  the candidate to `refs/orbit/candidates/<task>/<run>` on `origin`, and the
  release comment names that ref, so a claim on any host can fetch it. When
  that push fails (no push access, `origin` unreachable), the release says
  why and the candidate stays only on the follower that made it: a claim on
  that follower still continues it, and a claim on another host implements
  fresh. Claimed-local leaves run on the owner and continue its candidates
  from its own repository. Returning a blocked task to the backlog for an
  owner-local run (tagging it `os:linux` after a follower's review failure,
  say) keeps its candidate too: the owner's own `task_pr_pipeline` continues
  the candidate the failed claim kept, and its `resume_candidate` output
  names the follower's run with `source_machine_id`.

  To requeue a claim stranded before landing — it pushed its candidate and
  opened its pull request, then failed in its before-landing review or a
  later step, so the owner blocked the task — run
  `orbit task update <task> --status backlog` once the cause is fixed. The
  move keeps the candidate; only `--discard-candidate` or a spec change
  drops it. The task's next claim is handed the candidate with its pull
  request, continues on the same branch and pushes there under a lease on
  the published head, so its `pr_open` reuses that pull request and the
  task keeps one open PR. If the branch advanced or was deleted on the
  remote since that head, the push fails as `push_lease_lost` and the
  remote branch is left as it is, even when the base also moved; a person
  decides what to do with the newer commits. That lease is the adopted
  published head. It is separate from a before-landing repair preserved
  after the pull request merged (`review_repair_after_merge`). Its `resume_candidate` output names the branch it
  took over as `reused_branch`. A claim on the host that made the candidate
  renames the earlier worktree's branch to `<branch>-superseded-<suffix>`;
  that worktree and its commits are untouched. When the branch cannot be
  reused — a local copy at another commit, a branch never pushed at the kept
  head, a candidate this host could not restore — `branch_reuse_refused` says
  why, the leaf publishes on its own branch, and its `pr_open` closes the
  earlier pull request with a comment naming the new one. Check
  `superseded_pull_request` in that step's output (`orbit run show
  <leaf-run> --json`): `close_failed` means the earlier pull request is
  still open, so close it by hand. To start over instead, add
  `--discard-candidate`; that move closes the earlier pull request with a
  comment. A move made while a run or claim of the task is still live closes
  nothing, so make it after the claim settles. Either way, whichever
  run publishes the task's next pull request closes any other open Orbit
  pull request of the task (`closed_stale_pull_requests` in its `pr_open`
  output), so a re-admitted task keeps one open pull request
  ([`[pr]` rules](../CONFIG.md#pr--pull-requests)).

  A leaf can also commit past the head it pushed and then fail: a
  before-landing reviewer's fix is committed in the leaf's worktree before
  the review settles, so a rejected or incomplete review leaves it there
  unpushed. The leaf's failure hook then carries its local branch tip,
  reviewer fix included, to `refs/orbit/candidates/<task>/<run>` on
  `origin`. Its `claim_candidate_carry` checkpoint reports `carry: durable`
  with `head_sha` (the carried tip) and `published_sha` (the head on the PR
  branch), and the settlement keeps the carried tip with that durable ref
  and the pull request. The task's next claim on any host resumes that tip,
  so the reviewer's fix is not lost when the worktree is collected or the
  task moves to another host; the review record's reviewer commit is
  reachable from that ref. Because the carried tip was never pushed to the
  PR branch, the leaf does not take over the branch: `branch_reuse_refused`
  names the unpushed head, and its `pr_open` supersedes the earlier pull
  request as above. When the local tip is the pushed head, the hook reports
  `carry: published` and pushes nothing. When the carry push fails, it also
  reports `failed`, with the local `head_sha`, the earlier `published_sha`
  and `reason`; the settlement keeps the reviewer-fixed tip as host-local,
  so the task's next claim on that host resumes the fix. A claim elsewhere
  implements fresh rather than treating the older published head as the
  fixed candidate; recover the local tip before its worktree is collected.

  Every fresh start that
  sets a kept candidate aside is in the owner's task history as a
  `candidate_resume` event whose note begins `fresh:` and names the reason
  (`not_durable`, `spec_changed` or `discarded`; `reason_code` `not_durable`,
  `spec_changed` or `candidate_discarded` on an owner-local run), with the
  claim and the machine that committed it: `orbit task show <task>` shows it. The
  same holds the other way round: an owner-local run held for a red base, a
  missing validation tool or a provider failure pushes its candidate to
  `refs/orbit/candidates/<task>/<run>` too, and the task's next claim on any
  host continues it. When that push fails, the hold comment says the
  candidate is host-local and quotes the push error, and a claim on another
  host implements fresh with a `not_durable` reason. Carried refs are not
  deleted automatically; once a task is done, prune them on `origin` with
  `git push origin --delete refs/orbit/candidates/<task>/<run>`, listing
  them with `git ls-remote origin 'refs/orbit/candidates/*'`. A task
  released twice within 24 hours for a typed failure class is blocked by the
  next such failure, with one comment listing every reason; unblock it once the cause
  is fixed. The full diagnostic stays in the follower's run
  (`orbit run show <leaf-run>`, and `.orbit/state/logs/<leaf-run>.worker.log`
  on the follower). That run page carries a `Claim:` line (`pull_claim` in
  `--json`): the owner task, claim, owner selector and admitting drain,
  whether the leaf's outcome has reached the owner, and its `failure_class`.
- `orbit run show <drain-run>` lists the crew window as `Crews:` lines
  (`crew_window` in `--json`; the dashboard's run detail shows the same
  panel). The first line names the runnable crews. Each excluded crew is
  listed with its source and the reason: `preflight` (disabled, executor
  unresolved, or CLI not found), `provider_unavailable` (a claimed leaf's
  provider could not authenticate, which lists every crew of that provider,
  or reported its selected model at capacity, which lists that crew, with
  the task and error, written `(provider_unavailable until <time>)`: a
  capacity exclusion lifts at `until` within the same drain), `leaf_released` (a claimed leaf was
  released for a `transient` failure other than a forge outage, with the
  task, class and reason) or `provider_limit` (this host's latest reading of
  the crew's provider usage window is at or over its threshold, written
  `(provider_limit until <time>)` before the reading and `reading from host
  <machine-id>` after it; it lifts at `until` within the same drain. These
  readings come from the executor's local store, independently of the owner's
  login and usage limits. See
  [provider usage limits](../CONFIG.md#provider-usage-limits)). Each iteration's output carries
  the same window as `crews`. Auth exclusions also list provider, host, failure time, error class,
  re-login hint, credential source and next probe time (`auth_exclusions`).
  Doctor warns about these on live drains; the dashboard shows the same data.
  Follow the hint on the named host. A declared `auth_probe` first runs after
  ten minutes, backs off to twenty then thirty minutes on failure, and
  re-admits the provider's crews when it passes. Recovery is durable across
  restart/resume; a later auth failure starts a new delay. Only active auth
  exclusions are probed while the drain is admitting. Claude ships a minimal
  Haiku probe; other providers currently need a new drain after re-login.
  See [executor authentication recovery probes](../CONFIG.md#executor-authentication-recovery-probes)
  for the declaration and credential-route details. Missing launchers,
  refusal, crew and host exclusions retain their existing behavior.
- When a capacity backoff holds the owner's review crew, the drain requests
  nothing until it lifts: its `Requests held:` line is a
  `before_pr_reviewer_unavailable` hold that names the review's timing
  (before-landing or before-PR), the crew and the time it is backed off
  until. No action is needed; the drain admits again on the first pass after
  that time.
- **Failure breaker.** After three consecutive claims this drain admitted
  settle as failures, its breaker opens: the drain stops requesting work
  (`circuit_open`) and only keeps settling. Only failures that could be this
  host's count. A released claim (a provider failure or hold, a red base, a
  review hold for owed host evidence, a forge hold, a cancel) and Orbit's
  refusal of an implementer's handoff (`implementer output rejected before
  delivery`, such as a deferral naming a command outside the owner's gate
  list) neither extend nor reset the streak. A refused handoff still blocks
  its task for an operator to requeue.
  - The breaker does not latch. After a cooldown, `breaker_cooldown_seconds`
    in the drain's input (default 900, 15 minutes), it half-opens. It then
    admits one probe claim, once nothing the drain claimed is unsettled;
    while it waits the refusal reads `circuit_half_open`. A probe that hands
    off ends the streak and closes the breaker. A probe that fails reopens it
    with the cooldown doubled for each failure past three, at most eight
    times the cooldown (two hours at the default). A probe released back to
    the owner leaves the streak unchanged, and a later pass probes again.
    To change the cooldown, override the `workspace_pull_pipeline` job's
    `breaker_cooldown_seconds` default in the workspace catalog.
  - `orbit run show <drain-run>` prints the breaker on a `Breaker:` line:
    open or half-open, the failure count, the failure classes newest first,
    and when it half-opens (JSON `pipeline_state.pull_breaker`: `failures`,
    `failure_classes`, `opened_at`, `half_open_at`; absent while closed).
    The refusal is the `Requests held: circuit_open: …` line, and
    `orbit run readiness` on the follower prints
    `Pull drain requests held: circuit_open: …` (JSON
    `pipeline_state.drain_last_pass.admission_refusal` and
    `capacity.pull_drain_refusal`). Each iteration's output carries
    `consecutive_failures`, `breaker` and `breaker_state`
    (`closed`, `open` or `half_open`).
  - While it is open, inspect the blocked tasks and their leaf logs, fix the
    cause, and re-backlog them deliberately. The breaker probes again
    by itself. To skip the cooldown, start a new drain, which starts with the
    breaker closed. A change to the drain's config does not reset the breaker.
- The run outlives its window until every admission has settled, so a leaf
  that finishes late still hands off, and a later drain for the same owner
  carries anything an earlier drain left behind. A second `orbit run auto
  --pull` started meanwhile queues behind it and requests nothing until that
  tail ends. To keep a drain requesting, extend it instead (below).
- Each iteration reclaims the `target/` build output of every settled leaf
  whose worker has exited (`reclaimed_build_bytes` in the iteration output)
  and keeps its checkout. A leaf's `target/` runs to gigabytes, so follower
  disk does not wait on a GC schedule. `orbit gc worktrees --confirm` on the
  follower can then remove a leaf's checkout on the strength of a locally
  settled, accepted handoff alone. Released claims and other settlements,
  including handoffs the owner refused as obsolete, still require the task's
  status, read from the owner over the claim's route. A release returns the
  task to backlog, so its checkout is retained. A worktree kept
  as `skipped:owner_unreachable` carries the transport error in `detail`;
  `skipped:no_owner_route` means the follower has no route to ask (owner
  not registered with `orbit host add`, or an unregistered
  checkout), not that the owner is down. A status lookup failure without a
  transport error is `skipped:owner_lookup_failed`, with the reason in
  `detail`; it does not establish that the owner is down.

Operate it with the ordinary run commands: `orbit run show <run-id>`,
`orbit run concurrency <run-id> --set N` (MCP: `orbit_workflow_auto` with
`action: "resize"`), `orbit run extend <run-id> --for 4h [--concurrency N]`,
and `orbit run auto --stop` (closes the window; live leaves keep running and
still settle).

`orbit run extend` moves the drain's deadline to the later of its current one
and now plus `--for` (at most 24 hours from now), on the same run: its leaves,
claims and settlements are untouched, and `--concurrency` retunes it as
`orbit run concurrency` does. It also works in the settling tail after the
window closed, and the drain resumes requesting: each pass reads the window
from run state, not from the window answer its loop forwarded. The pass that
finds the window closed and nothing left to settle records the close in the
same transaction that rereads the deadline, so an extension accepted before
it keeps the drain admitting and one requested after it is refused, never
reported as accepted on a drain that is ending. A shorter window is a no-op
(`outcome: unchanged`). It refuses a drain whose admissions were stopped
(`--stop`) or that is being cancelled, since an extension never reopens one,
and a drain started without `--for`, which makes its single pass only. Each
request is audited on the run (`pipeline.run.window_extend.completed`, with
`previous_deadline` and `deadline`), and `orbit run show` prints the
`Window: extended to …` line. The job's loop ceiling (6000 iterations, each
idle one waiting at least `poll_sleep_seconds`) still bounds a drain extended
over days; past it the drain ends and a new one carries its admissions. The drain's `--concurrency`, as last
retuned, is the only ceiling on its leaves: the claimed leaf jobs declare no
active-run limit of their own. On the follower, `orbit run readiness` with no
`--concurrency` and no local drain reports that ceiling. Text prints
`Active leaf runs: <occupied>/<ceiling>` and the free slots left under it, plus
a `Leaf ceiling:` line. `--json` sets `capacity.max_active_leaf_runs`,
`capacity.free_slots`, and `capacity.limit_source` to `pull_run_input` for the
submitted value or `pull_run_control` after `orbit run concurrency --set` (or
`orbit run extend --concurrency`). A live local drain still supplies the
ceiling instead (`run_input` or `run_control`), and with neither drain live
the report stays at 5 (`default`). A leaf in its before-landing review (the owner's
`review.timing = "before_landing"`) runs no implementer but keeps its claim until it
settles. Admission counts `occupied - min(reviewing, --concurrency)` against
`--concurrency` before each claim. Reviewing starts after the persisted
before-landing admission has `applies=true`; validation and publishing before
that boundary count as implementing, while later review revalidation and
handoff remain reviewing. The replacement credit is capped at
`--concurrency`, but reviewing leaves can exceed it as replacements reach
review. With an unchanged ceiling and no pre-existing excess, total occupied
leaves stay at most twice the ceiling and implementing leaves at most the
ceiling. Lowering it preserves running leaves, so existing excess can remain
temporarily. Owner/local drains continue counting all in-flight children
against their ceiling. `orbit run show <run-id>` counts the `Claimed leaves:` it
carries by stage (`.claimed_leaves[].stage`), and so does the dashboard's run
page. Review rebuilds still compete for repository build-budget slots. `--for`
is at most 24 hours. Ship-sweep
enablement is unchanged by any of this.

To close the feature entirely, set `DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED`
to `false` in `orbit-core` and rebuild: it is a source constant, not
configuration, so no key or environment variable can open or close it.

## Claim inspection and manual recovery

List claims on the owner. This tool is off the MCP surface and requires
operator capability:

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
```

It reports phase, age, reservation expiry, execution machine, bound run, last
event, unresolved merge intent, and landing invalidation. **Nothing in this
listing reclaims, rebinds, or repairs a claim.**

A claimed leaf's run lives in the follower's store, so the owner's
`orbit run history` never lists it. The owner keeps the failure or release
settlement each leaf sent: its kind, evidence class (`provider_unavailable`,
`baseline_red`, `forge_unavailable`, `evidence_hold`, `final_recovery`,
`failure`, or `summary` for an untyped one), typed failure class, crew and the
model it resolved to (`crew_model`, absent from a follower that predates it),
failed step and bounded reason. A typed handoff keeps the same record for the
agent scorecards, but this listing leaves it out. Settled claims carry no in-flight attempt, so this
listing needs no operator capability; the `run-failure-patterns` auto-task
reads it:

```bash
orbit run settlements --since 7d --no-reconcile --json
```

Claims settled before the owner kept settlements read `unrecorded` unless
their release kept a typed class; the task's history note carries the reason.

The owner's dashboard shows the same state, plus the accepted handoff, inside
the task detail it belongs to — there is no distributed tab, and the panel
appears only for a task this workspace holds a claim for:

```bash
orbit web serve --operator
```

`GET /api/distributed/claims` is the read. It accepts `task=<task-id>` and
`state=active|settled|all`; state defaults to `active`, and settled claims are
compact summaries unless `detail=true`. The task detail panel sends its task
filter with `state=all&detail=true`, so it fetches only that task's claims and still
shows their settled history. A replica answers that the owner
machine holds claim state rather than showing an empty list. Read-only
inspection needs no operator authority; the three actions below do, and the
server re-resolves that for every call regardless of what the page rendered.

Also inspect the task, lock surface, and recorded run:

```bash
orbit task show <task-id>
orbit task locks list
orbit run show <run-id>
orbit run events <run-id>
```

`job_run_machine` on the task names where the bound run lives; a host pointer is
not remote reachability. Inspect that run on the execution host.

### Interrupted claimed-run resume refusal

A claimed leaf cannot use generic resume, whether its claim is still live or
has settled. `orbit job resume` creates a new run, which cannot inherit the
claim/run binding, and a worker refuses a claimed leaf definition that no
claim binds:

```bash
orbit job resume <run-id>
```

Expect a validation error naming deliberate recovery, not a new attempt.
Resuming an older local run after another host has bound the task is the same
refusal: the error names that host's run and machine, and it does not reuse
the old checkpoints. Continue the binding on that machine, or admit a new
attempt only after an authorized rebind.

A claim is one delivery attempt, and its review ledger is keyed by the claim.
Claimed before-landing and before-PR review ledgers stay on the follower until
settlement copies them to the owner. When final recovery escalates an
exhausted lineage, its `human_action` names one owner command:

```bash
ORBIT_OPERATOR=1 orbit --root <owner-global> --workspace <owner-workspace> task requeue <task-id> --reason '<decision>'
```

`--root` is the owner's Orbit data directory and `--workspace` is the
registered workspace. Both are required: `--root` alone does not select a
checkout, so the command is written to work from any directory. Run it on the
owner; a replica refuses it. The requeue returns the task to the backlog. Its
next claim is a new owner admission with its own leaf run, worker binding and
review budget, and it continues the candidate the failed claim kept. See
[the review gate](./review-gate.md).

The same requeue applies to any claimed leaf that failed or escalated with a
committed candidate, such as a final recovery that escalated a review finding.
The claim's settlement records that candidate on the owner's task as a
`claim_candidate_kept` history event (`orbit task show <task>`). The event
names the commit, its branch, its durable ref or pull request, the leaf run
and its machine, and the step the leaf stopped at. A leaf that failed before
it committed keeps nothing and records no event. Without
`--discard-candidate`, the requeue reports the candidate it resumes:
`Resumes ... at <head>.` in text, or `resumes` with `source: claim` in JSON.
A subsequent claim continues that candidate, and its `resume_candidate`
output names it as `source_sha`. An owner-local run also continues it when
the task still names a foreign run, or the owner-local leaf that kept it.
If a later owner-local attempt stops without preserving a candidate, the next
owner-local run implements fresh instead. In that case `resumes.owner_local`
is `false`, while `source_run_id`, `machine_id` and `head_sha` identify the
candidate still offered to claimed runs; `next_step` explains both routes.
When the kept candidate was never pushed (`host_local: true`),
only a run on the machine that made it continues the candidate; a run on
another host implements fresh. If the task's description or acceptance
criteria changed since the candidate was kept, the requeue is refused and
nothing changes, just as for a published owner candidate. Requeue with
`--discard-candidate` instead to start over: it discards the candidate,
closes the task's undelivered pull requests, and the next claim implements
fresh.
In-run step retries of the **same** bound run are different: they keep the
claim. Crash recovery before launch may recover the same queued run; it must
not restart a run whose execution became uncertain.

Deliberate recovery is operator-driven:

1. Inspect the claim, run, branch, and any PR or local candidate.
2. Reconcile an uncertain merge intent **before** reassignment. Database
   revocation cannot cancel a request already sent to GitHub.
3. Revoke the old claim, invalidate pending landing authority, release only
   that reservation, and choose the task transition in one authorized recovery.
   On the owner's dashboard, use **Recover claim → blocked** to diagnose or
   **Recover claim → backlog** to retry. Both controls require a reason and the
   phase you were shown, and refuse if the claim moved on. There is still no CLI
   verb and no registered tool for it; do not invent one, and do not admit a
   second attempt by shipping the task again while the old claim is unfenced.
4. A sleeping worker that returns receives `stale_claim`. Its local compute
   and an in-flight GitHub write cannot be undone; its old candidate cannot
   become authoritative.

Owner-local landing of an already-authorized handoff does not need a drain or
ship-sweep. Followers never merge. Review-only work stays in `review` until
explicit completion authority is recorded.

### Dead leaves and their pull requests

A delivery leaf can die after it opened its pull request. The recovery path
depends on where it ran:

- **Claimed leaf (follower).** Its claim's release keeps the candidate and the
  pull request, and the task's next claim continues them on the same branch.
  Generic resume is refused, as above, and no drain adopts the run.
- **Owner-local leaf.** The owner drain's next pass adopts it when it holds
  the leaf's `--complete` and `--auto-fix` grants and the task is still in
  `review`. The pass resumes the interrupted run from its checkpoints, once
  (never a resumed attempt), so completion and auto-fix continue on the same pull
  request and head. A leaf that died before promoting its task leaves it
  interruption-`blocked`, which the blocked-task recovery backstop decides. Every rule and every refusal is listed in
  [stuck job runs](stuck-job-runs.md#a-delivery-leaf-that-died-after-opening-its-pull-request).
- **Owner-local leaf that stopped with a blocked pull request.** An
  `--auto-fix` owner drain hands a conflicting or red Orbit pull request whose
  task has no live run back to its newest failed, timed-out or interrupted leaf,
  rewound to completion
  ([out-of-run auto-fix](stuck-job-runs.md#a-conflicting-or-red-pull-request-no-run-is-watching)).
  Claimed leaves and tasks of other workspaces are left alone.

When the task cannot be resumed, requeue it. An operator's move to `backlog`,
or a `--discard-candidate` requeue out of `blocked`, closes the task's pull
requests that no live run or claim is delivering. Whichever run publishes the
task's next pull request closes any other open Orbit pull request of the task.
Either way, the task keeps one open pull request
([`[pr]` rules](../CONFIG.md#pr--pull-requests)).

### Requeue or salvage an interrupted review

A claimed leaf that is cancelled or interrupted while its reviewer is working
is not failed. Its uncommitted repair is kept, and the leaf's run state
records what was kept under `interrupted_review` (`orbit run show <leaf-run>
--json`). This covers `orbit run cancel <leaf>`, a forced drain cancel, or a
worker the orphan reconciler finalizes.

- The host that stopped the leaf commits the reviewer's edits as one
  reviewer-authored commit, `review: partial reviewer repairs preserved`. The
  commit carries the `Orbit-Review-Run`, `Orbit-Review-Step` and
  `Orbit-Review-Attempt` trailers, and its parent is the reviewed candidate
  head. It is kept at `refs/orbit/candidates/<task>/<run>-review`. A cancel
  pushes that ref to `origin`. Reconciliation, which also runs when a
  workspace opens, keeps the ref local to that host. The candidate branch, the worktree and
  any pull request are left as they were.
- The release keeps the candidate, its pull request, and the stopped review:
  the step, the attempt and the repair ref. Its comment on the owner's task
  names the ref. The owner adds the verdict and findings from the attempt's
  `review-report.json`. A forced drain cancel records the release before it
  stops the leaf, so that release names the ref the stop is about to write.
- The task returns to the backlog, and its next claim resumes the candidate.
  `resume_candidate` resolves the repair commit, fetching the ref from
  `origin` when it is missing. It applies the repair only when the commit is
  that run's partial repair on that head. The implementer then runs with
  trigger `review` and `repair.interrupted_review`: the findings,
  `repair_applied`, and `repair_refused` when the repair was not applied. A
  published candidate continues its branch and pull request (`reused_branch`).
  The resumed leaf is a new admission. Validation, footprint locks, the build
  budget and the review policy it captured all apply to it as to any leaf.
- An unpublished candidate is durable through its repair ref once the ref
  reached `origin`, because the ref's parent is the head. A forced-cancel
  release, or a reconciled leaf, may name a ref that never reached `origin`.
  A claim on another host then implements fresh, with reason `not_durable`.

To requeue, do nothing: the release already returned the task to the
backlog. Check that the owner comment names the repair ref and that
`git ls-remote origin 'refs/orbit/candidates/<task>/*'` lists it. If the
repair should not be continued, run
`orbit task update <task> --discard-candidate --status backlog` instead. To
judge whether the candidate is worth continuing, follow the orbit-orchestrate
skill's "Salvage an incomplete candidate" procedure. It diagnoses with
`orbit run agent --cwd <preserved worktree>`, which never commits, pushes or
edits a pull request, and then routes the task by cause. Prune `-review`
refs with the other carried refs once the task is done.

An owner's own run stopped mid-review keeps the same commit and ref, and a
task comment names them. Its next run does not apply that repair
automatically. Use the salvage procedure to fold it into the remaining work.

### Let the owner land follower deliveries

To land follower deliveries the way `orbit run auto --complete` lands the
owner's own tasks, set this on the **owner** checkout, then restart its
long-lived processes so they read it:

```toml
[workflow]
distributed_completion = "done"
```

Followers pick it up on their next probe. A request built from the old
contract is refused with `ship_contract_mismatch` and retried with the new
one. From then on:

- Each accepted handoff is authorized in the same transaction that moves the
  task to `review`, and `task_landing_pipeline` starts at once. The task
  reaches `done` when the landing job has verified the merge.
- A provider transport failure (a TLS or request timeout, a reset
  connection, an HTTP 5xx) that interrupts the landing before it records a
  merge intent or requests a check rerun or branch update is retried by the
  same landing job against the same handoff: at most three passes, 20s and
  then 60s apart, within the job's check-wait budget. This includes the
  required-check evidence read for a blocked pull request; recovery still
  requires fresh check evidence for the accepted head. A credential,
  permission or rate-limit refusal is not retried. A landing job that still
  fails, for that or any other reason, records its attempt `stopped` with
  the error, so the dashboard and `orbit.drain.claims` show the stop instead
  of an attempt left `dispatched` behind a finished job. A failure after the
  merge request was sent is never resent: the attempt keeps its unresolved
  merge intent, and the next attempt (**Retry landing**) reconciles that
  intent against the pull request before anything else.
- When a pull request's hosted check fails, the landing classifies each
  failed job from its failed-step log (the CI sweep's log read and error
  signature) before it stops anything:
  - **inherited**: the same signature fails on the candidate's base commit,
    or the landing branch has moved past that base and the same job passes
    on its tip. The landing updates the pull request's branch from the
    landing branch once and waits for the checks on the new head. No agent
    runs. If the same check still fails on that new head and the same job
    passes on the base tip, the failure is the candidate's: the landing
    stops for the one automatic repair, and the stop evidence carries the
    failed job, its signature and a log excerpt. A repair that is already
    spent returns the task to the backlog instead (below). A base that has not moved, and is still
    red, stops in `review` without a repair. `orbit run auto --auto-fix` uses
    this same classification for that unmoved red base: it spends no repair
    attempt, waits out its completion deadline, and stops with a base-red
    reason. It does the same when the base has moved and the tip's same job
    has not concluded; a concluded tip job, or a tip that lists no such job,
    is not that hold. The owner merges that head only
    when it is a clean merge of the accepted candidate with a commit already
    on the landing branch.
    When the owner checkout has `origin`, adopting a newly observed updated
    head requires a successful fresh fetch of the landing branch, even if
    the initial fetch failed and the head and base are cached. A failed
    fetch stops adoption without spending rerun, update or repair budgets.
  - **transient**: the log shows the network or the runner giving out. The
    landing reruns the failed jobs once and judges the rerun.
  - **candidate**: anything else. The landing stops for the claim's one
    automatic repair, the same repair a conflicting base gets, and the stop
    evidence carries the failed step's signature and log excerpt for the
    repair leaf.

  The one automatic repair is a new claim on the same task, admitted to a
  pull. While the stopped claim is `repair_pending`, the task is
  `in-progress`. The repair is reserved for the executor that delivered
  the candidate, because it already holds the candidate's objects. Its next
  `orbit run auto --pull` pass takes the repair before any backlog task. No
  other follower ever takes it. If that executor stops pulling (its
  admissions stopped, its host offline, its window over), the owner's own
  `orbit run auto` drain takes the repair 30 minutes after the landing
  stopped:
  - Until then, each pass of the owner drain lists the task in its last
    pass's waiting tasks (`orbit run show <drain>`, the dashboard Drain
    card) with reason `repair_reserved`. The detail names the original
    executor and the time the owner takes over.
  - After that, the pass starts one repair-only pull drain against the owner
    itself, a one-pass `workspace_pull_pipeline` child of the drain marked
    `owner_repair_pull`. It reports the task as `repair_owner_fallback`,
    naming that pull run. The owner admits the pull's request under its
    local identity. It offers only pending repairs, never a backlog task.
    The owner wave admits the backlog.
  - The repair takes one of the drain's free slots. It passes the same
    crew screen as the drain's `--allow-crew` list, and the same OS,
    browser-kit and capacity screens as any pull. The repair pull inherits
    the owner drain's total workspace leaf ceiling, so unrelated active
    leaves count once against that ceiling. The owner reserves the due
    repairs' free slots separately from its backlog wave, and each pull
    allocation atomically checks shared occupancy before admitting work.
    The superseded claim
    settles `revoked` in the same commit, and the repair leaf runs on the
    owner as a claimed leaf.
  - An owner drain whose admissions are stopped, or that is held by a
    scheduled shutdown or has no free slot, starts no repair pull. Its pass
    says what the repair waits for.
  - A repair pull that ends without taking the repair, for example because
    its crew is not runnable here, has its own last pass, which says why.
    The owner drain starts another one 10 minutes after that pull ended.
  - To hand a repair back to its original executor before the owner takes
    it, restart that executor's pull drain.

  A **candidate** failure once that repair is spent (the claim is the
  repair, or the task already returned to the backlog for a landing repair)
  does not leave the task in `review`. The owner ends the claim, keeps its
  candidate with the stop's reason, and returns the task to the backlog
  with history event `landing_repair_requeued`. The task's next run resumes
  that candidate and repairs it with the failed check and its
  signature in its prompt rather than implementing afresh. It opens its own
  pull request, which closes the old one. A task returns to the backlog at
  most twice this way. The next such stop blocks it (history event
  `landing_repair_exhausted`), with a comment that carries that stop and
  every counted earlier stop. The candidate is still kept for the run after
  an operator unblocks the task, and that run starts with a fresh budget.

  A failed required check that no failed job answers, such as a provider
  app's aggregate beside passing analysis jobs, is classified too, so a job
  failure beside it never hides it. Its evidence is its own failure
  annotations, read as `--auto-fix` reads them (see
  [PR auto-fix repairs](../CONFIG.md#pr-auto-fix-repairs)). It is
  **candidate** only when every annotated source file is one the candidate
  changed from its base. Annotations on other files, or annotations that
  cannot be read or validated, are **inconclusive**. A failed optional check
  that no job answers keeps its previous handling.

  After a rerun or branch update, a stale failure for a job already acted
  on waits while replacement jobs register. That wait does not hide a
  separately failed required aggregate: it is classified on the next poll,
  even when every workflow job now passes.

  Each of the rerun, the branch update and the repair is spent once. A
  failure past that budget, or one the landing cannot classify (its jobs or
  log are unreadable), stops the landing with the task in `review` and the
  classification in the stop evidence (`orbit task show <task-id>`).
  Owner PR refusals also retain a versioned JSON diagnostic in the existing
  landing-attempt evidence string (`kind: owner_landing_refusal`,
  `schema_version: 1`), alongside its safe human-readable `reason.value`.
  The task status note, the handoff revocation reason, the repair leaf's
  stop text and a blocked repair comment carry that human `reason.value`,
  not the diagnostic object. Read the object from the stopped attempt, whose
  attempt number and owner run bind it to the accepted handoff; retrying the
  same handoff preserves that first attempt's evidence. Older plain-string
  evidence remains valid and is still shown as recorded.

  The diagnostic records the accepted base/candidate, latest typed PR identity,
  review and rollup, owner-owed contexts, final observed classification, and
  whether the required-check read was collected, failed to read/parse, or
  disagreed about the head. Its allowlisted required-check projection includes
  the provider's `isRequired` observations, **not** a separately versioned
  branch-policy snapshot. Missing, null, unreadable, unknown and collected-empty
  observations must not be interpreted as passing checks.

  Capture adds no provider requests. Only the latest status observation is
  retained; `status_observations` counts the observations it replaces. Each
  collection retains at most 32 entries with original count and truncation,
  and observation strings are redacted before a 512-character limit with
  original character count and truncation. The human reason has a separate
  4,096-character limit, retaining its beginning and final disposition when
  truncated. Arbitrary payloads, URL fields and transcript archives are excluded.
  Workflow prerequisites and emission proof are explicitly `not_collected` by
  this diagnostic, even if separate recovery work obtained its own evidence.
  A later workflow/job timestamp therefore cannot prove what the initial API
  returned or that a required check would eventually appear. These observations
  grant no wait, repair or merge authority and do not change existing deadlines.

- An older published head can also lack a currently required context while
  every reported check succeeded. With captured landing authority (or the
  owner's `auto_fix` completion authority), recovery can update the branch
  once when authenticated PR-run, integration, effective-rule revision and
  complete workflow source evidence proves the old event cannot emit those
  exact contexts and the freshly fetched descendant base does emit them.
  The adapter supports literal PR synchronize triggers, literal branch lists,
  unconditional jobs on supported literal hosted runners and its bounded
  single-axis literal matrix names. It also
  recognizes the adapter's finite live-PR-state REST projection and its exact
  open-state dependent condition. Other needs/conditions, path filters,
  reusable jobs and dynamic evidence refuse this recovery. A check name or an unrelated successful push is insufficient.
  Every currently required and owner-owed check must actually succeed on the
  new head before the normal conditional merge.

  This uses the same cumulative base-update reservation as inherited failures:
  one update per repository/PR on the owning task, recorded before mutation.
  The owner rechecks current handoff authorization and pinned native/review
  evidence in the owner claim/task transaction and again before the provider update; revoked
  or unreadable authority cannot reserve an update.
  Polls, sibling executions and restarts cannot reset it or extend its original
  absolute deadline. An ambiguous provider reply is observed against the
  remote head and exact clean merge tree; it never authorizes a repeated write.
  Expiry stops further provider actions and waiting; an exact existing merge
  can still settle through verified readback and the normal native gates.
  Pending checks retain their ordinary bounded wait, and missing emission
  never dispatches a source-repair agent.
- Claims admitted before the change keep their `review` contract and still
  need **Approve handoff**.
- Setting the key back to `review` (or removing it) stops every handoff that
  has not landed. The landing job refuses with `owner completion policy
  withdrawn`. Restore the key, or revoke the handoff and recover the claim.

Do not merge follower pull requests on the provider by hand. That skips the
owner's validation gate, and the owner still has to settle the claim.

An authorized, stopped accepted handoff whose exact head is already merged
can use **Retry landing** on the owner. The public retry also accepts a
provider base-only update when fresh Git/provider evidence proves a two-parent
clean merge of the immutable accepted candidate with a descendant landing base.
It preserves the accepted candidate/base, native and review certificates,
foreign leaf binding and original stopped attempt. An already-merged update
requires its reported merge on the freshly fetched landing branch and the
exact clean delivery tree; readback settles it without a second merge. Initial
handoff acceptance remains strict, and the retry accepts no replacement-head
or provider-evidence JSON from the caller.

A source edit, foreign merge, failed fetch or unavailable proof cannot use
that exception. If deliberate operator recovery is needed, revoke the handoff
and use **Recover claim → blocked** on the owner before moving the task through
`in-progress` to `review`. The task keeps its original `job_run_id` and
`job_run_machine`. Changed source needs fresh review and validation; the old
certificate is never copied to a new head.

Reconcile that head instead, as an operator on the owner (the owner checkout
must hold the merged head and merge commit; `git fetch origin` there first):

```bash
orbit task reconcile-review inspect <task-id>
orbit task reconcile-review submit <task-id> --request <key>
orbit task reconcile-review status <task-id>
```

`inspect` shows the binding (run, host, claim, handoff, pull request, merged
head and base) and the `contract` a new key would freeze. It refuses with the
next step while a claim is live, the pull request is open or names another
repository or landing branch, there is no command to validate with, or
`workflow.purpose_crews.review` is unset or does not resolve.

The contract's `required_commands` are the accepted handoff's captured list
(`commands_source: accepted_handoff`). An accepted handoff always captures one,
so an empty list means that acceptance explicitly required no check; the
reconciliation then adopts the owner's `workflow.required_validation_commands`
at submission as its own contract (`commands_source:
owner_configuration_at_submission`, with `accepted_commands: []`) rather than
claiming the delivery was held to it. `review_crew` is `workflow.purpose_crews.review`
at submission; a pool contributes one member, preferring one that did not
implement the task, with `review_crew_source` naming the pool's layer.

`submit` freezes that contract into the record and admits one run of
`task_review_reconciliation_pipeline`: it runs every contract command at the
merged head (and each failure again at the base), has the contract's reviewer
inspect exactly that head read-only, and settles the reconciliation. Editing
the owner's configuration afterwards changes nothing for that record: every
attempt, including one admitted after a stopped run, uses the frozen contract.
If the frozen crew no longer resolves, the attempt fails and resubmitting the
key refuses until the crew is restored; a new key adopts the current
configuration. Resubmitting the same `--request` key replays a live or settled
reconciliation without running anything again; a new key starts a new
reconciliation. The
record lives in the owner's review store, separate from review-gate
certificates, and never changes the original run's identity or the merged pull
request. Agents cannot submit or dispose one.

The default text output includes this information; `--format json` returns the
full tool document. `status` prints one line per reconciliation with its id,
outcome, current run (when present), and exact next step. `submit` and
`accept-baseline` print that same summary for the resulting record, including
when a request is replayed. Use the printed id with `--reconciliation`.

`status` names the outcome and the exact next step:

- `accepted`: complete the task from review.
- `refused`: the reviewer left open findings (they are filed as one follow-up
  task), a command fails only at the merged head, or the delivery changed while
  the run ran. Fix forward through the follow-up, or submit a new key for the
  current head.
- `awaiting_disposition`: every failing command also fails at the base. Once a
  commit on the landing branch remediates it, record the decision. The command
  reruns that same required check at the named commit in a detached checkout;
  it records the output and refuses the disposition unless the check passes:
  `orbit task reconcile-review accept-baseline <task-id> --reconciliation <id> --command '<command>' --remediation <commit> --reason '<why>'`.
  The outcome becomes `accepted_with_disposition`; validation stays incomplete
  in the record.

  The remediation must contain the delivery as it landed. `submit` binds the
  commit the provider reports the pull request landed as (`binding.pull_request.landed`:
  its merge commit, squash commit or last rebased commit), and the disposition
  refuses, before running anything, a remediation that is not a descendant of
  it. A fix that reached the landing branch before the pull request merged
  passes the check without the delivery's code, so it would hide any failure
  the delivery added to the same command. Land the fix on top of the landed
  commit and name that new commit. Only the landed commit counts, not the pull
  request's head, which a squash landing does not keep. The disposition also
  refuses when the provider's answer (head, landed commit, task, claim or
  handoff) changes before or while the check runs; inspect and submit a new
  request key. A legacy all-pass `accepted` record can still authorize
  completion after the reconciliation consumer confirms its existing checks
  for task meaning, execution, handoff, pull request, merged head and binding
  integrity; schema 3 or earlier does not disqualify that outcome. The legacy
  restriction applies to baseline dispositions: a record written before the
  landed commit was bound (schema 3 or earlier) keeps its original validation
  and failed evidence, but cannot record a disposition or authorize
  `accepted_with_disposition`. Submit a new request key to reconcile the same
  head with an authenticated landed binding before recording a baseline
  disposition. Validation remains incomplete after a disposition, and the
  remediation must still contain the landed delivery and pass the required
  command.

On the dashboard, **approve** on a review task that has a handed-off claim
sends **Approve handoff** for the exact candidate. A plain status write would
be refused with `active execution claim requires a claim-scoped mutation`, which names the task and the claiming run.

**Stopping or cancelling a follower drain** ([ORB-13663], [ORB-13892]). All
three are safe; none strands a claim, and none fails a task that never ran.

- `orbit run auto --stop` (or the dashboard's auto **Stop**) closes the
  window. The drain keeps running until every admission settles, and the stop
  also runs a settle-only pass of its own. Its output lists the claimed
  leaves still running (`remaining_children` in `--json`).
- `orbit run cancel <drain-run> --confirm` (or the dashboard's **cancel** on
  the run, answering **Cancel** when asked whether to stop the leaves) is
  graceful. It returns at once with `cancelling: waiting for N leaves` and
  lists them (`waiting_leaves` in `--json`). The drain stops requesting work;
  its next pass releases every claim it had not launched back to the owner's
  `backlog`, with a comment naming the drain and your `--reason`. Launched
  leaves keep running and deliver their own handoff or failure; once every
  settlement has reached the owner, the drain ends `cancelled`. Meanwhile
  `orbit run show <drain-run>` prints a `Cancelling:` line and the
  `Claimed leaves:` it waits for (`.run.drain_cancel` and `.claimed_leaves`
  in `--json`), and the dashboard's run page shows the same.
- `orbit run cancel <drain-run> --confirm --force` (the dashboard's **OK** to
  stopping the leaves) does not wait. It stops the drain, records each running
  leaf's claim as released, stops the leaf's process group, and every claim —
  running or not — goes back to the owner's `backlog` with a comment naming
  the drain and the reason. The stopped leaves are listed as `forced_runs`.
  The release holds even with `--block`, which only decides how the stopped
  leaves' own local task couplings are left.
  A leaf that had already recorded its handoff is left to finish and deliver
  it. `--force` on a drain that already ended still stops the leaves it left
  running. It touches only what that drain carries: the leaves it admitted
  and, while it was live, those an ended drain for the same owner left
  behind. Never a leaf another live drain admitted.
- If the pull drain worker cannot be confirmed stopped, `--force` fails
  naming the drain and the signal outcome before finalizing the drain,
  stopping leaves, or releasing carried claims. This includes workers in
  another PID namespace or with an unverifiable identity. Stop the drain
  on its host and retry once its exit can be confirmed. A worker confirmed
  already exited permits forced leaf cancellation and claim release.
- A leaf `--force` cannot stop and see gone keeps its claim on the owner.
  Its worker might run in another PID namespace, or have an identity that
  cannot be verified. The cancel lists it under `unstopped_leaves` (the
  dashboard flags it) and exits 1. When the leaf is refused before the stop,
  nothing is released and the leaf delivers its own outcome when it ends. A
  stop that was signalled but not confirmed holds the recorded release
  (`release_held` in `pull_settlements`): it reaches the owner only once the
  leaf is seen to stop, so a second executor never starts the task beside a
  first that is still running. Stop that leaf by hand on its host.
- The MCP stop control takes the same option: `orbit.workflow.auto` with
  `action: "stop"` and `force: true` stops admissions, then force-cancels each
  live drain as above (a local drain's task runs included). It fails, naming
  the leaves, when any leaf could not be confirmed stopped.
- A drain that is still queued, or whose worker is gone, is cancelled at once;
  its unlaunched claims go back to `backlog` and its live leaves settle
  themselves.
- Cancel prints a `Pull settlements` section, one line per admission, and
  `--json` carries the same list as `pull_settlements`.
- Prefer `--stop` when you only want no new work: it ends nothing. The
  dashboard's cancel prompt for a drain run says this too.

A graceful cancel waits as long as a leaf runs and its settlement is owed. If
the owner stays unreachable the drain keeps showing `cancelling` (each pass
reports the transport error); `--force` ends it, and anything undelivered stays
recorded for the retry below.

For the owner's local drain (`orbit run auto`), cancel still detaches the task
runs it started, which finish on their own; `--force` cancels them too and
returns their tasks to `backlog`, preserving their candidates and recording
the cancellation reason. Use `orbit run cancel <drain-run> --confirm --force
--block` to keep those tasks blocked instead. The MCP forced-stop control
returns cancelled local tasks to `backlog`. Stops
are confirmed before each child is finalized. An unconfirmed stop is reported
under `unstopped_children` with the child run ID and reason; the CLI exits 1
and the dashboard flags the incomplete cancellation. The parent and children
that were successfully stopped remain reported as cancelled and `forced_runs`
respectively. The MCP forced-stop control also fails if a detached child
cannot be confirmed stopped.

The dashboard reports the same `pull_settlements` list after **Stop** and
after **cancel**: a one-line summary counting each outcome, with any
settlement that has not reached its owner called out (`owner_unreachable` and
`pending_delivery` say to run Stop again once the owner is reachable;
`launch_uncertain` says it needs manual recovery, below). With no live window
the auto card's button reads **Send pending results** and runs the same settle-only
pass; it stays available because the pass needs no active drain.

Leaf delivery can still fail — the owner was unreachable when the leaf ended,
or when `--force` released it. The settlement stays recorded on the follower
as `settling` and is retried without a new drain. A live drain retries on
each pass, and a leaf's worker retries briefly (15s, 60s, 240s). After both
have ended, the OS clock sweep (`orbit clock tick`, every minute by default
after `orbit routine init --install-clock`; use `orbit clock enable` to resume
an installed paused clock) retries it: each tick opens the host's replica
checkouts too, only to deliver what their drains recorded. A tick never ends
unlaunched work, and it delivers a leaf's failure once the leaf's dead worker
is reconciled. On a host without the clock, flush it with
`orbit run auto --stop` in the replica checkout once the owner is reachable
(safe to repeat, and it needs no active drain), or by starting the next
drain. `orbit run cancel <drain-run> --confirm` on a drain that already ended
does the same flush. When the drain and its leaves died together, as when a
host OOM killed the user systemd manager and the clock with it, the
follower's `orbit run history` or `orbit run show` is enough: it marks each
run whose worker is gone `interrupted`, then settles what they left that no
live drain carries, a dead leaf's failure and the release of each claim the
drain never launched. It contacts no owner while a drain or leaf is alive. `orbit doctor` reports a
`pull-settlements` warning while any recorded settlement is undelivered: how many
wait, how long the oldest has, and `orbit run auto --stop` as the fix. The row
is `ok` once they are delivered, and on a workspace that never pulled. Every line
that leaves work for the operator says what to do next, and `launch_uncertain`
still needs the deliberate recovery below.

**Settlements an older binary stranded.** A binary before [ORB-13663] left a
cancelled drain's finished leaves `settling` and its failed leaves
`launched` with no settlement, and their owner claims `running`. After
upgrading the follower, run `orbit run auto --stop` in its replica checkout.
Delivered handoffs move their tasks to `review` on the owner, and the
failures move theirs to `blocked`. Inspect first: `orbit doctor` counts the
recorded-but-undelivered (`settling`) settlements. It does not see a failed leaf
that recorded no settlement (`launched`), so for a complete read-only list of
everything still holding a slot, query the database:

```bash
sqlite3 -readonly ~/.orbit/orbit.db "SELECT leaf_run_id,
  json_extract(record_json,'$.phase'),
  json_extract(record_json,'$.receipt.claim.task_id')
  FROM local_pull_admissions
  WHERE json_extract(record_json,'$.phase') NOT IN ('settled','idle','refused')"
```

If the owner revokes those claims in the meantime, the next pass cannot
deliver their settlements. The owner refuses each one as `stale_claim`. The
pass then looks up the claim on the owner, sees it has ended, and closes the
record locally (`closed_obsolete`). Its `refusal` records why. Those tasks still need an owner-side
status decision: set a task to `done` if its pull request merged, otherwise
move it back to `backlog`.

**A settlement the owner refuses while it still holds the claim**
([ORB-13979]) — for example, an owner that refuses a footprint widening onto
a path it protects — is an answer,
not a lost delivery, and repeats until an operator changes the owner. The
follower records the refusal on the admission (`settlement_refusal`), logs it
once, and keeps the record `settling` with its outcome unchanged; nothing is
lost. It is not a failed pass, so it does not degrade the drain, but the
drain requests no new claim while one is held (pass output
`refusal: settlement_refused: …` and `settlement_refused: <count>`). Drain
passes, the clock sweep and the leaf's worker deliver it again only after a
backoff that starts at one minute and doubles to at most 15 minutes, instead
of on every pass. `orbit run show <drain-run>` lists each one on a
`Settlement refused:` line (`.refused_settlements` in JSON) with the owner's
reason, the refusal count, the next attempt and the remedy, and a leaf's
`orbit run show` carries it on its `Claim:` line (`.pull_claim.settlement_refusal`).
Fix the condition the reason names on the owner. The next due attempt then
settles the recorded outcome and the drain resumes requesting. To retry at
once, run `orbit run auto --stop` in the replica checkout: an operator's
settle-only pass ignores the backoff, though it also closes the drain's
window.

To give the claim up instead, revoke or recover it from the owner's dashboard
(**Recover claim → blocked** or **→ backlog**). The next delivery on the
follower, whether a drain iteration or `orbit run auto --stop`, looks the
claim up, sees it has ended, and closes the record `closed_obsolete`, which
frees the slot. A claim whose bind the owner refuses before the leaf ever
launched closes the same way: the leaf is failed and never starts.

Handoff **approval** and **revocation** are owner-operator mutations (agent
capability cannot approve). They are owner-domain seams reached from the
owner's dashboard — `POST /api/distributed/handoffs/<handoff-id>/approve` and
`.../revoke`, governed as `handoff.approve` and `handoff.revoke` — and they are
still not `orbit tool run` entry points. Never substitute `--complete` on a
follower or `orbit job resume`:

- Approval records one immutable candidate-scoped authorization and one
  landing-start request, and it does not merge. It carries the exact candidate
  and base commits you were shown, so a stale page is refused with
  `stale_claim` rather than approving whatever the owner now holds. Retries of
  the same request ID replay that decision instead of creating a second grant.
- Revocation invalidates pending landing permission and leaves the task in
  `review`; it requires a reason. Unresolved merge intent still has to be
  reconciled first — the dashboard refuses with `uncertain_merge_intent`, and
  so does the store.
- Pull eligibility and `agent` access never grant merge rights. A replica
  refuses all three actions with `replica_checkout`; route them to the owner.

## Migration recipe: leftover epic, child, review, and reservations

Epic execution is retired. Hierarchy remains for reading; admission ignores
the `epic` tag except for roots that declared **no** own `context_files` while
a descendant did. Those inherited-only roots are withheld until an operator
supplies own context or retires them. Do not guess the union.

There is no public `epic retirement` command. Gather the reconciliation evidence
manually using standard inspection commands:

```bash
orbit run history -j epic_pipeline --limit 50
orbit run history --limit 50
orbit task list --tag epic
orbit task show <task-id> --fields status,context_files,job_run_id,job_run_machine
orbit task locks list
orbit doctor
```

Refuse to treat the workspace as migrated while any of these remain
unreconciled, **regardless of the root's status** (a root in `review` can
still have a live completion step):

| Leftover | What to do |
|---|---|
| Non-terminal `epic_pipeline` run | Wait, cancel with evidence, or finish it; do not delete the run row |
| Active child/family run | Inspect on its host; stop or complete it; do not ship the root as a leaf beside it |
| Uncertain landing (failed/interrupted family run, or a missing recorded run) | Reconcile the branch/PR/local ref before reassignment |
| Active reservation naming an epic-family task | Release only when the owning run is terminal and no newer claim holds it: `orbit task locks list`, then operator `orbit task locks release <reservation-id> --confirm` |
| Inherited-only epic root | Copy descendant selectors you can prove from `orbit task show`, or restore pruned declarations from history; never invent files |
| Historical epic worktrees | Let GC reap them; keep decoding until `orbit doctor` / worktree GC shows they are gone |

Failed-run triage is also retired. A failed claimed or family run parks the
task in `blocked` with `job_run_machine`. Re-backlog is a deliberate
`orbit task update --status backlog`, made by whoever inspected the evidence.

## Retained ship-sweep (do not enable as part of setup)

Keep the seeded `ship_sweep` routine, `workspace_ship_pipeline`, and
`orbit run ship-sweep`. Existing `enabled` flags, cadences, and
`workflow.auto_ship` stay as the operator left them.

```bash
orbit routine list
orbit run ship-sweep --dry-run
```

The CLI sweep reads the registry selected by `--root <dir>`, then `ORBIT_ROOT`,
then `~/.orbit`. Use `orbit --root <dir> run ship-sweep --dry-run --json` to
inspect an alternate registry before allowing dispatch. It does not create a
workspace in the directory from which the scheduler invokes it.

A replica sweep reports destination-authority refusal before it reads a
backlog. Scheduled invocation still confers no completion authority. Do not
enable a dark routine to "turn on" distributed drain.

## Replica worktree GC

On the owner, delivery removes a task worktree once the run lands, and the
owner's `worktree-gc-<workspace>` routine is the hourly backstop. A follower's
claimed-leaf worktrees live in its replica checkout, which no owner process
touches, so the replica schedules its own GC on its host clock. That routine
and the replica's store-gc routine are the only ones a replica fires. Its ship
sweep, task pilot, CI and Dependabot sweeps and its auto-tasks stay owner
work: `orbit routine list` shows them as `owner-only` with the owner machine
named, a toggle or pause is refused, and the sweep reports them `skipped` with
an `owner_only_in_replica:` reason. The auto-task panel marks toggle and
manual mint unavailable and names the owner; replica task minting remains
refused. A delivery- or state-triggered routine stays owner work even when it
targets one of those two jobs.

Enable the replica's GC on the follower as an operator, with
`orbit_routine_control` (`action: toggle`) or the dashboard's Operations
routines panel. You can also set `enabled: true` in the replica checkout's
`.orbit/routines/worktree_gc.yaml`. Then confirm it is armed:

```bash
orbit routine list --workspace <replica-workspace>
orbit clock status
```

GC reads historical tasks carrying this machine's prefix from the local store.
It asks the owner only for the owner's prefix, through the run's claim route or
the replica's registered workspace. Existing admissions identify the owner's
prefix; before any pull, GC uses a single foreign prefix in this workspace's
stored task ids. Multiple foreign prefixes without admissions are ambiguous.
Other or ambiguous prefixes stay `skipped:task_prefix_unroutable` without an
owner call, and missing tasks stay `skipped:task_unresolved`. GC reclaims a
worktree only when its claim was accepted and settled or every task is settled
in its authoritative store. A worktree
whose owner is unreachable or unrouted stays, reported as
`skipped:owner_unreachable` or `skipped:no_owner_route` in the run's `reap`
output. A failed leaf keeps its uncommitted candidate in its worktree. Once its
task is settled, GC removes that worktree after it confirms the candidate is
recoverable: from the carry ref, from the published branch, or from a rescue
patch that GC writes and verifies first, pinning that patch's base on a local
ref so an unpublished HEAD survives branch deletion
([worktree-reclaim.md](worktree-reclaim.md#dirty-worktrees-of-settled-tasks)).
Each kept worktree's declared build output is reclaimed on every sweep, and
`orbit run show <gc-run>` lists every kept worktree with its reason and
`retained_bytes`.

A claimed leaf's required validation builds its host validation output outside
the leaf's worktree, under the primary checkout that owns the replica's Git
common directory. When the replica checkout is itself a linked worktree, that
is the primary clone it was added from
([worktree-reclaim.md](worktree-reclaim.md#host-validation-output)). The
replica's GC attributes each such target to the leaf run whose worktree path
hashes to its name, from the follower's own run records. Once that run is
terminal with no live worker, the target goes with the leaf's worktree: in the
sweep that reaps it, or in a later one after the worktree is gone. After a
settled claim, the drain's next pass also gives it back with the leaf's
`target/`, unless `worktree.reclaim` is empty and the worktree remains. A
running leaf keeps it as `skipped:run_not_terminal` and a held lease keeps
it as `skipped:host_target_in_use`. A target no follower record derives stays
as `skipped:unattributed_host_target`. An unscoped `orbit gc worktrees`
reports a row for every target it examines.

## Replica store GC

The follower's host store is not the owner's. Command audit, resource history,
and the run audit and terminal run state of the workspaces it replicates live
in the follower's own `~/.orbit/orbit.db`. The owner's `store-gc-<workspace>`
routine never opens that file. On a follower-only host every checkout is a
replica, so the store stays untrimmed until a replica schedules
`store_gc_pipeline` for itself.

That routine is seeded disabled, the same as on an owner checkout. Review the
plans, then enable it on the follower:

```bash
orbit gc audit --json --workspace <replica-workspace>
orbit gc runs --json --workspace <replica-workspace>
```

Enable it with `orbit_routine_control` (`action: toggle`) or the dashboard's
Operations routines panel, or set `enabled: true` in the replica checkout's
`.orbit/routines/store_gc.yaml`. Then confirm it is armed:

```bash
orbit routine list --workspace <replica-workspace>
```

The run applies both plans against the local store. It deletes command-audit
rows and per-minute resource history past their windows (those tables are
host-wide on this machine), this workspace's run-audit rows, run-audit rows
and terminal run state whose workspace id is no longer registered, the audit
blobs nothing still names, and the pipeline state of terminal runs older than
`retention.runs_days`. Another registered workspace keeps its own window. Run
rows and steps stay, and nothing is vacuumed. The activity does not call the
owner and does not write the owner's coordination store. Enabling it on the
owner is unchanged: that routine still prunes only the owner's store, plus
unregistered ids in that same file.

One enabled replica routine is enough for the host-wide tables and for
unregistered workspace ids. Each replicated workspace still needs its own
routine, or a pass from that workspace, before its run audit and run state
are pruned. A delivery- or state-triggered copy of the job stays owner work
and is listed `owner-only`.

## Scheduled host shutdown or reboot

When the host has a shutdown or reboot pending (for example
`unattended-upgrades` running `shutdown -r 04:00`), Orbit holds new
unattended work only while that schedule is within
`machine.host_shutdown_hold_minutes` (default 180; see
[CONFIG.md](../CONFIG.md#machine--who-this-machine-is)). A reboot set
further ahead is still reported, and routines, auto-tasks, and drain waves
admit as usual, so the host is not idle for the whole lead time. Inside the
horizon, routines and auto-tasks do not fire, drain waves admit no leaves, a
pull drain requests no claim and records `admission_refusal` beginning
`host_shutdown_scheduled`, and `orbit run ship-sweep` skips with
`host_shutdown_scheduled`. Runs already in flight are not cancelled or
signalled.

```bash
orbit doctor                 # warning while the schedule is inside the horizon; ok, naming the time, when it is further away
orbit run readiness          # inside: "Admissions held: host reboot scheduled for …"
                             # beyond: "Host reboot scheduled for …. Admissions continue until machine.host_shutdown_hold_minutes …"
cat /run/systemd/shutdown/scheduled   # USEC= / MODE= written by logind
```

- To resume admissions before the restart, cancel the schedule as root with
  `shutdown -c`, or wait until the schedule is further than the horizon. The
  next tick or drain iteration admits again.
- Otherwise, do nothing. `/run` is cleared on reboot, and admissions resume
  on their own when the host comes back.
- `orbit run ship` and `orbit run auto` still run while a hold is active and
  log a warning. Beyond the horizon they run without that warning. A single
  explicit ship may be killed by the restart. A drain started during the hold
  admits nothing until the hold clears.
- Reboot policy (apt `Automatic-Reboot*` settings) is root territory, and
  Orbit does not change it.

## Host resource pressure

While CPU, memory, Linux disk I/O stall or a checkout/worktree/global-root
filesystem stays at or above its high mark for ten seconds (`[workflow.resource_throttle]` in
[CONFIG.md](../CONFIG.md#workflowresource_throttle--host-pressure)), Orbit
starts no new task work until that resource falls back below its resume mark.
Local drain waves admit no leaves and poll for recovery, pull drains request
no new claims but keep settling and reconciling, and `orbit run ship` without
task ids (and `orbit run ship-sweep`) is refused with `resource_throttled`.
Running work is never cancelled, paused or killed.

**Whose resources gate a pull claim.** A claimed leaf runs on the follower,
so the follower's own readings decide whether its pull drain requests a
claim. The owner's throttle holds only the owner's own drain waves and
discovery (`orbit run ship` without task ids, `ship-sweep`). The owner never
reads its throttle when it serves `orbit.task.pull` or the probe, so a busy
owner does not idle its followers. The owner's `orbit run readiness`
"Admissions throttled" line and its backlog's `resource_throttled` waits
describe the owner's drain, not the followers' pull drains. To see why a
follower is not claiming, run readiness and `run show` on the follower
(below).

```bash
orbit run readiness          # "Admissions throttled: memory 93% (throttled at ≥ 90% since …"
orbit run show <drain-run>   # Throttled: line from the drain's last pass
```

Disk I/O is read from `/proc/pressure/io` `full avg60`
(`io_full_high_percent`, default 20; resume below `io_full_resume_percent`,
default 10) and appears as `io full` in readiness and the drain's last pass,
for example `io full 47% (throttled at ≥ 20% since …; resumes below 10%)`. A
host without pressure stall information, such as macOS, has no I/O reading and
admits as before.

On the dashboard, the rail foot's host block shows the serving host's `load`,
`mem` and `disk` readings, with an `io` row on hosts that report I/O pressure.
A critical or held reading washes the block red, and the detail panel gives
the verdict, reason and sample age. The top bar shows one warning chip only
while a reading is elevated or worse. Settings › Machine lists the I/O row too,
on hosts that report it.

- Sampling monitors share recent pressure history in
  `<global-root>/cache/host-resource-pressure.json`, independently of drain
  records and workspace. A fresh `orbit run ship` or `ship-sweep` evaluates
  its sample against that history using its configured thresholds. Unknown
  readings, observation gaps over fifteen seconds and changed thresholds
  reset the affected resource's history; a first high sample alone does not
  prove sustained pressure. If history is unavailable, evaluation uses local
  observations. Drains sample every five seconds and also record the throttle
  on each pass; readiness and the dashboard Drain card can read that record.
  MCP `orbit.workflow.auto` status carries
  `capacity.resource_throttle`; `orbit.workflow.run.show` carries the drain's
  `drain_last_pass`.
- `orbit run auto`, MCP `orbit.workflow.auto` start, the dashboard's drain
  start, and a ship of named tasks (`orbit run ship`, MCP
  `orbit.workflow.ship`, the dashboard's ship) proceed and report a warning. The drain admits nothing until the
  pressure clears; a named ship starts at once.
- CPU-light work keeps moving under CPU pressure. When CPU is the only held
  resource, a local drain still starts `no-diff-expected` auto-task leaves
  (after-landing review, friction curation, full review) until
  `workflow.resource_throttle.cpu_light_leaves` (default 2) of them are live
  leaves; every other leaf waits. Memory, disk or I/O pressure holds them too, and
  pull drains do not use the budget. Readiness marks these tasks `cpu-light`,
  prints `CPU-light budget: <active> of <reserved> reserved slots in use`,
  reports `cpu_light_budget_full` for a light task waiting on a spent budget,
  and carries the numbers in `capacity.cpu_light_budget`; the drain's pass
  output carries the same object.
- A task minted over a frozen delivery batch within two hours of the batch's
  admission deadline (`retry_until`, or an operator reissue's) sorts ahead of
  same-priority backlog, corrective work included; critical work still leads.
  Local dispatch and pull admission use the same owner-side expiry set and
  ordering. If deadlines cannot be read, both retain the ordinary order.
  Readiness names that deadline as `frozen-batch-deadline` (JSON
  `frozen_batch_deadline`). Raising such a task to critical is no longer
  needed to keep its batch from expiring behind ordinary work.
- Readings that cannot be taken (unavailable, invalid or stale) never
  throttle. They are listed as `resource_telemetry_unknown` and logged once.
- Each throttle and recovery is logged once under `orbit.core.host_resource`.
- To restore the previous behaviour, set
  `workflow.resource_throttle.enabled = false`; no pressure is then sampled
  for admission.

Followers must match the owner's pull request schema, independently of `orbit --version`.
The read-only probe reports `protocol_fingerprint`, a SHA-256 fingerprint of the JSON schema
derived from the running build's `AdmissionRequest` and all its nested types. The follower
first probes with legacy-compatible fields, checks that fingerprint and `protocol_schema`,
then declares `caller_fingerprint` on a second probe. A different or missing fingerprint,
including a legacy owner, refuses with typed `protocol_skew` before any `orbit.task.pull`.
A protocol identity containing `[REDACTED_ENV]` is a corrupted transport reply,
not evidence of schema skew. The follower treats it as typed `OwnerNegotiation`,
records a transient pass error and retries on the next pass. The owner also
checks redacted drain replies, including nested fingerprints, commit/tree IDs
and evidence hashes: read-only calls fail negotiation; mutating calls report
`OutcomeUnknown` so the follower reconciles or replays the same request.
Credentials remain scrubbed even when they overlap an identity. Known
`XDG_SESSION_*`, `DBUS_SESSION_BUS_ADDRESS`, `SESSION_MANAGER` and
`TERM_SESSION_ID` metadata is excluded from session-name matching; credential
words still take precedence, and other session names retain conservative
handling. Purely numeric environment values shorter than 12 digits are
excluded from substring substitution.
The integer revision remains for persisted requests and lifecycle semantics; request field
changes no longer depend on a manual bump. Deploy matching builds on both hosts and restart
long-lived processes.

`orbit run show <drain-run>` exposes why a pull drain's last pass requested no claim although
its window was open: the `Requests held:` line, from `admission_refusal` under
`pipeline_state.drain_last_pass`. Its values are `circuit_open` (the failure breaker is open),
`circuit_half_open` (it waits for the drain's claims to settle before one probe claim),
`settlement_refused`, `no_runnable_crew`, `host_suppressed`, `owner_busy_backoff`
(a claimed leaf was released because the owner's store stayed locked; requests resume by
themselves at the time it names),
`host_shutdown_scheduled` (a shutdown or reboot inside
`machine.host_shutdown_hold_minutes`), or the owner probe's refusal.

The dashboard run detail shows the same refusal in its **Requests held** panel beside
crew and waiting information. It includes the breaker failure classes, the time until
it half-opens, and each refused settlement's reason, next retry and remedy. Healthy
drains have no panel. The run-detail API returns `pull_breaker` (null while closed)
and `refused_settlements` (an empty array when none are refused) alongside `run`;
`run.drain_last_pass.admission_refusal` supplies the last pass's refusal text.

The value is set afresh each pass. This host's resource throttle has its own `Throttled:` line,
and degradation has its own warning. Follower readiness repeats the refusal as
`Pull drain requests held:` (`capacity.pull_drain_refusal`).

`orbit run show <drain-run>` also exposes a pull drain's latest pass error and consecutive failure
count. JSON carries `last_pass_error_code`, `last_pass_error`, `consecutive_pass_failures`, and `degraded` under
`pipeline_state.drain_last_pass`. Three consecutive failed passes latch a visible degraded
warning and stop new admissions for that drain. A successful pass before the threshold resets
the streak. Protocol skew immediately latches degradation and ends the drain **failed** with `protocol_skew`,
even with an open window. `orbit doctor` reports the latest skewed pull drain, and the dashboard
keeps its pass health and failure code visible after it ends. Its durable admissions and settlement
records remain available to leaf workers, the settle-only pass, and the clock sweep. Other
degraded drains keep retrying settlements and outlive their window until nothing
is unsettled; successful settlement does not clear the warning. Fix the reported cause, run
`orbit run auto --stop` to close the window, and start a new drain once this one ends. An unreadable or unwritable run-state record fails the activity visibly.

A pull drain also records what its owner kept off this host. When a request is answered idle,
the receipt's diagnostics fill `drain_last_pass` as a local drain's classifier does: `queued` is
the receipt's `queue_depth`; `deferred` lists footprint holds (`context_lock_conflict`, the holder
in `blocked_by`) and other owner holds (`owner_hold`); `excluded` lists unmet dependencies
(`dependency_not_done`, the unfinished tasks in `blocked_by`), `os:` waits (`host_os_mismatch`),
browser-kit waits for `ui` tasks (`browser_kit_missing`) and unrunnable crews (`crew_unavailable`), both lists bounded to 20 with `deferred_total` and `excluded_total` the full counts; and
`waiting_by_reason` counts every kept-off task by code. `waiting_recorded_at` dates the owner's
answer. A pass that sends no request (throttled, settlement held, breaker open, window closed,
owner unreachable), or whose requests all claim, keeps the previous diagnostics and their date
rather than recording an empty backlog. `consecutive_idle_passes` counts the idle answers in a row
that found tasks waiting; from three, `orbit run show` and the dashboard add an `idle:` line
saying how many tasks were kept off this host and why. Both print the same `Still waiting` lines
for a pull drain as for a local one.

## Verification

On the owner:

```bash
orbit --version
orbit workspace show
orbit doctor
orbit config get review.timing
ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input '{
  "caller_version": "<owner-version>",
  "caller_schema": 9,
  "caller_before_pr": false
}'
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
curl -s -H 'Host: localhost:7878' http://localhost:7878/api/distributed/claims?workspace=<workspace-id>
```

From a follower session aimed at the owner selector, repeat the probe with
that follower's `orbit --version`. Confirm:

- versions and schema match;
- the probe admits (`admits: true`), and with the owner's `review.timing`
  at `before_pr`, the follower's drain lists the owner's review crew as runnable
  (`Crews:` in `orbit run show <drain-run>`);
- the probe created no task, reservation, or claim (`orbit task locks list`
  unchanged);
- `orbit job resume` of a known claimed leaf still refuses;
- no leftover callers file is treated as an ACL.

After starting a drain (step 8), confirm the first claim end to end: the
owner's `orbit.drain.claims` shows it `running` on the follower's machine,
the follower's `orbit run show <leaf-run-id>` shows the claimed PR leaf and its `Claim:` line, and
after handoff the owner task is in `review` with the PR attached and nothing
merged. With before-PR review on, the owner task also carries
`review-gate.json` and the reviewer's verdict comment, and the follower's
audit holds one brokered `orbit.task.artifact.get` and one
`orbit.task.artifact.put` for the leaf (the smoke procedure in the
[claimed-review artifacts runbook](./claimed-review-artifacts.md) reads
them).

## Rollback

Stop the follower's drain with `orbit run auto --stop` and let its live
leaves settle, or cancel them (each cancelled leaf settles its claim as a
failure) and recover their claims on the owner's dashboard. Leave the replica registered but idle. Restore the
demoted host as an owner only by a deliberate, documented re-init after
quiescing the current owner — this runbook does not perform that reversal.
Preserve schedule files; do not bulk-disable routines as cleanup.

## Related references

- [Runbook conventions](./CONVENTIONS.md)
- [Bound concurrent builds](./build-budget.md)
- [Inventory and protect Orbit state](./state-and-backup.md)
- [Recover stuck job runs](./stuck-job-runs.md)
- [Distributed drain design](../design/distributed-drain/2_design.md) and
  [task-pull spec](../design/distributed-drain/specs/task-pull.md)
- Embedded skill: [distributed-drain setup](../../crates/orbit-core/assets/skills/orbit/references/setup/distributed-drain.md)
