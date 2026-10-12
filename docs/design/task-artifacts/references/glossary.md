---
type: design
summary: "Glossary: Task Artifacts"
tags: ["task-artifacts"]
last_validated: 2026-10-11
---

# Glossary: Task Artifacts

This glossary covers Orbit-specific task artifact terms. Generic issue-tracker or version-control vocabulary is excluded unless Orbit gives the term a narrower meaning.

| Term | Meaning |
|------|---------|
| **Acceptance document** | `acceptance.md`, the Markdown source of truth for validation expectations. See [2_design.md §2.3](../2_design.md). |
| **Artifact manifest** | `artifacts/manifest.yaml`, the structured index of files stored under a task's `artifacts/` directory. See [2_design.md §2.5](../2_design.md). |
| **Authority-scoped task ID** | The canonical `<prefix>-<digits>` identity (for example, `ORB-00000`) allocated by one configured machine or registry authority under its configured task prefix (2–5 uppercase ASCII letters excluding `ADR`, `L`, and `F`; a non-empty ASCII decimal suffix). Five digits is the minting minimum width, not a validation requirement. `ORB` is accepted for migrated machines but reserved for new identities. See [2_design.md §9](../2_design.md). |
| **Bundle** | The directory and files that together represent one task. See [1_overview.md §2.1](../1_overview.md). |
| **Envelope** | `task.yaml`, the small structured metadata file in a task bundle. See [1_overview.md §2.2](../1_overview.md). |
| **Canonical task bundle** | The active local task copy under `~/.orbit/tasks/workspaces/<workspace-id>/<task-id>/`. See [2_design.md §2.1](../2_design.md). |
| **Local task registry** | `~/.orbit/tasks/index.sqlite`, the mandatory local allocator, workspace-binding registry, and generated-index store. See [2_design.md §2.6](../2_design.md). |
| **Prose sidecar** | A Markdown file that stores long-form task content outside `task.yaml`. See [1_overview.md §2.3](../1_overview.md). |
| **Status-neutral directory** | The v2 canonical layout where a task bundle's path does not encode lifecycle state. See [Status-neutral task directories](../4_decisions.md#status-neutral-task-directories). |
| **Task event stream** | Append-only lifecycle and metadata rows stored in `events.jsonl`. See [1_overview.md §2.5](../1_overview.md). |
| **Typed relation** | A structured link with an explicit relation type; depending on the type, its target can be another task, an artifact, or a GitHub pull request. See [1_overview.md §2.6](../1_overview.md). |
| **Workspace ID** | The task-store partition key in `.orbit/config.yaml`, used to bind a checkout to canonical bundles under `~/.orbit/tasks/workspaces/`. Registered workspaces may use their `ws_*` ID; older standalone bindings may use a generated `<slug>-<6char>` ID. See [2_design.md §2.6](../2_design.md). |
