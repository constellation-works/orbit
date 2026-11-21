---
type: design
summary: "Spec: V2 Audit Envelope"
tags: ["activity-job"]
last_validated: 2026-11-04
---

# Spec: V2 Audit Envelope

Activity / Job runs emit a structured v2 audit tree that describes run, step, activity, and control-flow structure. This tree is stored as append-only SQLite audit rows with payload bodies in redacted content-addressed blobs.

## Why This Exists

Payload bodies alone do not describe job structure. Reviewers need to answer questions like:

- which job run emitted this activity?
- which step retried?
- which branch failed a join?
- which workspace produced this run?

The v2 envelope adds that structure.

## Event Tree

Every event carries:

- `schemaVersion`
- `event_type`
- `event_id`
- `ts` (timestamp)
- `run_id`
- `agent_identity`
- optional `parent_event_id`
- optional `workspace_path`

Common event families are:

- `run.*`
- `step.*`
- `activity.*`
- construct-level events for `parallel`, `fan_out`, and `loop`
- policy/tool denial and CLI invocation events

`cli.invocation.started` includes redacted argv, stdin blob ref, optional model, wall-clock timeout, and optional `cwd`. When present, `cwd` is the subprocess working directory selected by Activity/Job's workspace resolver.

Historical HTTP agent-loop rows can be correlated with envelope rows by shared run identity. They are not v2-envelope descendants: those SQLite loop-event rows do not carry an envelope `parent_event_id`. Current provider invocations emit CLI envelope events.

## Persistence Layout

In production, envelope events append to the SQLite `v2_audit_events` table with `source: v2_envelope`.

Historical `source: loop_event` rows remain opaque payloads in the same table; current sinks do not emit them. Content-addressed blobs remain under:

```text
.orbit/state/audit/blobs/<hh>/<hash>
```

The writer's `events_snapshot()` reads events back from its configured persistence sink; it returns `SnapshotUnavailable` when no inspectable sink is attached. Production CLI inspection reads the persisted SQLite audit rows and blob store.

## CLI Inspection

`orbit run events [run_id]` is the human-facing chronological reader for the v2 envelope. It resolves the same default run as `orbit run show`, prints timestamp, derived activity `step.id`, event type, and a concise body summary, and can emit the flattened raw event objects as JSON with derived `step_id` attached to descendant events.

`orbit run trace [run_id]` renders the `event_id` / `parent_event_id` tree. JSON mode returns deterministic `roots` and `orphans` arrays so partial traces remain inspectable instead of silently dropping events whose parent is absent.

`orbit run logs` reads CLI stdout/stderr through the same runtime-owned envelope accessor rather than parsing the file layout in the CLI command module. `orbit run show -s <id>` treats the activity DAG `step.id` from `step.started` events as the primary step identifier, with legacy job-run target IDs and numeric step indexes retained as fallbacks. This inspection surface landed in [T20260426-0705] and [T20260426-0709].

## Invariants

- Envelope writes are append-only rows whose payload is one JSON object.
- Disk persistence failure does not crash the run by itself; the writer records a failure counter and tracing error and continues counting attempted envelope emissions. It retains no in-memory fallback event stream, so failed writes leave gaps in the persisted audit trail.
- `workspace_path` is attached when the caller has a meaningful repo identity.
- CLI invocation start events include `cwd` when the runtime selected a subprocess working directory.
- Parent stacks propagate into worker threads so nested branch/worker events remain traversable.

## Failure Modes

- Audit writer mutex poisoning surfaces as a structured audit failure.
- SQLite persistence can fail even when event emission returns an event ID. Inspect the audit-failure count or degraded-audit flag rather than treating successful emission as proof of persistence.
- Reviewers need envelope rows and the blob store to reconstruct a run; older HTTP agent-loop runs may also have historical loop-event rows.

## Migration Notes

- Retiring the HTTP agent-loop writer does not rewrite historical loop-event rows or their referenced blobs.
- CLI agent events are first-class envelope events, so every agent run is visible in the envelope stream.
- File-backed runtime traces moved from `.orbit/audit/` to `.orbit/state/audit/` in [T20260426-0519]. Existing `.orbit/audit/` files are legacy local artifacts rather than the current write target.

## Agent Signature

Last revised by `codex` for [T20260508-8].
