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
| `plugin_broker_refused`, `review_attempt_stale` | The reviewer finished or ran past its deadline, or its attempt was settled or replaced | Re-queue the task; the next run admits a fresh attempt |
| `plugin_broker_refused`, `review_manifest_stale` | The owner holds another attempt's manifest | Re-queue the task; the next run pins its own manifest |
| `plugin_broker_refused`, `claimed_review_bridge_refused` | Another activity, task, path, request field, report attempt, or a malformed report | A prompt or binary mismatch: confirm both hosts run the same Orbit version, then re-queue |
| `plugin_broker_unavailable`, "could not reach this run's coordinator" | The step runner stopped or restarted while its reviewer ran | Let the run fail and retry it; check `orbit run show <leaf-run-id>` for why the step runner stopped |
| `capability_denied`, "ORBIT_PLUGIN_BROKER is not set" | The follower's binary predates the route, or launched the reviewer without a broker | Upgrade the follower, then re-queue |
| An `orbit.task.artifact.put` source-path or size refusal | The report source is outside the workspace `.orbit/tmp`, is a link out of it, or is over 1 MiB | Fixed in the reviewer, not the host; the refusal happens before anything reaches the coordinator |

An owner that cannot be reached over SSH fails the call outside the sandbox
with the owner-route error, which the coordinator returns unchanged. Recover it
as an unreachable owner in the
[distributed drain runbook](./distributed-drain.md).

## 3. Smoke procedure

Run this on a follower whose drain claims leaves from an owner with
`review.before_pr = true`. These commands only read state. The live
end-to-end proof for a deployment is recorded separately in the rollout
record below.

```bash
# Owner and follower: same binary, review gate on.
orbit --version
ssh <owner-host> orbit --version
ssh <owner-host> orbit config get review.before_pr      # expect: true

# Follower: the claimed leaf that ran the reviewer.
orbit run show <leaf-run-id>          # Claim: line, and the agent_review_repair step

# Follower: one brokered row per bridged call, under the reviewer activity.
sqlite3 -readonly ~/.orbit/orbit.db "SELECT tool_name, status, brokered, peer_pid,
  task_id, activity_id, job_run_id FROM audit_events
  WHERE tool_name IN ('orbit.task.artifact.get','orbit.task.artifact.put')
    AND task_id = '<owner-task-id>' ORDER BY timestamp"

# Owner: the follower's calls arrive over ssh-mcp, and the report is attached.
ssh <owner-host> orbit audit list --tool orbit.task.artifact.put \
  --caller-machine <follower-machine-id> --transport ssh-mcp --json
ssh <owner-host> orbit tool run orbit.task.artifact.get \
  --input '{"id":"<owner-task-id>","path":"review-report.json"}'
ssh <owner-host> orbit task show <owner-task-id> --json | jq .review.verdict
```

Expect these results:

- The follower has at least one `success` row for each tool, with
  `brokered = 1`, a `peer_pid`, the owner task and `agent_review_repair`.
- The owner has a `success` put row from the follower over `ssh-mcp`.
- The report's `attempt_id` matches the attempt in the owner's
  `review-manifest.json`.
- The verdict is set by the gate's settlement, not by the report.

Record each result in the evidence schema below.

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

Record one JSON object per verification in the shape of
[`claimed-review-artifacts-rollout.json`](./claimed-review-artifacts-rollout.json):

| Field | Meaning |
| --- | --- |
| `candidate` | Commit SHA of the Orbit binary on both hosts |
| `owner`, `follower` | Host name and machine id, plus `orbit --version` |
| `checks.<name>.status` | `VERIFIED`, `FAILED`, `NOT_RUN` or `NOT_VERIFIED` |
| `checks.<name>.evidence` | The command run and the excerpt of its output that decides the status |
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

The record starts with every check `NOT_VERIFIED`. Change a check only with
evidence; never mark a check `VERIFIED` from a final report or from the
reviewer's own summary.

## 5. Related references

- [Review gate runbook](./review-gate.md) — deciding a blocked review.
- [Distributed drain runbook](./distributed-drain.md) — claimed leaves and
  unreachable owners.
- [Agent-call broker design](../design/plugins/2_agent_call_broker.md) — the
  broker, its peer authentication and its error codes.
