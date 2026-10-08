# Distributed drain setup and recovery

One owner checkout, replica checkouts on other machines, and a pull drain on
each replica. Use this when the user wants a second machine to execute the same
logical workspace, or when a claimed attempt needs inspection. Installing
matching binaries is not a rollout; starting `orbit run auto --pull` is.

The owner serves probe, receipt lookup, task admission, bind and settlement
through a deterministic internal RPC selected by Orbit's runtime SSH launch.
These five operations are absent from ordinary MCP discovery and refused by
public `tools/call`, including their formerly advertised spellings. Client names
and initialize metadata grant no internal access. Both endpoints must support
the internal protocol; preflight fails closed without a public fallback. A
replica runs `orbit run auto --pull <selector>` to use that route. Supported
operator diagnostics remain owner-side CLI commands; approval, revocation and
recovery remain owner-dashboard actions. Do not invent tools for them, write a
callers file, or start a second owner store.

Owner/replica registration lives in
[multi-host.md](../../../orbit-setup/references/multi-host.md). SSH federation
and session authority live in
[remote-access.md](../../../orbit-setup/references/remote-access.md). Tool
routing is in [tool-surface.md](../tool-surface.md).

## What v1 does not do

- No heartbeat, automatic reclamation, fleet registry, or follower merge.
- No review on the follower's own terms. With the owner's `review.before_pr`
  on, every claimed PR leaf runs the before-PR reviewer the claim captured
  (the owner's `operation.review_crew`, which each follower must be able to
  run); the follower's own `review.before_pr` is ignored. After-landing review
  (the owner's `delivery-code-review` auto-task) never affects admission.
  Status `review` means a delivery handoff is waiting; whether a reviewer ran
  is on the task's `review-gate.json`.
- Age, reservation TTL, and a missing local run are diagnostics, not proof of
  death.
- Seeded `ship_sweep`, `workspace_ship_pipeline`, and `orbit run ship-sweep`
  stay at their current enablement. This setup does not turn them on.
- Remote machine labels (`--remote-caller-machine-id`) are attribution, not
  credentials. SSH login is owner access. There is no destination callers
  file, forced-command acceptance, KeyBound proof, or replacement identity
  registry.

## Prerequisites

On every participating host:

```bash
orbit --version
orbit config get machine.id
orbit workspace show
orbit doctor
orbit config get review.before_pr        # decisive on the owner only
orbit config get operation.review_crew   # the owner's before-PR reviewer crew
```

Require one owner per repository, matching binaries, matching distributed-drain
protocol schema `10`, and equivalent crew and toolchain resolution. Review policy
is the owner's: admission captures the owner's `review.before_pr` and, when it
is on, its `operation.review_crew`, review budget, and
`workflow.required_validation_commands` into the claim. With it
on, the owner must ship through PRs (the before-PR review runs only on that
route; admission refuses otherwise) and set `operation.review_crew`, and every
follower must resolve that crew; `orbit run auto --pull` refuses before
claiming with `before_pr_reviewer_unavailable` otherwise. The follower's own
`review.before_pr` is reported to the owner but never gates admission or the
leaf, so a follower need not change it. Hosts may run different operating systems:
each follower declares its OS, and a task tagged `os:linux`, `os:macos` or
`os:windows` is claimed only by a host of a named OS; it waits in the backlog,
named, until one pulls it. Empty
`workflow.required_validation_commands` means no host-required validation check:
a claimed leaf runs no such check, and its review certificate records the
captured empty list. When configured, the owner requires the reviewer to report
each captured command as passing evidence, then independently verifies the
owner's exact-run and exact-head handoff logs against the same command list.
List skew or missing evidence fails closed.
A leftover `~/.orbit/mcp-callers.toml` or `~/.orbit/mcp-ssh-acceptance/` is
ignored: `orbit doctor` warns; delete the files. Deny a caller by removing its
key from `~/.ssh/authorized_keys`.

Per-host compiler capacity is independent of drain slots. Keep the shared
build-budget defaults (two heavy slots, four Cargo jobs) unless the operator
raises them.

Managed agents never propagate `--operator` or `ORBIT_OPERATOR`. Claim,
machine, bound run, and phase checks fence attempt ownership from trusted
runtime invocation context, not from a payload label.

## Collapse two owners before any replica work

If both hosts initialized the same repo as owners, stop competing drains on the
host that will become a replica, reconcile in-flight runs and reservations,
export tasks that must move, then re-register. An owner checkout is never
rebound in place, so drop its registration first (registry only; `.orbit` stays):

```bash
ORBIT_OPERATOR=1 orbit workspace remove <workspace-id>
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica --owner <owner-machine-id>
```

`workspace role` reasserts; it is not a takeover. Copying Git history does not
move the control plane.

```bash
orbit task export --ids <task-id> -o <archive>
orbit task import <archive> --on-conflict=renumber
```

`--on-conflict=owner-wins` is for a repeatable mirror of IDs the owner already
minted. Do not rsync the store. Restore pruned `context_files` only from
history:

```bash
orbit task lint <task-id>
orbit task lint <task-id> --restore-pruned
```

Missing files are valid declarations. `--restore-pruned` never guesses.
Reservation TTL on a pulled claim is 14,400 seconds and does not revoke the
claim or shrink the frozen footprint.

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
are refused even on Linux. Both peers require the same protocol revision (currently 10).
Tagged `no-diff-expected` tasks are claimable and hand off `NoDiff` with no
PR; a review needs no report, and a tagged claim that changes code is refused
as `no_diff_expected_changed`. An ordinary claim that proves its
implementation changes nothing may also hand off `NoDiff`. In claimed mode the implementer writes the verifier report
and its logs beneath `.orbit/tmp/` and returns `no_diff_artifacts` scratch
references; commit imports and verifies them through the claim. The owner
rechecks the report against its live base before authorized completion without
a PR. A changed base or evidence requires fresh validation.


## Start a follower's drain

On the follower, from the replica checkout, with the owner registered by
`orbit host add <owner-ssh-target>` (check `orbit host list` shows it reachable
with this machine's `binary_version` and `protocol_fingerprint`) and the same
`workflow.required_validation_commands` the owner declares (an empty list on
both sides runs no required check; the drain starts and notes it):

```bash
orbit run auto --host <owner-name> --pull <workspace-name-or-ws_id> --for 8h --concurrency 3
orbit run auto --pull <selector> --for 8h --concurrency 3
```

`--host` takes the registered owner's exact name or `machine_id`; `--pull`
then takes a workspace name or `ws_*` ID from that host's live workspace list.
Orbit copies the matching descriptor's selector. Without `--host`, `<selector>`
must be the full owner-qualified selector from federated `orbit.workspace.list`;
copy it unchanged rather than spelling it by hand. A bare workspace without
`--host` is refused with `unknown_selector`, and the local host is refused as
a pull owner. Host-resolution errors and remedies are in
[tool-surface.md](../tool-surface.md#routing-failures-and-remedies).
The command refuses before submitting
unless the checkout is a replica of that owner and workspace and the owner's
probe admits this executor. It prints a `workspace_pull_pipeline` run ID and
returns.

- Without `--for` (or `--for 0s`) the drain makes one admission pass up to
  `--concurrency`, admits no replacements, and ends once those claims settle.
- `--allow-crew <crew-a>,<crew-b>` limits the crews this drain declares, on
  every pass and on resume; unknown or blank names refuse before submission.
  The owner's before-PR review crew still has to run here but need not be
  named. Task crews and configuration are unchanged.

- Each claim runs locally as `task_claimed_pr_pipeline` and ends at a pull
  request handed to the owner, which moves the task to `review`. The owner
  approves before anything lands; the follower never merges. With
  `workflow.distributed_completion = "done"` on the owner, acceptance
  authorizes the handoff and the owner's landing job merges and completes it,
  as `--complete` does for owner tasks.
- The drain keeps settling claims after `--for` expires, until none is left.
  `orbit run auto --stop` closes the window early; live leaves keep running.
  Each leaf also delivers its own handoff or failure when it ends, so
  cancelling the drain (`orbit run cancel <run-id> --confirm`) strands nothing. Cancel is
  graceful: the drain stops requesting, returns unlaunched claims to the
  owner's backlog, and ends `cancelled` once its running leaves have settled;
  `--force` stops that drain's leaves too and returns their tasks to the
  backlog. A leaf it cannot confirm stopped keeps its claim, is listed under
  `unstopped_leaves`, and fails the command. The MCP stop
  (`orbit.workflow.auto`, `action: "stop"`) takes the same `force`.
  Prefer `--stop`, which ends nothing. The OS clock sweep retries any
  settlement still recorded on the follower (for example after the owner was
  unreachable) once no drain or leaf worker is left to; `orbit run auto --stop`
  flushes it by hand, with or without an active drain.
- An unreachable or refusing owner is reported in each iteration's output.
  Protocol skew ends the drain failed immediately with `protocol_skew`; its
  durable settlements remain available to leaf workers and the settle-only
  pass. Other failed passes are retried until three in a row degrade the drain,
  where settlement retries continue. A request the owner refused and holds no
  receipt for closes as `Refused`; a committed one is carried forward.
- A settlement the owner refuses while it still holds the claim (for example a
  footprint widening onto a path the owner protects) stays
  recorded, holds new requests, and is retried with a backoff of at most 15
  minutes rather than every pass. `orbit run show <drain-run>` prints it once
  as `Settlement refused:` with the owner's reason and remedy; fix the owner,
  and the next due attempt settles it, or `orbit run auto --stop` retries it
  at once.
- The leaf's agent runs in claimed mode: the sandbox denies `~/.ssh`, so it
  has no direct route to the owner. The run broker carries only the scoped
  claimed-task read and other owner calls documented in the runbook; the
  agent is denied `orbit.task.update` and returns its execution summary as
  step output. `claim_handoff` carries it to the owner's `execution_summary`.
  Never loosen the sandbox to give an agent direct owner access.
- A leaf that fails before handing off settles its claim as a failure: the
  owner's task moves to `blocked` with the leaf run, failed step and error in
  its summary. Inspect the run itself on the follower (`orbit run show <run>`).
  A later claimed retry is judged on its own implementer's summary, so that
  stale `Outcome: failed` needs no hand-clearing before re-dispatch.
- `orbit run concurrency <run-id> --set N` retunes the slot ceiling live. It is
  the only ceiling: the claimed leaf jobs declare no active-run limit of their
  own, so size it to what the follower can carry.
- Each iteration reclaims the `target/` build output of every settled leaf
  whose worker has exited (`reclaimed_build_bytes`) and keeps the checkout.
  `orbit gc worktrees --confirm` on the follower removes settled leaves'
  checkouts without asking the owner; see
  [maintenance.md](../../../orbit-setup/references/maintenance.md).

## Read-only owner surface

Both probe and receipt lookup are owner-served `control_plane` tools. A replica
destination refuses them. They need an identified caller (`agent` or
`operator`); a non-interactive shell uses an agent envelope or
`ORBIT_OPERATOR=1`. They create no claim.

```bash
orbit tool run orbit.drain.probe --input '{
  "caller_version": "<this-binary-version>",
  "caller_schema": 10,
  "caller_before_pr": false
}'
```

Declaring version or schema reports the first refusal admission would raise.
`caller_before_pr` is diagnostic: whatever the follower declares, the probe
evaluates the owner's captured contract, reports it under `ship` and `review`,
and names the before-PR reviewer crew in `diagnostics` when it is on. `admits`
does not check whether this follower can resolve that crew; the pull
preflight does. The probe is never a health check for admission.

```bash
orbit tool run orbit.drain.receipt.lookup --input '{"request_id":"<request-id>"}'
```

Returns `found`, `expired` (permanent tombstone), or `not_found`. `not_found`
does not license a replacement request ID. Naming another machine's namespace
needs operator capability. Idle receipts compact immediately; unsettled claims
keep full receipts. Do not delete tombstones by age.

Claim listing is operator-only and off MCP:

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
```

It inspects. It does not reclaim.

A claimed leaf's run lives in the follower's store, so the owner's
`orbit run history` never lists it. The owner keeps each failure or release
settlement a leaf sent, with its evidence class (`provider_unavailable`,
`baseline_red`, `forge_unavailable`, `evidence_hold`, `final_recovery`,
`failure`, `summary`), typed failure class and bounded reason. Listing settled
claims needs no operator capability:

```bash
orbit run settlements --since 7d --no-reconcile --json
```

The owner's dashboard reads the same claim state, plus the accepted handoff,
inside the task detail (`orbit web serve --operator`, then
`GET /api/distributed/claims?task=<task-id>`). The endpoint accepts
`state=active|settled|all` (default `active`); settled claims are compact
summaries unless `detail=true`. There is no distributed tab; the panel appears
only for a task this workspace holds a claim for, and a replica reports that
the owner machine holds that state.

Followers must match the owner's pull request schema, independently of `orbit --version`.
The read-only probe reports `protocol_fingerprint`, a SHA-256 fingerprint of the JSON schema
derived from the running build's `AdmissionRequest` and all its nested types. The follower
first probes with legacy-compatible fields, checks that fingerprint and `protocol_schema`,
then declares `caller_fingerprint` on a second probe. A different or missing fingerprint,
including a legacy owner, refuses with typed `protocol_skew` before any `orbit.task.pull`.
The integer revision remains for persisted requests and lifecycle semantics; request field
changes no longer depend on a manual bump. Deploy matching builds on both hosts and restart
long-lived processes.

`orbit run show <drain-run>` exposes a pull drain's latest pass error and consecutive failure
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

## Recovery

Generic resume of a claimed leaf is refused:

```bash
orbit job resume <run-id>
```

Inspect the claim, task, locks, and the run on the execution machine named by
`job_run_machine`. Reconcile an uncertain merge intent before any reassignment — a
revocation or recovery is refused with `uncertain_merge_intent` until it is.

Approve, revoke and recover are owner-operator actions on the owner's dashboard
(`handoff.approve`, `handoff.revoke`, `claim.recover`). They carry the exact
candidate or phase the operator was shown and are refused with `stale_claim`
when the owner moved on. There is still no registered tool and no CLI verb for
them: do not invent one, do not ship the same task again, and do not treat
`--complete` on a follower as landing. Followers never merge, and a replica
refuses all three with `replica_checkout`. A returning worker after revocation
receives `stale_claim`.

A recovered follower pull request that merged at a head other than its
handed-off candidate completes only after an operator reconciles that merged
head on the owner. Run `orbit task reconcile-review inspect <task-id>`, then
`orbit task reconcile-review submit <task-id> --request <key>`, and follow it
with `orbit task reconcile-review status <task-id>`. The submitted run
validates and reviews exactly that head under the contract `submit` froze: the
accepted handoff's captured commands (or, when that acceptance explicitly
required none, the owner's configured commands, labelled as such) and the
review crew. Later configuration edits apply only to a new request key.
`status` names the next step,
including `accept-baseline` for a failure the base already had. Agents
cannot submit or dispose a reconciliation.

`accept-baseline` reruns the same required check at the named landed
remediation commit in a detached checkout and records its output. It refuses
the disposition unless that check passes; merged-head validation stays
recorded as incomplete. The remediation must contain the commit the provider
reports the pull request landed as (merge, squash or rebased commit), never
just the head, which a squash landing does not keep. A fix that landed before
the merge is refused before anything runs: land the fix on top of the landed
commit and name that commit. If the provider's answer changed, or the record
predates binding the landed commit, submit a new request key.

## Leftover epic and review state

There is no epic execution path. Gather evidence with ordinary commands:

```bash
orbit run history -j epic_pipeline --limit 50
orbit task list --tag epic
orbit task show <task-id> --fields status,context_files,job_run_id,job_run_machine
orbit task locks list
orbit doctor
```

Do not treat the workspace as migrated while an old epic/child run, reservation,
or uncertain landing is live, even if the root is already `review`. Inherited-only
epic roots (no own context, descendants that have some) need operator-supplied
own context; nothing inherits the old union.

The owner's clock can dispatch final recovery for an eligible blocked task;
inspect its decision, `blocked` status and `job_run_machine` before a manual
re-backlog. See [automation.md](../../../orbit-setup/references/automation.md#built-in-final-recovery-of-blocked-tasks).

## Verify

A probe with `admits: true`, matching versions and protocol, a replica role
and, when the owner's `review.before_pr` is on, a follower that resolves the
owner's `operation.review_crew` mean the hosts are **installed**. Start a drain
only when the user asked for one, and leave schedules untouched. After the
first claim, confirm on the owner that `orbit.drain.claims` shows it on the
follower's machine, and that after handoff the task is in `review` with its PR
and nothing merged. With before-PR review captured, the owner accepts a handoff
only with the leaf's passing review of exactly the handed-off head, by the
captured reviewer and contract; otherwise acceptance is refused
(`review_evidence_missing`, `review_not_passed`, `reviewed_head_mismatch`, and
related `review_*` reasons). The task's `review-gate.json` records the review.
