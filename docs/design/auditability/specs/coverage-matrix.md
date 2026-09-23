---
type: design
summary: Spec: Audit Coverage Matrix
last_validated: 2026-09-23
---

# Spec: Audit Coverage Matrix

Every Orbit operation that touches code, task state, persistent runtime state, external services, or provider/tool execution must have an audit channel. This matrix defines the expected channel by operation class.

## Why This Exists

"Audit everything" is too vague to enforce in review. A coverage matrix gives reviewers a concrete checklist and makes known gaps visible.

## Operation Classes

| Operation class | Required audit channel | Minimum fields |
|-----------------|------------------------|----------------|
| Top-level CLI command | Command audit row | command, subcommand, role, status, duration, cwd |
| Orbit tool mutation | Command audit row or targeted runtime row | tool/action name, target type/id, actor, status |
| MCP tool preflight or dispatch | Command audit row | tool, status, trusted workspace/caller/process, transport, full capability set, origin session, unique call id, optional run/lease |
| Task lifecycle update | Command/tool audit plus task history | task id, actor, status transition or document field changed |
| Task lock reservation/check/release | Targeted command audit row | reservation target, files payload, status, conflict/denial state |
| Activity/job run | V2 envelope audit row | run id, agent identity, run start/finish |
| Activity/job step | V2 envelope audit row | run id, step id, outcome or skip/retry reason |
| Filesystem policy decision inside v2 run | V2 envelope audit row | profile, op, path, allowed, matched rule |
| Filesystem or proc-spawn policy denial live projection | Global tracing JSONL | target, tool, path, profile, matched_rule |
| Friction task submission live projection | Global tracing JSONL | target, task_id, agent, model, summary |
| HTTP provider turn | Loop audit row plus blob store | run id, session id, provider, model, request/response blob hashes |
| Tool call inside HTTP loop | Loop audit row plus blob store | run id, session id, tool name, input/output blob hashes, outcome |
| CLI backend provider invocation | V2 envelope audit row | provider, redacted argv, stdin/stdout/stderr blob refs, timeout, exit code |
| Invocation cost/usage | Invocation store | job run id, activity id, task ids, agent, model, token usage, tool-call summary |
| Audit export/prune | Command audit row (planned) | command, actor, target, status |

## Invariants

- A mutation path must not rely only on stdout text for auditability.
- A provider request or response must not be stored inline in a command audit row.
- A policy or allowlist denial must record both the denied subject and the reason.
- Live tracing projections must be additional to canonical audit, error, log, or scoreboard writes.
- A task-state mutation must leave task history or document metadata in addition to any audit row.
- New operation classes must update this matrix in the same PR that introduces the operation.
- Registry-backed and in-process MCP tools must have parity for preflight denial, runtime success, and tool failure, with one trusted call context across the path.
- MCP initialize/tool JSON cannot become trusted audit identity, capability, transport, or correlation; malicious-payload wire tests must pin this boundary.
- MCP capability, workspace, binding, role, owner, placement, and unknown-name denials produce exactly one row with D2's call ID before runtime/store mutation; checkoutless/global denials use the global coordination audit database.

## Known Gaps

- `orbit audit` commands are not currently wrapped by `AuditGuard`, so export and prune operations are not command-audited.
- Some deterministic automation actions rely on enclosing job context rather than dedicated targeted audit rows.
- CLI backend tool allowlists are recorded as harness-delegated advisories, not enforced Orbit denials.
- Command audit stdout/stderr fields exist but are not broadly populated.
- Friction report submissions have no active producer for the live `orbit.friction.reported` JSONL event; the CLI and web formatters retain only its renderer.

## Review Checklist

When reviewing a change that mutates code or state, ask:

1. Which operation class does this change add or modify?
2. Which audit channel records success?
3. Which audit channel records failure or denial?
4. Which actor or agent/model identity is persisted?
5. Are payloads redacted before durable storage?
6. Does the change require a new ADR because it changes the audit contract?

## Agent Signature

Last revised by codex for [ORB-10262].
