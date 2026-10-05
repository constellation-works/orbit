---
type: runbook
summary: Verify and recover a distributed-drain follower's before-PR reviewer, whose manifest read and report write reach the owner through the run's coordinator instead of SSH inside the sandbox.
tags: [operations, review-gate, distributed-drain, sandbox, plugin-broker]
paths:
  - "crates/orbit-core/src/adapter/command/dispatch/claimed_review.rs"
  - "crates/orbit-core/src/adapter/command/dispatch/brokered.rs"
  - "crates/orbit-core/src/adapter/tool_host/worker_tools.rs"
  - "crates/orbit-cli/src/command/mcp/claimed_review.rs"
  - "crates/orbit-cli/tests/tool/claimed_review_bridge_sandbox.rs"
related_features: [review-gate, distributed-drain, plugins, policy-sandbox]
related_artifacts: [ORB-14194, ORB-14221, ORB-14171]
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
sends the call to the owner over the follower's own SSH route.

The owner's claim fence decides both calls. The owner answers the manifest
read, and takes the report write, only while the claim is still active: running
or handed off, bound to this leaf on this follower, and still the task's
current claim. If the claim was released, failed, revoked or superseded, the
owner refuses the call with `stale_claim` and changes nothing. This holds
even while the follower's ledger still shows the reviewer running. A claim
that has outlived its reservation is not revoked by that alone. It ends when
an operator recovers it, and from then on the owner refuses its calls. A
replayed report follows `artifact.put`'s normal semantics: the same bytes
replace the same artifact.
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
| `stale_claim` | The owner no longer holds the claim as active: it was released, failed, revoked by recovery, or superseded by a later pull, or it is bound to another run | Preserve the refusal, the claim's owner state (`ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'` on the owner) and the follower's ledger. Do not replay the call. The claim's lifecycle already ended, so the review cannot finish in this run. Let the run settle. Then follow the [distributed drain runbook](./distributed-drain.md) for that claim state. |
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
# Run this block on the follower; owner commands go over the operator's own
# SSH route and select the owner workspace explicitly.
TASK='<task-id>'
OWNER='<owner-host>'
OWNER_WS='<owner-workspace>'   # registered name or ws_* id on the owner
REPO='<owner/repo>'
DRAIN_RUN='<drain-run-id>'
LEAF_RUN='<leaf-run-id>'
OWNER_LANDING_RUN='<owner-landing-run-id>'
PR_NUMBER='<pr-number>'
START="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# On both hosts, record host, version and executable hash. Record the deployed
# commit from the authenticated deployment/build receipt; orbit --version is
# a version string and does not prove which commit was installed.
hostname
orbit --version
shasum -a 256 "$(command -v orbit)"
ssh "$OWNER" 'hostname; orbit --version; shasum -a 256 "$(command -v orbit)"'
# review.before_pr is a workspace setting: read it for the owner workspace.
ssh "$OWNER" orbit config get review.before_pr --workspace "$OWNER_WS" --json # expect true

# Follower: inspect the actual drain, claimed leaf and accepted owner handoff.
orbit run show "$DRAIN_RUN" --json --no-reconcile > drain-run.json
orbit run show "$LEAF_RUN" --json --no-reconcile > leaf-run.json
orbit run show "$LEAF_RUN" --step handoff --json --no-reconcile > leaf-handoff.json

# Owner: the landing run exists only on the owner.
ssh "$OWNER" orbit run show "$OWNER_LANDING_RUN" --workspace "$OWNER_WS" \
  --json --no-reconcile > owner-landing-run.json

# Owner: inspect the task artifacts through the public tool surface. Force
# --format json (piped output is plain text otherwise) and save each complete
# wrapper. The artifact bytes are the wrapper's `.content` string: hash and
# encode that, never the wrapper. jq -j adds no trailing newline.
for ARTIFACT in review-manifest review-report review-gate; do
  ssh "$OWNER" orbit tool run orbit.task.artifact.get --workspace "$OWNER_WS" \
    --input "'{\"id\":\"$TASK\",\"path\":\"$ARTIFACT.json\"}'" \
    --format json > "$ARTIFACT.output.json"
  jq -e '.encoding == "utf8"' "$ARTIFACT.output.json" > /dev/null
  jq -j '.content' "$ARTIFACT.output.json" > "$ARTIFACT.json"
  shasum -a 256 "$ARTIFACT.json"
  base64 < "$ARTIFACT.json" | tr -d '\n'; echo
done
jq -e '.content | fromjson | {verdict, validation_complete, final_candidate}' \
  review-gate.output.json

# Owner: the reviewed head as GitHub reports it. github.pr.list returns
# {count, pull_requests}; each row carries reported_head_sha and state, and
# no merge commit.
ssh "$OWNER" orbit tool run github.pr.list --workspace "$OWNER_WS" \
  --input "'{\"state\":\"merged\",\"repo\":\"$REPO\",\"limit\":100}'" \
  --format json > merged-prs.json
jq -e --argjson pr "$PR_NUMBER" \
  '.pull_requests[] | select(.number == $pr) | {number, state, reported_head_sha, url}' \
  merged-prs.json
# The merge (landed) commit is a different identity from the reviewed head
# under a squash merge; read it from GitHub's public PR view.
gh pr view "$PR_NUMBER" --repo "$REPO" \
  --json number,state,headRefOid,mergeCommit,mergedAt,url > pr-view.json
MERGE_COMMIT="$(jq -er '.mergeCommit.oid' pr-view.json)"
gh api "repos/$REPO/git/commits/$MERGE_COMMIT" > merge-commit.json
jq -e '{merge_commit: .sha, merge_tree: .tree.sha, landing_base: .parents[0].sha}' \
  merge-commit.json
jq -e '.. | objects | select(.phase == "complete" and has("merge")) | .merge |
  {strategy, managed_merge, delivery_evidence}' owner-landing-run.json

# Follower: retain all success, failure and denial rows for each artifact call.
orbit audit list --since "$START" --run "$LEAF_RUN" \
  --tool orbit.task.artifact.get --limit 1000 --json > follower-get-audit.json
orbit audit list --since "$START" --run "$LEAF_RUN" \
  --tool orbit.task.artifact.put --limit 1000 --json > follower-put-audit.json

# Owner: find the authenticated follower transport and report attachment.
ssh "$OWNER" orbit audit list --since "$START" --tool orbit.task.artifact.put \
  --caller-machine '<follower-machine-id>' --transport ssh-mcp --limit 1000 --json \
  > owner-put-audit.json

# For every retained row, preserve its public audit identity and full details.
orbit audit show '<audit-row-id>' --json
ssh "$OWNER" orbit audit show '<owner-audit-row-id>' --json
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
- the reviewed identity: the gate's `final_candidate.commit`, the accepted
  handoff's candidate and the merged PR's `reported_head_sha` are the same
  commit;
- the landed identity, kept separate: the merge commit from `gh pr view`, its
  tree and first parent from GitHub's public Git commits API, the integration
  strategy and `managed_merge` evidence from the owner's successful completion
  step, and that step's `delivery_evidence` tying its head to the merge commit.
  A squash merge produces a new commit, so its SHA never equals the reviewed
  head. If the landed content differs from the reviewed candidate, the review
  does not cover it.

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
# Confirm the selector picks exactly one test, then run it.
NATIVE=claimed_review_bridge_sandbox::claimed_review_artifacts_cross_the_broker_from_a_confined_reviewer
cargo test --locked -p orbit-cli --test tool "$NATIVE" -- --exact --list
cargo test --locked -p orbit-cli --test tool "$NATIVE" -- --exact --nocapture
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
| `gate`, `handoff`, `landing` | Deterministic gate result and gate artifact SHA-256; accepted exact-head handoff; the merged PR's reported head (must equal `final_candidate.commit`) and, separately, its merge commit, tree, landing base, integration method and owner landing-run equivalence evidence |
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
evidence. The `native_*` and `broker_scope_unit` checks record actual runs of
the tests above and name the exact source commit and tree they ran on. Never
mark a `live_*` check or a live identity field `VERIFIED` from a final report,
fixture, source merge, or the reviewer's own summary; the record's top-level
`status` stays `NOT_VERIFIED` until the live lane is. The smoke requires a real independent repair;
a clean review with no substantive finding does not prove the repair leg.

## 5. Related references

- [Review gate runbook](./review-gate.md) — deciding a blocked review.
- [Distributed drain runbook](./distributed-drain.md) — claimed leaves and
  unreachable owners.
- [Agent-call broker design](../design/plugins/2_agent_call_broker.md) — the
  broker, its peer authentication and its error codes.
