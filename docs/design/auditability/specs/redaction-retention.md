---
type: design
summary: Spec: Redaction and Retention Boundaries
last_validated: 2026-10-07
---

# Spec: Redaction and Retention Boundaries

Audit storage must preserve enough detail to reconstruct agent behavior while redacting known secret shapes before durable persistence.

## Why This Exists

Auditability and secrecy pull in opposite directions. Orbit needs faithful records, but it must not make provider keys, bearer tokens, or sensitive environment values durable by accident.

## Redaction Invariants

- BlobStore redacts bytes before computing the stored hash.
- CLI output captures above the capture limit (1 MiB by default) redact the
  diagnostic prefix with the surrounding captured bytes before cutting it to
  half the limit. Only complete redacted prefix lines are retained, so a
  long token cannot leave a fragment at that cut. The newest complete-line
  tail remains raw for provider protocol parsing and is redacted by BlobStore
  before storage. Captures at or below the limit retain their original bytes.
- Command audit error messages are scrubbed for sensitive live environment values before insertion.
- Completed provider result strings, including nested objects and arrays, are
  scrubbed for sensitive live environment values and known secret patterns before
  they become downstream step input or persisted pipeline state. JSON keys and
  non-string values retain their types.
- HTTP-shaped payload redaction covers authorization headers, x-api-key headers, JSON API-key fields, and bearer tokens.
- Shared pattern redaction covers high-confidence provider token shapes embedded in prose; exact whole-token artifact fields are rejected instead of persisted.
- Shared pattern redaction covers structural OpenSSH fingerprints, public-key comments, and connection hosts without applying a general hostname or high-entropy-string heuristic.
- CLI argv redaction uses HTTP defaults plus bare `sk-...` token scrubbing when argv-shaped data is being persisted.
- Orbit artifact write tools use the action-keyed field policy in [artifact-redaction.md](./artifact-redaction.md) before YAML/markdown/JSON persistence.
- Default tracing output redacts string field values, `Debug`-formatted field values, and unstructured `message` fields before writing stderr or either global JSONL feed.
- Sensitive environment values are matched as live text and, when different, as the JSON-string body and the Rust `Debug` body of that text. A multi-line private key or a value containing quotes or backslashes therefore does not survive serialization into an audit blob or a tracing `?` field.
- Readers should not need to apply the standard redactor again for normal stored blobs.

## Retention Boundaries

Command audit rows:

- Live in the configured SQLite audit database.
- Are queryable, exportable, and prunable through `orbit audit`.
- Are also pruned past `retention.audit_days` by `orbit gc audit --apply`, host-wide, in the same batches as the run audit.
- Should remain compact and should not embed transcript bodies.

Activity/job and loop traces:

- Live under `.orbit/state/audit/` for workspace-local run reconstruction.
- V2 envelope and loop events are persisted through the SQLite-backed v2 audit store.
- Use content-addressed blobs for payload bodies.
- Are bounded by `orbit gc audit`: it plans by default and, with `--apply`, deletes this workspace's v2 audit rows older than `retention.audit_days` (default 60) in batches of 1,000 rows, each its own write transaction, then sweeps the blobs under `.orbit/state/audit/blobs/` that no remaining run audit row, run step or pipeline state names. The opt-in `store-gc` routine runs it daily. `VACUUM` stays an operator action; each report shows the freed pages.
- A blob is protected from the sweep while it is being published. The writer records `.orbit/state/audit/pending/<hash>` before it writes or reuses a blob and retires the marker after the row naming it commits; writing content that is already stored refreshes the blob's mtime. The sweep moves a candidate to `.orbit/state/audit/sweep/`, then rechecks the marker and the mtime (24-hour grace) and moves it back if either is fresh. A marker older than the audit cutoff belongs to a write that never published and is reclaimed with its blob.

Run pipeline state:

- Lives in `job_run_states`, beside the `job_runs` row.
- Is dropped by `orbit gc runs --apply` for success, failed, timeout, cancelled and interrupted runs older than `retention.runs_days`, which also stamps `job_runs.archived_at`. Run rows, steps and summaries stay; held and non-terminal runs are never selected.

Invocation metrics:

- Live in SQLite as usage records.
- May be recomputed or summarized into scoreboards.
- Are not a transcript retention mechanism.

Global process tracing:

- Uses `~/.orbit/state/logs/orbit.jsonl` for operational events and separately budgeted `orbit-agent.jsonl` beside it for agent stdout/stderr. Readers merge the active feeds; both use the same redaction and age limit.
- Is append-only within the active file; oversized files are renamed to dated archives and old archives are pruned from long-lived processes and when the active file exceeds its size budget.
- Is an operational log stream, not the canonical workflow envelope.
- Carries policy-denial path/resource strings, so default tracing redaction is part of its durability boundary.

## Failure Modes

- If a redactor misses an unknown secret shape, the audit layer may persist that value. Reviewers should treat new provider payload shapes as redaction-sensitive changes.
- If redaction changes payload bytes, the stored hash identifies the redacted payload, not the raw provider payload.
- If v2 audit-store writes fail, the run may continue with in-memory audit snapshots only; durable reconstruction is incomplete.
- `orbit audit prune` deletes command audit rows only; run audit rows and blobs are pruned by `orbit gc audit`.
- A blob whose write crashed before its row committed keeps its pending marker and survives until the marker is older than `retention.audit_days`.
- A blob named by pipeline state survives its audit row until `orbit gc runs` drops that state. A blob named by a step response or error is kept for as long as the step row exists, since run retention keeps steps.

## Migration Path

Redaction changes are forward-only. Orbit does not automatically sweep existing authored records, because masking without an author review can destroy their meaning. Git history rewriting is outside the artifact-write boundary and requires a separate, explicit operator incident-response decision. [ORB-10591]

Future retention work should add:

- JSONL and invocation-metric figures in the `orbit doctor` `store-retention` row, which already reports reclaimable audit, run-state and blob bytes
- optional hash manifests for file-backed audit bundles
- audit records for export and prune operations
- a documented legacy handling policy for pre-`.orbit/state/audit/` run traces

## Agent Signature

Last revised by codex / gpt-5.5 for [T20260427-0023].
