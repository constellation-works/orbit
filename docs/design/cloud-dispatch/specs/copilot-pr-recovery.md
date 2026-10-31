---
title: "Scope: Copilot repair of an existing pull request"
owner: codex
status: Draft
feature: cloud-dispatch
doc_role: spec
type: design
summary: "Proposed opt-in Copilot delegation for existing-PR CI and conflict repairs, sharing Orbit's repair ownership and budgets while preserving exact-head validation and separate merge authority."
tags: [cloud, copilot, recovery, ci, github]
last_updated: 2026-10-29
last_validated: 2026-10-29
---

# Scope: Copilot repair of an existing pull request

Proposal only. Nothing here enables Copilot, sends a prompt, changes current
auto-fix routing, or grants merge authority. The existing
[cloud-dispatch scope](../1_scope.md) concerns new Claude cloud deliveries;
this companion covers repair of an already published candidate. Its phases
are independent of that proposal's PTY/session implementation.

## 1. Outcome and boundaries

An operator can delegate an evidenced CI failure or merge conflict on an
Orbit-owned PR to Copilot programmatically. Orbit retains the task, PR and
candidate history, records the request and remote work, then validates the
result through its existing delivery rules. The agent's completion message,
a reaction, or a successful workflow wrapper cannot certify a repair.

First scope: an open, same-repository PR, its existing head branch and its
actual target branch. Repair must preserve the original task's intent. Start
with an explicit operator request and an explicit repaired-head acceptance;
bounded automatic dispatch/adoption comes only after the verification phase.

The operator flow is inspect blocker → request one remote repair → observe
progress/approval needs → accept the exact repaired head → normal review and
delivery. A refusal names what is missing rather than silently selecting a
different agent or starting another attempt.

Out of scope: new implementation issues/PRs, forks, cross-repository work,
arbitrary hosted agents, subscription provisioning, new model/provider enums,
an Internet-facing Orbit endpoint, and Copilot deciding approval or merge.
The CI-failure sweep continues to collect and file evidence; it does not
silently become a paid-agent dispatcher.

## 2. GitHub contract and eligibility

Checked against primary documentation on 2026-10-29; these are provider
capabilities, not a live account/repository certification:

| Route | Documented behavior | Scope decision |
|---|---|---|
| Agent Tasks REST API | `POST /agents/repos/{owner}/{repo}/tasks`; `base_ref` plus `head_ref` finds an existing PR and commits to that branch. Creation returns a task ID; GET exposes state/session/artifact records. | Preferred when the actual account supports it. Set both refs and `create_pull_request: false`; verify the exact existing PR first. |
| PR comment | A writer can mention `@copilot` to request CI/conflict repair; by default commits go to the current PR branch. | Explicitly selected compatibility transport, subject to the spike proving programmatic user-authenticated triggering and observable attribution. |
| Issue assignment | REST/GraphQL can assign an issue and start work. | Later new-work scope; never use it as a fallback that opens a replacement PR. |

The endpoint-specific [Agent Tasks reference][tasks-api] currently requires
Copilot Business/Enterprise for task creation, a user access token with
Agent tasks read/write permission, and excludes installation access tokens.
The [general API guide][api-guide] describes broader paid-plan availability.
Treat that discrepancy as an eligibility gap: do not promise that a paid
subscription, an existing `gh` login, or comment access implies API access.
Pin and test the adapter's API version (the reference uses `2026-03-10`).
Keep model selection optional and provider-discovered; record requested and
observed models separately, with unavailable observations explicit.

GitHub documents [existing-PR changes and conflict requests][pr-work], but
that is not proof that every bot or installation-authored comment triggers
the agent. Prove the selected user identity/transport on the intended account.
Comments need repository write access. Read-only readiness reports missing
entitlement, policy, authentication or observation capability without starting
a session. Credentials stay in the host's credential facility; tasks,
prompts, artifacts and agent environments receive references, never tokens.

Choose one transport before admission and persist it for the attempt. There
is no automatic API-to-comment fallback after a request might have executed.
Creation/list/detail operations do not establish a cancel or force-stop
contract; do not invent one.

## 3. Admission reuses the existing repair decision

Reuse owner-side readiness, evidence classification, attempt accounting and
footprint rules. Extend the existing repair record with a remote executor;
do not build a second competing auto-fix service or another repair budget.
Relevant current anchors are:

- `crates/orbit-core/src/application/job/pr_repair/task_auto_fix.rs`: operator
  readiness/start, live-run refusal, and completion authority.
- `crates/orbit-engine/src/executor/automation/vcs/pr/auto_fix/admission.rs`:
  admitted, duplicate and exhausted repair decisions.
- `crates/orbit-engine/src/executor/automation/vcs/pr/auto_fix/checks.rs`:
  checked failure evidence and the inherited/transient repair split.
- `crates/orbit-engine/src/executor/automation/vcs/pr/auto_fix/published_head.rs`:
  only a contiguous host-recorded publication chain advances a pinned head.
- [Distributed-drain repair and landing](../../distributed-drain/2_design.md#31-attempt-ownership-and-recovery)
  and [activity/job delivery](../../activity-job/2_design.md): retained
  candidates, claims, review, publication and settlement remain authoritative.

Admit only with explicit remote-repair authority, an eligible observed
blocker, account capability, remaining shared attempt/deadline budget and
exclusive ownership of this PR's mutation. Existing `auto_fix: true` or a
configured local auto-fix crew does not opt into Copilot. A repair request
does not acquire completion authority, reset exhausted attempts, approve
proposed work, or bypass a human hold.

CI eligibility requires authenticated evidence for the exact PR head and all
relevant unsuccessful jobs. Candidate failures can request code repair.
Inherited red-base failures retain the existing base-recovery route; seven
PRs failing the same base defect must not launch seven Copilot sessions.
Known transient failures retain the bounded rerun route, which itself needs
rerun authority. Pending, missing, truncated or ambiguous evidence holds
without a model call. An archive/deduplication refusal is not a code-repair
request. Conflicts need a fresh GitHub/base observation and the preserved
candidate; old-head CI reruns cannot resolve them.

Immediately before dispatch, re-read the task contract, PR identity, head,
base and ownership. A changed identity invalidates the prepared request.
Retain the typed reason and evidence when admission refuses.

The documented REST request selects branch refs, not an expected-head SHA
precondition. Preflight read plus POST is therefore not an atomic GitHub
handoff. Carry the captured SHA in the prompt, but never treat that instruction
as enforcement. Record any observable remote starting revision and recheck
the resulting ancestry/diff; an unprovable or changed starting revision needs
operator reconciliation. Do not claim the API itself fences a concurrent push.

## 4. One mutating owner, including manual Copilot sessions

Local auto-fix, failure recovery, follower continuation and remote Copilot
must share one owner-held PR repair reservation. An extant live worker or
foreign claim blocks remote admission until its supported handoff releases
mutation authority. Do not kill it, clear its reservation or edit receipts.
The admitted remote attempt excludes local retry/rebase/push paths for the
same PR while allowing unrelated work to progress.

This fence governs Orbit writers, not every actor with GitHub write access.
Out-of-band human/provider updates can still happen. Detect and preserve
those interventions; they supersede stale admission instead of being
overwritten or silently attributed to this repair.

If a human has already started Copilot, first attach the existing observable
session to an audited operator adoption; do not send a duplicate prompt.
If the provider cannot establish that session's identity/state, report
external work in progress and hold. A comment timestamp, commit author or
agent response alone cannot prove exclusive ownership.

An expired Orbit deadline is not a remote cancellation. Retain an outstanding
remote-owner fence when work may still push. Releasing it or switching back
to a local repair needs verified remote quiescence, or an explicit operator
reconciliation that establishes exclusive branch ownership. If the selected
transport cannot prove quiescence, report the missing proof and do not start
a competing writer. Disabling routing prevents new requests; it does not
cancel existing sessions or erase their obligations.

## 5. Durable request and ambiguous-send handling

Extend the existing attempt ledger with a versioned record containing:

- owner workspace and task/run/claim bindings, task contract digest;
- repository ID, PR number, exact head/base repositories and branch names,
  original head/base SHAs, blocker kind and normalized evidence signature;
- authorized actor, selected executor/transport, prompt digest, stable request
  marker, attempt/deadline and shared budget reservation;
- returned comment/task/session IDs and URLs, observed model/state, bounded
  timestamps/diagnostics, and any accepted old-head/new-head transition.

The request identity represents the repair episode, not each poll or each
new base commit. Base movement, a new run ID or a transport failure must not
mint an unlimited series of repair attempts. Record intent and spend the
reservation atomically before the external call. Repeated clicks, drain
passes, restarts and retries find that same intent.

| Durable state | Permitted next work |
|---|---|
| Prepared, no send attempted | Revalidate; a proven pre-send refusal can release its unused reservation. |
| Dispatching, reply unknown | Observe only; the remote session may already exist. |
| Submitted/active | Poll the recorded remote identity and PR; never re-send. |
| Result observed | Validate/adopt the exact repaired head, or retain a typed hold. |
| Held, expired, failed or cancelled | Preserve evidence and any outstanding remote-owner fence; no implicit local fallback. |

Neither documented route supplies a creation idempotency guarantee. On a
lost response, reconcile through supported task/session or authenticated
comment listings using the recorded marker, actor and exact request digest.
The marker is a correlation aid, not an authority token. Read/pagination
limits, multiple matches or a missing result do not prove that no send took
effect. If no unique receipt can be recovered, hold as dispatch-unknown;
never retry merely because a timeout, `5xx` or empty listing was returned.
Definite rejection can be distinguished from ambiguous delivery, but cannot
silently change transport or refund an unknown paid request.

The prompt names the preserved PR/task intent, captured refs/SHAs, evidence
and allowed repair. It asks for the existing branch, no merge, no substitute
PR and no widening of task scope. Redact secrets and bound log excerpts;
external logs, comments and documents remain untrusted evidence. Provider
instructions are useful guidance, not enforcement of the owner-side fence.

## 6. Observe, adopt a new SHA, then use normal delivery gates

Prefer task/session GETs where supported, with bounded polling/backoff and
the original completion deadline. Comment transport must establish an
observable request/session relationship in the spike. An eyes reaction is
acknowledgement only; `completed` is a provider observation, not a verdict.
`idle`, waiting-for-user, failed, timed-out, cancelled and unknown states
remain distinct and must not become success by default.

GitHub may require a human to [approve workflows after a Copilot push][pr-work].
Represent this as awaiting workflow approval. Missing check rows are not
green, and no automatic approval click, credential elevation, CI rerun or
follow-up paid prompt follows from this repair scope.

On a changed head, re-fetch provider/git evidence for the same PR, repository
and target branch. Compare the old candidate and repaired diff against the
task contract and preserve prior partial review evidence as historical.
The first implementation requires an explicit operator acceptance of that
exact repaired head. It must append a distinct owner-authenticated external
repair transition; never fabricate `pr_auto_fix_pushed`, rewrite the old
publication checkpoint, or accept an arbitrary branch movement as a local
repair ancestor.

For initial acceptance, require the captured head to be an ancestor of the
new head and the captured target base to be incorporated when resolving a
conflict. Non-fast-forward rewriting needs explicit reconciliation, outside
the first automatic-adoption scope. These ancestry checks are necessary,
not sufficient: changed files, task meaning and full review/validation still
gate adoption. Do not trust a Copilot author string as origin proof.

Re-run applicable validation and review for the new diff/head through normal
policy. Prior-head green checks or review cannot pay its obligations unless
an existing authenticated equivalence rule explicitly permits reuse. Respect
an operator's no-rerun instruction as a hold or explicit separate disposition,
not permission to pretend those checks passed. Refuse stale head/base/contract
bindings immediately before any publication or merge.

Remote completion itself never merges, closes the task or advances coverage.
After acceptance, resume the existing PR through the existing review/merge
authority and exact-current-head required checks. No `--admin`, force push or
branch-protection change is supplied by this adapter. An independently merged
PR follows normal external-merge settlement; preserve who merged it and any
explicit exception, without turning that exception into standing policy.

## 7. Minimal product and implementation surface

Keep the core change generic: remote executor choice on an existing repair
attempt, durable ownership/receipt states, and authenticated head acceptance.
Copilot request formats, actor/session discovery and GitHub API versioning
belong to the GitHub integration/adapter. This is not a new model family,
general cloud-agent framework or public Orbit service.

Proposed controls, not implemented config/CLI names: default-off workspace
opt-in; explicit transport and host credential binding; remote concurrency
and daily dispatch caps; the existing shared per-PR attempt/deadline limits;
and inspect/request/adopt operations. Refuse zero/invalid bounds and expose
effective values. Select the executor before an attempt; there is no automatic
local-to-Copilot cost escalation. Readiness and doctor checks make no paid call.

An operator sees the current blocker, exact head, repair owner, request/session
link, attempt/deadline, latest observation and next needed action. Missing
provider cost/model telemetry stays unavailable, not zero or attributed to the
local wrapper crew. The repair record and external references survive restart,
upgrade and read-only older clients; unsupported versions refuse before any
external write. No silent migration resets an intent or permits a second send.

## 8. Delivery phases and evaluation

1. **Capability spike, separately authorized.** Use a disposable same-repository
   PR and the actual account. Establish the chosen transport, entitlement,
   credential identity, same-PR behavior, observable task/session linkage,
   model reporting, workflow-approval behavior and remote quiescence limits.
   Include a controlled lost-response reconciliation exercise. Record explicit
   unavailable results; do not infer eligibility from a subscription label.
2. **Operator-triggered MVP.** Implement one proven transport, shared admission/
   ownership/budget, intent-before-send, observe-only ambiguous recovery and
   explicit exact-head acceptance. End in the existing review path. No new
   scheduler, automatic fallback or provider-wide dispatch framework.
3. **Evaluate before opt-in automation.** Exercise public boundaries with a fake
   forge/provider: duplicate clicks/restart/lost reply, wrong actor or PR,
   stale task/head/base, active local/follower/manual remote owner, changed
   head without a matching request, no-diff completion, missing/skipped CI,
   workflow approval hold, timeout with possible late push, and partial review
   preservation. Use the repository's test policy; add regressions for observed
   defects and avoid tests pinning prompt wording or model/schedule values.
   A separately authorized live trial must demonstrate one send per episode,
   no competing writer and no merge without fresh authority/evidence.
4. **Bounded automatic routing.** Only after those results, allow explicit
   workspace opt-in to route already-admitted eligible repairs to Copilot and
   adopt heads whose provider linkage/ownership and review rules are proven.
   Missing proof retains manual acceptance. Compare with local repair on time
   to validated head, dispatch count, unresolved/ambiguous attempts, actual
   observed spend, regressions and operator interventions; include failures
   and unvalidated results. Do not require or assume a model-quality advantage.

Acceptance of this scope does not authorize the spike, paid sessions, live
automation, implementation dispatch, merges, releases or credentials changes.
After scope review, file implementation and evaluation work against the phase
boundaries above with the capability result as a prerequisite, not a required
live paid call embedded in a code task's acceptance criteria.

## 9. Decisions still requiring evidence or operator choice

- Which transport is actually available to the target account and can recover
  an unambiguous request/session identity? The API plan discrepancy is open.
- What observable provider event is sufficient for automatic repaired-head
  attribution/quiescence? Until established, acceptance and ownership transfer
  remain explicit operator actions.
- Which account/model and daily/concurrency caps should the workspace opt into?
  Use operator configuration; no hard-coded subscription/model defaults here.
- If Copilot rewrites history or opens an unexpected PR, retain and reconcile
  it explicitly; do not widen the first implementation to auto-adopt it.

[tasks-api]: https://docs.github.com/en/rest/agent-tasks/agent-tasks
[api-guide]: https://docs.github.com/en/copilot/how-tos/use-copilot-agents/cloud-agent/use-cloud-agent-via-the-api
[pr-work]: https://docs.github.com/en/copilot/how-tos/use-copilot-agents/cloud-agent/use-cloud-agent-on-github
