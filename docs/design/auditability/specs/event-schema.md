---
type: design
summary: Spec: Audit Event Schema
last_validated: 2026-10-31
---

# Spec: Audit Event Schema

Audit events in Orbit are split by operational level, but every channel must preserve enough stable identity, timing, status, and join keys for incident reconstruction.

## Why This Exists

Orbit has more than one audit storage mechanism. This spec prevents "audit event" from becoming an ambiguous term and defines the minimum contract each channel must keep.

## Channel Contracts

### Command Audit Event

Command audit events are SQLite rows represented by `AuditEvent`.

Required invariants:

- `execution_id` is non-empty and unique within the command audit table.
- Producer-generated `execution_id` values use the shared prefix + timestamp + process id + per-process sequence helper; new producers must not roll timestamp-only ids.
- `timestamp`, `command`, `role`, `status`, `exit_code`, `duration_ms`, `working_directory`, and `pid` are always present.
- `status` is one of `success`, `failure`, or `denied`.
- A denied command uses `status: denied` and a non-zero exit code.
- CLI command rows are finalized after dispatch and output rendering, including
  stream execution. Their duration includes that work; a rendering or stream
  error records failure with exit code 1. Successfully rendered payloads retain
  their declared exit code and audit message. Closing stdout is a successful
  rendering outcome.
- A following CLI stream has no completed command row while it is running.
  Tool-owned audit rows retain their runtime dispatch scope; the CLI guard
  suppresses its own row when tool dispatch already recorded one.
- Error messages are redacted before insertion.
- Command rows do not embed full provider request/response bodies.
- Legacy `host`, `session_id`, and `job_run_id` retain their meanings: executing-process hostname, legacy session correlation, and canonical run correlation respectively.
- MCP rows may add `workspace_id`, caller/process machine and display-host IDs, transport, the complete effective capability set, `origin_session_id`, per-call `trace_id`, best-effort `caller_ip`, the untrusted `self_reported_actor`, and, on federated routed calls only, the compatibility `mcp_call_id`. V1 MCP sessions carry no execution leases, so `lease_id` stays null on their rows.
- Host-authorized application calls add an optional `application` object containing the non-secret consent record and the active scope index/callback flag; the credential secret is not included.
- `trace_id` is minted once per `tools/call` by the accepting MCP server and is shared by that call's denial or dispatch outcome. Two calls in one origin session have distinct trace IDs. A federated routed call may also carry the compatibility `mcp_call_id`; it is not required on every MCP row and is not the direct per-call correlation key.
- MCP capability authorization and filters use set membership. There is no authorizing member, ordinal, ceiling, or max-capability field.
- External MCP JSON may populate only the legacy workspace address selector. Trusted audit identity and correlation come from the adapter/runtime and authenticated managed envelope; standalone MCP role is `unverified`.
- A trusted managed-run ID fills an empty `job_run_id` or must match it. Managed-run correlation adds no lease column for v1 MCP sessions, which carry no execution leases.
- MCP `initialize` may supply `self_reported_actor`: an unverified client claim kept in its own column. It never changes `role`, the canonical `actor_*` projection, or any authorization decision.

Failure modes:

- If the command exits through an early return, the RAII guard still writes one row.
- The CLI explicitly drops the finalized guard before a process exit, which
  would otherwise bypass destructors. Closed stdout during rendering unwinds
  to the rendering boundary so the success row can be persisted first.
- If audit insertion fails during `Drop`, the CLI prints a warning and does not mask the command result.
- If a successful tool call's runtime audit insertion is lock-busy, it is retried once. If the insertion still fails, a read-only call returns its result unchanged with a warning, and a mutating call returns success with `audit_warning` (`code: audit_row_unpersisted`, naming the committed `tool` and `id`), because the mutation is already committed. A failed tool call keeps its own error. The non-fatal `Drop` rule applies only to guard-side RAII persistence.
- If runtime initialization fails before the guard exists, no command audit row is written.

### V2 Activity/Job Audit Event

Activity/job audit events are represented by `V2AuditEvent` and persisted by the v2 audit store.

Required invariants:

- `schemaVersion`, `event_type`, `event_id`, `ts`, `run_id`, and `agent_identity` are always present.
- `event_id` is unique within a `run_id`.
- Child events set `parent_event_id` when emitted under a parent context.
- `workspace_path` remains optional; CLI/library callers with a real workspace should attach it, while smoke and stub hosts may omit it.
- Body variants use the `body_kind` discriminator.
- `cli.invocation.started` includes optional `cwd` when Activity/Job resolved a subprocess working directory.
- A run starts with a run-start event and finishes with a run-finished event whenever execution reaches the dispatch wrapper.

Failure modes:

- If v2 audit-store persistence fails, the writer counts and logs the failure and the run continues. No in-memory copy of the event is kept, so sink-backed snapshots omit it and the durable trail is incomplete for that event.
- If a worker thread emits events, it must install the parent stack snapshot before emission.
- If an activity/job run fails before writer construction, no v2 audit event is expected.

### Loop Audit Event

Loop audit events are sink events represented by `LoopAuditEvent`; the durable runtime sink persists them as v2 audit rows.

Required invariants:

- Every event includes `run_id`.
- Session-scoped events include `session_id`.
- HTTP request/response bodies and tool input/output payloads are stored as blob hashes, not inline bodies.
- Policy denials record the denied tool and reason.
- Blob bytes are redacted before hashing and writing.

Failure modes:

- If a blob write fails, the sink may return an `error:<message>` reference; readers must treat that as a failed payload capture.
- If no durable sink is installed, `NullSink` intentionally drops events and blobs. Production activity/job callers should not use it.

### Invocation Trace

Invocation traces are metric records, not transcript records.

Required invariants:

- Records identify `job_run_id`, `activity_id`, agent, optional model, task ids, duration, token usage, and tool-call summaries.
- Invocation traces must not be treated as proof that transcript-level audit exists.
- A missing invocation trace is a metrics gap; a missing audit event is an audit gap.

## Migration Rules

- New audit event body variants must be added to the relevant enum and documented here or in a linked feature spec.
- Existing body variants are append-only unless an ADR records a breaking migration.
- If a channel moves storage roots, readers should either support the previous root explicitly or document that historical files are manual-only.

## Agent Signature

Last revised by codex / gpt-5.5 for [T20260508-8].
