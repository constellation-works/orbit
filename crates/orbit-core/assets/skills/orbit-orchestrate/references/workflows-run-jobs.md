# Inspect and run a job

## Dispatch and scheduling

Orbit's execution layer. This covers the mechanics; for deciding *what* to
dispatch see [orchestration-dispatch.md](orchestration-dispatch.md), and for scheduling it see
[automation-scheduler.md](../../orbit-setup/references/automation-scheduler.md).

## Concepts

- **Job** — a deterministic, multi-step pipeline from the installed catalog.
  Version 2 YAML composes activities; version 3 also admits `auto-task:<name>`
  steps with durable waits and accepted results (see
  [auto-task workflows](workflows-auto-tasks.md#version-3-auto-task-workflows)). Discover with
  `orbit job list` / `orbit job show <id>`.
- **Activity** — one named step definition referenced by a job's step list
  (`agent_implement`, `task_pilot`, `git_commit`, `git_push`, `pr_open`,
  `worktree_setup`, `reserve_locks`, ...). Activities are never invoked directly
  by CLI; a job's step list references them. `orbit job show <id>` names the
  activity each step runs.
- **Run** — one execution, with a `jrun-*` id, a durable state bundle under
  `.orbit/state/job-runs/`, and an audit trail.

For a task-backed agent activity using `tools` as an allowlist, Orbit computes
`effective_tools = deduplicate(activity.tools union task.required_tools)`. Task
requirements are immutable after creation. The
activity list remains the baseline; an empty task requirement list preserves it
exactly. When one agent activity selects a batch, Orbit unions the requirements
from every selected task into that same effective list.

Admission rejects
invalid required names before provider launch, and
the run envelope, `ORBIT_ACTIVITY_TOOLS`, and audit evidence carry the effective
list. A deny-list activity instead exposes registered agent-facing tools except
those covered by `tool_disallow_list`; a task requirement never overrides that
list.

Filing refuses an unliftable covered requirement (a mutating tool, an
always-operator-only tool, or a name a wildcard entry still covers), and a
stored one still refuses dispatch when covered. A read/repeat-safe exact
denial can still be required, because a grant could lift it. The shipped `agent_implement` uses
this deny-list mode. Tool inclusion does not bypass later role, capability, policy, sandbox,
subprocess, or authentication checks.

## Running a job

```bash
orbit job list                                   # catalog
orbit job show <job_id>
orbit run job <job_id> --input key=value --json
orbit run job <job_id> --input crew=<name> --json # override the run's crew for this run
orbit run job <job_id> --wait                    # block until terminal; nonzero unless it succeeded
orbit run history --json
orbit run history -j <job_id>
orbit run show <run_id> --json
```

There is no `--crew` flag on `run job` — crew selection is always a run input.
That picks the run's resolved crew (`resolved_run_crew` in `orbit run show
--json`); an individual activity can still route elsewhere via an explicit
activity `crew` or a `crew_config_key` pool, which overrides even an explicit
request. [run-debugging-agent-processes.md](run-debugging-agent-processes.md#verify-model-routing-before-reading-logs)
covers reading `activity_provenance` for what actually dispatched.

**Runs are asynchronous by default.** `orbit run job` submits to a detached
worker and returns as soon as the run is durable — it prints the run id and the
inspection commands, and does *not* claim the eventual outcome. Add `--wait` to
block on it.

An MCP session that opts into the `pipelines` toolset (`--toolset pipelines` or
`machine.mcp.toolsets` in the operator profile; included in the `resident` role)
submits the same catalog job with `orbit_pipeline_invoke` (or use
`orbit run job <job_id>` on the CLI):
required `job_name`, and `input` unless `default_input` is true (that form
also requires `workspace`, forbids `input` and `priority`, and is
operator-only). It returns a run ID immediately.

Public submissions require
an operator; a managed run may invoke only a host-authorized child pipeline
admission. There is no MCP wait. `orbit.pipeline.wait` is registered and
CLI-only (`orbit tool run orbit.pipeline.wait` with `run_ids`); an operator
session polls `orbit_run` action `show` from MCP (operator authority; denied
for agent sessions), while a session with a shell on the owning host polls
`orbit run show <run_id> --json`.

Equivalent catalog commands exist as `orbit job list|show|run|replay|resume`.
`replay` re-runs from step 0 against the current definition; `resume` creates a new linked run using persisted checkpoints where
resumable, preserving the original attempt. Read both run records; a resume is
not a rewrite of failed history.
