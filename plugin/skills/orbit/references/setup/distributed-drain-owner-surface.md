# Distributed drain: read-only owner surface

Contents:

- [Read-only owner surface](#read-only-owner-surface)

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

Rows list newest first. `--no-reconcile` reports stored claim records as-is: it
skips recovery of an interrupted coordination commit and fails instead while
one is pending. EVIDENCE is the most specific class a settlement carried;
claims settled before the owner recorded settlements read as `unrecorded`
unless their release kept a class.

The `--json` shape is
`{"settlements":[{task_id, claim_id, machine_id, machine_name, drain_run_id,
leaf_run_id, phase, last_event, settled_at, kind, evidence, failure_class,
review_decision_pending, crew, crew_model, failed_step_id, reason,
run_evidence, accounting}]}`.

`review_decision_pending` marks a before-landing review refusal awaiting a
recorded decision: the claim still fails and blocks the task, but failure scans
count it separately. Older records without the flag read as false.

Each settlement's `accounting` (`{outcome, counts_as_failure, reason}`) says how
a failure scan counts it, derived from its typed kind, class, pending-decision
flag and the leaf's recorded step code, never its reason:

- `outcome` is `failure`,
  `review_decision_hold` (the pending decision above), `evidence_hold`,
  `operator_cancel`, `upgrade_paused` (a `transient` settlement whose leaf recorded
  `upgrade_quiesce`: an Orbit upgrade paused it), `delivery_hold` (an `environment`
  settlement whose leaf recorded `delivery_stale_branch`: `worktree_setup` kept a
  retained branch untouched for operator inspection), `baseline_red` or `unrecorded`,
- with `counts_as_failure`
  (true for `failure`, `baseline_red` and `unrecorded`) and
- a `reason` token
  naming the deciding evidence, such as
  `review_decision_pending_without_candidate_failure` for a flag on a settlement
  that cannot be that hold.

It changes nothing about the claim or its task.

Each settlement's `run_evidence` is the leaf's own run record
(`{provenance, recorded_at, job_id, build, build_state, root_cause,
root_cause_state}`): `job_id`, `build` and failing step (`root_cause`), sent
with the settlement and kept in an immutable row beside the claim.

`provenance` is `settlement` (sent with the settlement),
`hydrated` (read from the follower by `orbit run settlements-hydrate`) or null
(none held); `build_state` and `root_cause_state` are `recorded` or `unrecorded`,
and `unrecorded` is never filled from the owner's or a current build or from the
settlement's text. For a claim settled before a follower sent it, an operator on
the owner reads it from the follower:

```bash
orbit run settlements-hydrate --follower-workspace <workspace> [--claim <id>] [--task <id>] [--limit 25] [--after <claim-id>] [--recheck] [--json]
```

Operator-only and bounded (default 25, at most 100); an agent, managed scan or
claimed worker is refused, and the scan above stays read-only with no operator
capability. The follower's run must be the claim's bound run, on its machine,
terminal, a claimed-leaf job for exactly its task, and its admitting drain must
pull from this owner.

The result is immutable `hydrated` evidence; no claim, task
or run changes. Per claim the outcome is `hydrated`, `replayed` (same fact held),
`conflict` (different evidence held, which stands), `mismatch`, `absent` (the
follower holds no such run; legacy evidence stays `unrecorded`), `unreachable`
(ask again later) or `no_leaf_run`.

When more claims are selected than the
limit, `remaining` counts the rest and `next_after_claim_id` names the page's
last claim; repeat with `--after <that claim-id>` and the same filters to
visit them all, unsuccessful outcomes and `--recheck` alike. A live run is a
`mismatch`: hydrate after the leaf has ended. The JSON report is
`{"rows":[{"task_id","claim_id","machine_id","leaf_run_id","outcome","detail"}],"remaining","next_after_claim_id"}`.

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
then declares `caller_fingerprint` on a second probe.

A different or missing fingerprint,
including a legacy owner, refuses with typed `protocol_skew` before any `orbit.task.pull`.
The integer revision remains for persisted requests and lifecycle semantics; request field
changes no longer depend on a manual bump. Deploy matching builds on both hosts and restart
long-lived processes.

`orbit run show <drain-run>` exposes a pull drain's latest pass error and consecutive failure
count. JSON carries `last_pass_error_code`, `last_pass_error`, `consecutive_pass_failures`, and `degraded` under
`pipeline_state.drain_last_pass`. Three consecutive failed passes latch a visible degraded
warning and stop new admissions for that drain.

A successful pass before the threshold resets
the streak. Protocol skew immediately latches degradation and ends the drain **failed** with `protocol_skew`,
even with an open window. `orbit doctor` reports the latest skewed pull drain, and the dashboard
keeps its pass health and failure code visible after it ends. Its durable admissions and settlement
records remain available to leaf workers, the settle-only pass, and the clock sweep.

Other
degraded drains keep retrying settlements and outlive their window until nothing
is unsettled; successful settlement does not clear the warning. Fix the reported cause, run
`orbit run auto --stop` to close the window, and start a new drain once this one ends. An unreadable or unwritable run-state record fails the activity visibly.
