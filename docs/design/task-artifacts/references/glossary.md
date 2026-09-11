---
type: design
summary: "Glossary: Task Artifacts"
tags: ["task-artifacts"]
last_validated: 2026-09-11
---

# Glossary: Task Artifacts

This glossary covers Orbit-specific task artifact terms. Generic issue-tracker or version-control vocabulary is excluded unless Orbit gives the term a narrower meaning.

| Term | Meaning |
|------|---------|
| **Acceptance document** | `acceptance.md`, the Markdown source of truth for validation expectations. See [2_design.md §2.3](../2_design.md). |
| **Artifact manifest** | `artifacts/manifest.yaml`, the structured index of files stored under a task's `artifacts/` directory. See [2_design.md §2.5](../2_design.md). |
| **Authority-scoped task ID** | The canonical `ORB-00000` identity allocated by one configured authority. Bare IDs are not guaranteed unique across unrelated authorities. See [2_design.md §3](../2_design.md). |
| **Bundle** | The directory and files that together represent one task. See [1_overview.md §2.1](../1_overview.md). |
| **Envelope** | `task.yaml`, the small structured metadata file in a task bundle. See [1_overview.md §2.2](../1_overview.md). |
| **Canonical task bundle** | The active local task copy under `~/.orbit/tasks/workspaces/<workspace-id>/<task-id>/`. See [2_design.md §2.1](../2_design.md). |
| **Local task registry** | `~/.orbit/tasks/index.sqlite`, the mandatory local allocator, workspace-binding registry, and generated-index store. See [2_design.md §2.6](../2_design.md). |
| **Prose sidecar** | A Markdown file that stores long-form task content outside `task.yaml`. See [1_overview.md §2.3](../1_overview.md). |
| **Status-neutral directory** | The v2 canonical layout where a task bundle's path does not encode lifecycle state. See [Status-neutral task directories](../4_decisions.md#status-neutral-task-directories). |
| **Task event stream** | Append-only lifecycle and metadata rows stored in `events.jsonl`. See [1_overview.md §2.5](../1_overview.md). |
| **Typed relation** | A structured task-to-task link with an explicit relation type such as `blocked_by` or `supersedes`. See [1_overview.md §2.6](../1_overview.md). |
| **Workspace ID** | A stable `<slug>-<6char>` identifier stored in `.orbit/config.yaml` and used to bind a checkout to canonical bundles under `~/.orbit/tasks/workspaces/`. See [2_design.md §2.6](../2_design.md). |
