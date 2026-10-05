---
type: runbook
summary: Verify and recover a distributed-drain follower's before-PR reviewer, whose manifest read and report write reach the owner through the run's coordinator instead of SSH inside the sandbox.
tags: [operations, review-gate, distributed-drain, sandbox, plugin-broker]
paths:
  - "crates/orbit-core/src/adapter/command/dispatch/claimed_review.rs"
  - "crates/orbit-core/src/adapter/command/dispatch/brokered.rs"
  - "crates/orbit-cli/src/command/mcp/claimed_review.rs"
  - "crates/orbit-cli/tests/tool/claimed_review_bridge_sandbox.rs"
related_features: [review-gate, distributed-drain, plugins, policy-sandbox]
related_artifacts: [ORB-14194, ORB-14171]
last_validated: 2026-10-05
---

# Verify and Recover Claimed-Review Artifacts

Use this runbook when a distributed-drain follower runs the before-PR review
of a claimed leaf, and you need to check that the reviewer can read its
manifest and attach its report, or to recover a review that reported
`incomplete` because one of those calls was refused.

## 1. The route

A claimed leaf's task lives on its owner. The follower's reviewer runs inside
the agent sandbox, which masks `~/.ssh`, so it cannot reach the owner
itself. Its nested `orbit`, from `orbit tool run` or `orbit mcp serve`, hands
exactly two calls to the run's coordinator: the plugin broker that the step
runner starts outside the sandbox (`ORBIT_PLUGIN_BROKER`).

- `orbit.task.artifact.get` of `review-manifest.json`
- `orbit.task.artifact.put` of `review-report.json`

The coordinator takes the task, claim, owner and attempt from its own records,
never from the request. It carries a call only when all of these hold:

- the run is the claim's bound leaf;
- the step is the reviewer activity (`agent_review_repair`);
- the review ledger shows one open attempt, admitted by that leaf, whose
  reviewer is running and inside its deadline.

The coordinator also checks the content. It accepts a manifest only if the
owner's copy names the running attempt. It accepts a report only if the
report parses, is at most 1 MiB and names that attempt. The coordinator then
sends the call to the owner over the follower's own SSH route, and the
owner's claim fence still decides the write. A replayed report follows
`artifact.put`'s normal semantics: the same bytes replace the same artifact.
The coordinator never writes a review certificate. Every other tool and path
is refused.

The design is in the
[agent-call broker design](../design/plugins/2_agent_call_broker.md) §3
("Claimed-review artifacts").

## 2. Refusals and recovery

Every refusal reaches the reviewer as an error. The reviewer reports
`incomplete`, and the gate blocks the task as in the
[review gate runbook](./review-gate.md) §3. None of these refusals is fixed
by adding SSH credentials to the sandbox, loosening the sandbox profile,
attaching a report by hand, or disabling before-PR review.

| Error the reviewer sees | Cause | Recovery |
| --- | --- | --- |
| `review_attempt_stale` | The reviewer finished or ran past its deadline, or its attempt was settled or replaced | Preserve the call and run evidence. Do not replay it. After the normal run reaches terminal settlement, verify there is no live owner, diagnose the cause, and use the existing authorized backlog recovery to start a fresh attempt. |
| `review_manifest_stale` | The owner holds another attempt's manifest | Preserve the call and run evidence. Do not replay it. After terminal settlement and cause diagnosis, use the existing authorized recovery to start a fresh attempt. |
| `claimed_review_bridge_refused` | Another activity, task, path, request field, report attempt, or a malformed report | Preserve the refusal and inspect the bound task, run, claim and review attempt. Correct the diagnosed prompt or binary cause before any normally authorized fresh run. |
| `plugin_broker_unavailable`, "could not reach this run's coordinator" | The coordinator could not provide a usable response; the owner may or may not have received the request | Treat the outcome as unknown. Preserve both runs, the claim and ledger state, and use the existing idempotent/reconciliation path. Do not manufacture another claim or report. |
| `plugin_broker_busy` | The listener rejected the connection before dispatch because its bounded queue is full | Retry only the same call and bytes while the admitted attempt remains current, following the retryable response. Do not create a second claim or report. |
| `capability_denied`, "ORBIT_PLUGIN_BROKER is not set" | The coordinator capability was not passed to this process, or the follower binary/launch configuration is wrong | Preserve the refusal and inspect the run's launch evidence. Confirm the deployed binary hash and broker setup before normal recovery; a version string alone is not proof. |
| An `orbit.task.artifact.put` source-path or size refusal | The report source is outside the workspace `.orbit/tmp`, is a link out of it, or is over 1 MiB | Fixed in the reviewer, not the host; the refusal happens before anything reaches the coordinator |

An owner that cannot be reached over SSH fails the call outside the sandbox
with the owner-route error, which the coordinator returns unchanged. Recover it
as an unreachable owner in the
[distributed drain runbook](./distributed-drain.md).

If the report PUT is refused, the reviewer cannot persist an authoritative
`incomplete` report. Keep its refusal as diagnostic evidence and let the gate
fail closed on the missing or invalid report. A response envelope or a
manually attached artifact does not replace the report artifact.

## 3. Smoke procedure

The live before-PR smoke is owned by on-call after safe activation. Run the
normal claimed Mac lane against the deployed owner and follower, and collect
the evidence below through supported public commands. Do not start a second
claim, edit a task store, or substitute a fabricated reviewer report. The
retained rollout record starts as `NOT_VERIFIED` and is updated only from the
observed lane.

```bash
# Set the lane's task and run IDs from the authorized on-call record before it
# starts. Record START in UTC before dispatch so refusals are retained too.
TASK='<task-id>'
DRAIN_RUN='<drain-run-id>'
LEAF_RUN='<leaf-run-id>'
OWNER_LANDING_RUN='<owner-landing-run-id>'
START="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# On both hosts, record host, version and executable hash. Record the deployed
# commit from the authenticated deployment/build receipt; orbit --version is
# a version string and does not prove which commit was installed.
hostname
orbit --version
shasum -a 256 "$(command -v orbit)"
ssh '<owner-host>' orbit config get review.before_pr # expect true
ssh '<owner-host>' 'hostname; orbit --version; shasum -a 256 "$(command -v orbit)"'

# Follower: inspect the actual drain, claimed leaf and accepted owner handoff.
orbit run show "$DRAIN_RUN" --json --no-reconcile
orbit run show "$LEAF_RUN" --json --no-reconcile
orbit run show "$LEAF_RUN" --step handoff --json --no-reconcile
orbit run show "$OWNER_LANDING_RUN" --json --no-reconcile

# Owner: verify the actual merged PR and compare its current head to the
# accepted gate final_candidate and owner landing output.
orbit tool run github.pr.list --input \
  '{"state":"merged","repo":"<owner/repo>","limit":100}'

# Owner: inspect the task artifacts through the public tool surface. Save the
# complete JSON output for each call; jq -j preserves content without adding
# a newline when hashing its UTF-8 bytes.
orbit tool run orbit.task.artifact.get --input "{\"id\":\"$TASK\",\"path\":\"review-manifest.json\"}" > review-manifest.output.json
jq -j '.content' review-manifest.output.json | shasum -a 256
jq -jr '.content' review-manifest.output.json | base64 | tr -d '\n'
orbit tool run orbit.task.artifact.get --input "{\"id\":\"$TASK\",\"path\":\"review-report.json\"}" > review-report.output.json
jq -j '.content' review-report.output.json | shasum -a 256
jq -jr '.content' review-report.output.json | base64 | tr -d '\n'
orbit tool run orbit.task.artifact.get --input "{\"id\":\"$TASK\",\"path\":\"review-gate.json\"}" > review-gate.output.json
jq '{verdict, validation_complete, final_candidate}' review-gate.output.json

# Follower: retain all success, failure and denial rows for each artifact call.
orbit audit list --since "$START" --run "$LEAF_RUN" \
  --tool orbit.task.artifact.get --limit 1000 --json > follower-get-audit.json
orbit audit list --since "$START" --run "$LEAF_RUN" \
  --tool orbit.task.artifact.put --limit 1000 --json > follower-put-audit.json

# Owner: find the authenticated follower transport and report attachment.
ssh '<owner-host>' orbit audit list --since "$START" --tool orbit.task.artifact.put \
  --caller-machine '<follower-machine-id>' --transport ssh-mcp --limit 1000 --json \
  > owner-put-audit.json

# For every retained row, preserve its public audit identity and full details.
orbit audit show '<audit-row-id>' --json
ssh '<owner-host>' orbit audit show '<owner-audit-row-id>' --json
```

Read every saved run and audit response; do not infer fields that the public
projection does not return. Correlate the review step, the authenticated
caller/process machine and transport, the task, the review attempt, and the
manifest/report contents. The required evidence chain is:

- task, drain, leaf, reviewer and owner-landing run IDs; owner and follower
  machine identities; deployed commit and executable SHA-256 on both hosts;
- claim ID and settlement result; manifest attempt, base and candidate SHAs;
- exact manifest and report bytes with SHA-256, plus the public audit row IDs
  for authenticated GET and PUT and any refusal/failure rows;
- an independent reviewer finding with a substantive repair, its repair
  commit and diff, and the report's repaired finding and paths;
- `review-gate.json` with `validation_complete: true`, the deterministic
  verdict and `final_candidate`; the report verdict by itself is not evidence;
- accepted handoff output bound to the exact reviewed candidate, then owner
  landing evidence showing the landed head equals `final_candidate` and the
  corresponding PR is merged at that head.

The public audit projection does not expose every stored broker diagnostic.
Use its returned row ID with `orbit audit show`; do not query Orbit's SQLite
database directly or relabel another field as `peer_pid`.

Record the actual evidence and row IDs in the rollout record. Keep each field
`NOT_VERIFIED` until its evidence has been collected from the live lane.

### Native boundary regressions

These run the real public producer and consumer: a real owner home and a real
probe, pull and bind; the reviewer inside the real agent sandbox, calling
`orbit tool run` and `orbit mcp serve`; and an `ssh` stand-in that needs
`~/.ssh/known_hosts`.

```bash
# macOS (sandbox-exec), and Linux with unprivileged user namespaces (bwrap):
cargo test -p orbit-cli --test tool claimed_review_bridge_sandbox -- --nocapture
# Any Unix host: the broker's scope, refusals, lost answer and stopped broker.
cargo test -p orbit-core --lib dispatch::tests::claimed_review
```

The sandbox test prints `skipped claimed-review bridge sandbox integration:`
and passes when the platform sandbox cannot start, for example inside another
Bubblewrap sandbox. A skip is not evidence. Record it as `NOT_RUN` with its
reason, and run the test on a host that can start the sandbox.

## 4. Evidence schema

The retained
[`claimed-review-artifacts-rollout.json`](./claimed-review-artifacts-rollout.json)
has one object for this rollout. Keep the following identity chain explicit
in that record:

| Field | Meaning |
| --- | --- |
| `candidate`, `owner`, `follower` | Candidate commit and executable hash; host, machine ID, version and executable hash for both hosts |
| `runs` | Drain, claimed leaf and owner landing run IDs |
| `claim` | Claim ID and observed settlement state |
| `review` | Attempt ID, base, candidate, final reviewed head and substantive finding/repair evidence |
| `artifacts` | Exact manifest/report bytes and hashes, plus GET/PUT audit row IDs |
| `gate`, `handoff`, `landing` | Deterministic gate result, accepted exact-head handoff, landed head and merged PR evidence |
| `checks.<name>.status` | `VERIFIED`, `FAILED`, `NOT_RUN` or `NOT_VERIFIED` |
| `checks.<name>.evidence` | The command run, retained public response/audit row IDs and the observed result |
| `checks.<name>.recorded_at` | RFC 3339 time of the run |
| `checks.<name>.reason` | Why a check is `NOT_RUN` or `FAILED` |

The checks are:

- `native_macos_sandbox`: the sandbox test on a Mac.
- `native_linux_bwrap`: the sandbox test on a Linux host that can start
  Bubblewrap.
- `broker_scope_unit`: the orbit-core test.
- `live_follower_get` and `live_follower_put`: the follower audit rows above.
- `live_owner_attach`: the owner audit row and report.
- `live_no_credentials`: no sandboxed process read `~/.ssh` (the follower's
  sandbox denies it, so any SSH inside the sandbox fails with "Host key
  verification failed").
- `live_refusal`: one refused call on a stale attempt, with nothing written
  on the owner.

The record starts with every stage `NOT_VERIFIED`. Change a stage only with
evidence; never mark one `VERIFIED` from a final report, fixture, source merge,
or the reviewer's own summary. The smoke requires a real independent repair;
a clean review with no substantive finding does not prove the repair leg.

## 5. Related references

- [Review gate runbook](./review-gate.md) — deciding a blocked review.
- [Distributed drain runbook](./distributed-drain.md) — claimed leaves and
  unreachable owners.
- [Agent-call broker design](../design/plugins/2_agent_call_broker.md) — the
  broker, its peer authentication and its error codes.
