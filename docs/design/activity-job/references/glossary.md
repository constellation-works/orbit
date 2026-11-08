---
type: design
summary: "Glossary: Activity / Job"
last_updated: 2026-11-03
last_validated: 2026-11-07
---

# Glossary: Activity / Job

This glossary covers Orbit-specific execution-substrate vocabulary used by the Activity / Job docs. It deliberately excludes generic workflow terms such as "retry," "quorum," and "timeout" unless Orbit gives them a more specific meaning.

| Term | Meaning |
|------|---------|
| **ActivityV2** | The typed `schemaVersion: 2` activity shape: shared metadata plus one concrete runtime variant. See [../2_design.md §2](../2_design.md). |
| **Retired `backend:`** | Removed agent execution selector. `cli` still parses and is ignored; `http` / `auto` are refused at load. See [../specs/backend-resolution.md](../specs/backend-resolution.md). |
| **TargetRef** | Authoring-facing job-step reference: `activity:<name>` resolves to a concrete `TargetStep`; granted `script:<name>` and durable `auto-task:<name>` references remain for their dedicated dispatch paths. See [../2_design.md §3](../2_design.md). |
| **ToolAllowlistHarnessDelegated** | Advisory envelope event emitted on the CLI agent path to say Orbit resolved the activity tool policy for the provider invocation. Orbit enforces it at its own managed tool boundaries; native tools remain delegated to the harness. See [../2_design.md §7.1](../2_design.md). |
| **V2AuditEnvelope** | Structured audit wrapper carrying run/step/activity provenance for the v2 runtime. See [../specs/audit-envelope.md](../specs/audit-envelope.md). |
| **RuntimeHost** | The orbit-core to orbit-engine boundary trait that supplies deterministic actions, provider credentials, CLI command resolution, and `ToolContext`. See [../2_design.md §6](../2_design.md). |
| **Workspace Path Provenance** | The absolute repo path attached to envelope events so the shared audit trail can be filtered by originating workspace. See [../specs/audit-envelope.md](../specs/audit-envelope.md). |
