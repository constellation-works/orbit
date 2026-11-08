---
type: design
summary: "Spec: who may post, update and close fleet notices (operator, orchestrator, managed agent), and the size, rate and open-count limits"
last_validated: 2026-11-05
title: Spec — Notice authority and limits
owner: claude
status: Draft
feature: fleet-notices
tags: [fleet-notices, spec, security]
related_features: [fleet-notices, policy-sandbox, federated-mcp]
related_artifacts: [ORB-21207]
last_updated: 2026-11-05
---

# Spec: Notice authority and limits

Managed agent activities that opt into brief delivery read notices, so posting one can place a
fleet-wide message in other agents' prompts. The rules below decide who may post, in which scope
and kind, for how long, and who may close what.
The poster class is derived by the owner from the caller's session; a caller cannot choose it.
Reading is open to every caller that can reach the workspace. Holding a notice tool never grants
anything else: a notice cannot authorize a tool, a path, a status change or a skipped gate.

## Why This Exists

Without these rules a single confused or prompt-injected agent could broadcast instructions to the
whole fleet, impersonate the operator, close the operator's incident notice, or flood the board.
The guardrails bound each of those: agents post only about their own task with evidence, only in
their workspace, for at most a day; nothing but the operator can close the operator's notices; and
every class has size, rate and open-count limits.

## Poster classes

The owner resolves the class once per call, from the same caller resolution every governed
operation uses (`CallerEnvelope`, `crates/orbit-common/src/governance/authorization/caller.rs`):

| Class | Resolved when | Recorded `author` |
|---|---|---|
| `operator` | the caller holds the `Operator` capability and is not bound to a managed run (interactive CLI, an `orbit mcp serve --operator` session, the dashboard's operator session) | `class: operator`, `machine_id` |
| `agent` | the caller is bound to a managed run: a validated worker binding (`WorkerInvocation`) or a run-stamped tool context naming a task. The bound task is the only task it may act for | `class: agent`, the bound `task_id` and `run_id`, the run's crew and model |
| `orchestrator` | an `Agent`-capability session **not** bound to a managed run that carries orchestrator attribution: `orbit mcp serve --orchestrator <crew>`, or a per-call `orchestrator` naming a configured crew | `class: orchestrator`, `crew`, `model` |
| refused | anything else: an unbound agent session without orchestrator attribution, an unknown caller | — (`notice_poster_unidentified`) |

Order matters: a run-bound session is `agent` even if it also carries orchestrator attribution.
Managed worker sessions have `Operator` removed when the binding is installed; an unbound session
with `Operator` capability resolves as `operator` before orchestrator attribution. The `by` field
of the original direction is this `author.class`; the input has no field that sets it.

**Honest limit.** Orchestrator attribution is self-declared today ([ORB-11313] made it
attribution, not a grant). So `orchestrator` is "an unmanaged agent session that says which crew
it is", the same trust the task-filing surface already extends. The operator-only rights below do
not depend on it; the orchestrator rights are deliberately the ones a mistaken unmanaged session
could exercise without lasting harm (everything expires within the orchestrator caps).

## Rights

| Action | `operator` | `orchestrator` | `agent` |
|---|---|---|---|
| Post `scope: workspace` | yes | yes | yes, home workspace = the bound task's workspace only |
| Post `scope: global` | yes | yes | no (`notice_scope_refused`) |
| Post `kind: incident` / `info` | yes | yes | yes |
| Post `kind: maintenance` | yes | yes | no (`notice_kind_refused`) |
| Expiry cap | [notice-record](./notice-record.md#expiry) | same | same (24 h) |
| Must carry `linked_task` or ≥1 `evidence` | no | no | yes (`notice_evidence_required`) |
| Update an open notice it authored | yes | yes | yes, same bound task |
| Update another's open notice | yes | yes, when the author is `orchestrator` or `agent` | no (corroborates) |
| Close a notice it authored | yes | yes | yes, same bound task |
| Close another's notice | yes | yes, when the author is `orchestrator` or `agent` | no (`notice_close_refused`) |
| Close an `operator` notice | yes | no | no |

An agent's `linked_task` may be its own bound task or any task held by the same owner host,
typically the fix it found or filed; its `evidence` may cite its run. An agent may not post on behalf
of another task, and the input has no field to name one.

## Re-posting an open subject

A post whose dedupe key matches an open notice ([notice-record](./notice-record.md#dedupe)):

- **updates** when the poster has "update" right on the matched notice per the table above;
- otherwise **corroborates**: the content is untouched and only `reports` changes.

A [repeat](./notice-record.md#retries) of the notice's latest write is handled before these rules and
changes nothing they would change: for an editor and a corroborator alike it leaves content,
`expires_at` and `updated_at` unchanged, applies no expiry request, and is checked before expiry
validation, so it is never refused with `notice_expiry_invalid` — including a lost-reply retry
whose absolute `expires_at` is now less than 5 minutes ahead; it still returns `updated` or
`corroborated`.

An update may widen `scope` from `workspace` to `global` only if the updater may post global; it
may never narrow a `global` notice another class authored. An update may change `kind` only within
the updater's allowed kinds. This is how an orchestrator promotes an agent-reported incident to a
fleet-wide one: it re-posts the same key with `scope: global`, which updates the agent's notice
(the author stays the agent; the audit row names the orchestrator).

## Limits

All values are fixed (`application/notice/limits.rs`); exceeding one refuses the post with
`notice_limit_exceeded` naming the limit, and writes nothing.

| Limit | Value | Applies to |
|---|---|---|
| `max_title_chars` | 120 | every post |
| `max_body_bytes` | 4096 | every post |
| `max_evidence` | 8 | every post |
| `max_open_per_workspace` | 20 | effective-open notices with this home workspace, all classes |
| `max_open_global_per_owner` | 10 | effective-open `global` notices across one owner host's workspaces |
| `max_open_per_agent_task` | 2 | effective-open notices authored by one bound task |
| `agent_posts_per_task_per_day` | 5 | posts (including corroborations, excluding [repeats](./notice-record.md#retries)) by one bound task in 24 h |
| `orchestrator_posts_per_hour` | 30 | posts by one orchestrator crew per owner host, excluding [repeats](./notice-record.md#retries) |

Operators are bounded by the open counts and sizes but have no post rate. Counts are computed in the
posting transaction against effective state, so an expired notice frees its slot without a write.
A repeat counts against no limit and is never refused by one. Closing is not rate-limited.

## Refusals

Refusals from the core post and close use cases are typed errors with stable codes and leave an
audit row with target type `notice`. Input parsing or argument-conversion refusals can occur before
those use cases run, so they do not leave a notice-use-case audit row. The target id is present when
the use case can resolve or create one. These core refusals do not partially apply:

`notice_poster_unidentified`, `notice_scope_refused`, `notice_kind_refused`,
`notice_evidence_required`, `notice_expiry_exceeds_cap`, `notice_expiry_invalid`,
`notice_limit_exceeded`, `notice_close_refused`, `notice_not_found`, `notice_invalid_field`,
`notice_agent_posting_disabled` (only from an owner older than [ORB-22808], whose
`notice.agent_posting = false` refused agent-class posts and closes), and the federated `capability_refused` when a replica receives a write
([notice-federation](./notice-federation.md#writes)).

## Invariants

- The poster class is never read from input.
- No notice field is consulted by any authorization, admission, review or validation decision in
  v1. The only planned reader that acts on a notice is the dispatch-hold extension, which reads
  only `operator`- and `orchestrator`-authored `maintenance` notices
  ([2_design.md §7](../2_design.md#7-extension-point-maintenance-dispatch-hold)).
- A managed agent's notice always names its bound task and run, so every agent-authored word in
  another agent's brief traces to one run.

## Agent Signature

claude (claude-opus-5-5), 2026-11-02, [ORB-21207].
