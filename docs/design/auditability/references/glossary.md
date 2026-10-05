---
type: design
summary: "Glossary: Auditability"
tags: ["auditability"]
last_validated: 2026-09-16
---

# Glossary: Auditability

This glossary covers Orbit-specific auditability terms only. Generic observability, database, and security terms are excluded unless Orbit assigns them a specific meaning.

| Term | Meaning |
|------|---------|
| **Audit channel** | One of Orbit's distinct audit storage paths: command audit rows, v2 envelope rows, loop-event rows, blobs, or invocation metrics. See [../2_design.md §1](../2_design.md#1-storage-roots-and-audit-channels). |
| **Blob reference** | A sha256 string stored in an audit event that points to redacted content-addressed payload bytes. See [../2_design.md §5](../2_design.md#5-loop-level-provider-and-tool-events). |
| **Command audit row** | A compact SQLite `AuditEvent` record for a CLI or targeted runtime operation. See [../2_design.md §2](../2_design.md#2-command-audit-rows). |
| **Coverage matrix** | The prescriptive map from operation class to required audit channel. See [../2_design.md §3](../2_design.md#3-tool-driven-and-runtime-audit-records) and [../specs/coverage-matrix.md](../specs/coverage-matrix.md). |
| **Invocation trace** | A metrics-oriented usage record keyed by job run, activity, task ids, agent, model, tokens, and tool-call summaries. See [../2_design.md §8](../2_design.md#8-query-export-and-metrics-surfaces). |
| **Loop audit event** | A provider/tool-level event emitted by the HTTP loop engine for sessions, HTTP turns, tool calls, iteration boundaries, or policy denials. See [../2_design.md §5](../2_design.md#5-loop-level-provider-and-tool-events). |
| **Redaction boundary** | The point before durable write where Orbit removes known secret shapes from audit payloads or error strings. See [../2_design.md §6](../2_design.md#6-blob-storage-and-redaction). |
| **Run trace** | The workspace-local reconstruction material for one activity/job run: v2 envelope rows, loop-event rows, and content-addressed blobs. See [../2_design.md §4](../2_design.md#4-activityjob-envelope-events). |
| **Task history** | The persisted lifecycle and comment trail attached to a task, used beside command/tool audit rows to show task-state changes over time. See [../2_design.md §7](../2_design.md#7-identity-and-attribution) and [../specs/coverage-matrix.md](../specs/coverage-matrix.md). |
| **V2 audit envelope** | The activity/job wrapper event containing schema version, event id, run id, agent identity, optional parent id, optional workspace path, and a typed body. See [../2_design.md §4](../2_design.md#4-activityjob-envelope-events). |
| **Workspace provenance** | The `workspace_path` attached to v2 audit envelope events so traces can be filtered back to the repository that produced them. See [../2_design.md §4](../2_design.md#4-activityjob-envelope-events). |
