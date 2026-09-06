---
name: orbit
description: Orbit — the task/audit/dispatch layer for AI coding agents. Use for anything touching a task ID, `.orbit/`, `orbit` CLI or `orbit.*` MCP tools, the docs corpus, friction records, jobs and `jrun-*` runs, routines, auto-tasks, the sweep clock, worktree GC, task publication and recovery, federated MCP, the dashboard, or setting Orbit up on a machine or repository. Covers both doing work through Orbit and configuring Orbit so that work runs unattended.
---

# Orbit

Orbit governs AI coding agents: every change is a task, every task carries an
audit trail, and work dispatches into sandboxed parallel runs. Durable
operations go through the registered tool surface — never direct lifecycle
subcommands, never a rebuild from source.

This file is the router. It carries only what every Orbit call needs; each
reference below is loaded on demand.

## Tool Invocation

Use the connected, authoritative MCP server for durable work. First call
`orbit_workspace_list`, select the intended workspace, and copy its selector
into workspace-scoped calls. A direct server returns `ws_*` IDs; a federated
server returns host-qualified `selector` values. Never infer authority from
cwd or silently substitute a different machine's local store.

Registered CLI tools use `orbit tool run orbit.task.add --input '{...}'`;
MCP uses `orbit_task_add({...})`. Dots become underscores, but **CLI tool
registration does not imply MCP exposure**. Discover the connected server's
`tools/list` before calling. Use CLI administration on the intended host only
when the operation and authority are available and authorized.

Include `model` (`codex`, `claude`, `gemini`, or `grok`) on calls whose schema
supports attribution. This is the agent family, not the crew or model ID.
Follow each advertised schema for other fields; examples in these references
use placeholders that must be replaced, not sent literally.

Read [tool-surface.md](references/tool-surface.md) for the actual capability
split, routing, structured examples, and missing-operation handling. Filesystem
inspection and CLI-only diagnostics in these references refer to the host that
owns the selected workspace. They do not authorize local shadow records.

A plain `orbit_task_show` returns full detail; `field`/`fields` project it.
If an activity already injected a task snapshot, use that envelope first.

## Lifecycle

```text
proposed → backlog → in-progress → review → done
         ↘ rejected

someday → in-progress
blocked → in-progress

review      → rejected
rejected    → backlog | in-progress  (reconsider)
```

Creating a task does not authorize dispatch or completion. Follow the user's
approval and repository delivery policy. The diagram summarizes common paths;
`task.start` also supports authorized pickup from `proposed` with a real plan.
Use `blocked` when execution cannot safely continue. Provenance follows the
surface: `orbit tool run ...` is agent-driven, bare `orbit task ...` is
human-driven.

## Common Mistakes — DO NOT

| Mistake | Why it fails | Correct form |
|---------|-------------|--------------|
| `cargo run -- tool run ...` | Rebuilds from source instead of using the installed binary | `orbit tool run ...` |
| `orbit task show <id>` | Direct CLI subcommands skip agent provenance tracking | `orbit tool run orbit.task.show --input '{"id":"<id>"}'` |
| Editing task/friction projections or runtime evidence under `.orbit/` | File edits bypass the canonical store and audit contract | The matching registered tool; versioned config/resources are a separate admin surface |
| Inventing a task ID | IDs are allocated by the store | `orbit.task.add` returns it |

## References

**Working through Orbit** — doing the work itself:

| Reference | Read it for |
|---|---|
| [concepts.md](references/concepts.md) | What each Orbit noun means and how they nest. Read this first if the vocabulary is new. |
| [tool-surface.md](references/tool-surface.md) | MCP versus CLI, workspace routing, authority, discovery, and common calls. |
| [task-authoring.md](references/task-authoring.md) | Writing a task someone else can execute without guessing. |
| [task-execution.md](references/task-execution.md) | Carrying a task from pickup to verified implementation and handoff. |
| [task-review.md](references/task-review.md) | Reviewing someone else's work against its acceptance criteria. |
| [search.md](references/search.md) | Finding prior tasks, docs, and frictions by topic, path, or related task. |
| [docs-corpus.md](references/docs-corpus.md) | Authoring and registering the markdown corpus agents retrieve from. |
| [friction.md](references/friction.md) | Recording what made the work harder than it should have been. |
| [orchestration.md](references/orchestration.md) | Driving a backlog through: `orbit run ship`, `run auto`, epics, and keeping parallel runs from colliding. |
| [workflows.md](references/workflows.md) | Jobs, activities, and the `orbit run` surface. |
| [run-debugging.md](references/run-debugging.md) | A `jrun-*` run that failed, stuck, or was cancelled. |
| [common-failures.md](references/common-failures.md) | Matching a known failure signature to its remedy, once the failing step is identified. |
| [operational-logs.md](references/operational-logs.md) | Host-level incidents: sweep service failures, tracing warnings, missing log output. |

**Setting Orbit up** — configuring a machine or repository so work runs well.
Use these references to configure the host and workspace. Scheduled automation
is seeded disabled and requires deliberate enablement:

| Reference | Read it for |
|---|---|
| [setup/first-run.md](references/setup/first-run.md) | Zero to a working workspace: install, host identity and task prefix, `workspace init`, MCP client registration, verification. |
| [setup/linux-sandbox.md](references/setup/linux-sandbox.md) | Linux Bubblewrap/AppArmor prerequisite, capability probe, sandbox guarantees, and supported remediation before dispatch. |
| [setup/configuration.md](references/setup/configuration.md) | `orbit config` keys, crews and executors, policies and filesystem profiles, environment passthrough. |
| [setup/automation.md](references/setup/automation.md) | The scheduler: OS clock → `orbit sweep` → routines → jobs. The seven seeded routines and the order to enable them. |
| [setup/auto-tasks.md](references/setup/auto-tasks.md) | Recurring work as data — definitions that mint tasks on a schedule, instead of new code. |
| [setup/publication.md](references/setup/publication.md) | Publish task snapshots to a dedicated Git repository, inspect them, and restore on the owning authority. |
| [setup/maintenance.md](references/setup/maintenance.md) | Worktree GC, `orbit doctor` repairs, log retention, audit pruning, upgrades. Skipping the first is what breaks busy workspaces. |
| [setup/multi-host.md](references/setup/multi-host.md) | More than one machine: task-ID namespaces, routine host pins, and what syncs versus what stays local. |
| [setup/remote-access.md](references/setup/remote-access.md) | Reaching another machine's Orbit: the dashboard, `web connect`, and MCP over SSH or a socket. |

## Start here

- New to Orbit, or `.orbit/` is absent → [concepts.md](references/concepts.md), then [setup/first-run.md](references/setup/first-run.md).
- Preparing Linux for dispatch → [setup/linux-sandbox.md](references/setup/linux-sandbox.md) before starting an agent.
- Given a task ID → [task-execution.md](references/task-execution.md).
- Asked to create work → [task-authoring.md](references/task-authoring.md).
- "Why didn't this fire / run / clean up?" → [setup/automation.md](references/setup/automation.md), then [setup/maintenance.md](references/setup/maintenance.md).
- A run id in hand → [run-debugging.md](references/run-debugging.md).
- Task backups, publication, or recovery → [setup/publication.md](references/setup/publication.md).
- Multiple hosts, missing tools, or permission denials → [tool-surface.md](references/tool-surface.md), then [setup/remote-access.md](references/setup/remote-access.md).
