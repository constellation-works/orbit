---
title: Concepts
description: "Orbit's five layers: scheduling, tasks, activities and jobs, agents, and policies."
sidebar:
  order: 1
---

Orbit has five layers. Each answers one question: when work runs, what the
work is, how it runs, who runs it, and what bounds it. Each page below covers
one layer: what it guarantees and what it refuses. For commands, see the
[how-to guides](../how-to/).

<nav class="orbit-stack" aria-label="Orbit layers, top to bottom">
  <a class="orbit-stack-layer" href="./scheduling/">
    <span class="orbit-stack-q">When</span>
    <span class="orbit-stack-name">Routines and auto-tasks</span>
    <span class="orbit-stack-desc">Fire jobs on a schedule; file recurring chores as tasks.</span>
  </a>
  <a class="orbit-stack-layer" href="./tasks/">
    <span class="orbit-stack-q">What</span>
    <span class="orbit-stack-name">Tasks</span>
    <span class="orbit-stack-desc">The durable, reviewable unit of work.</span>
  </a>
  <a class="orbit-stack-layer" href="./activities-jobs/">
    <span class="orbit-stack-q">How</span>
    <span class="orbit-stack-name">Activities and jobs</span>
    <span class="orbit-stack-desc">Reusable execution units and the jobs that chain them.</span>
  </a>
  <a class="orbit-stack-layer" href="./agents/">
    <span class="orbit-stack-q">Who</span>
    <span class="orbit-stack-name">Agents</span>
    <span class="orbit-stack-desc">Provider CLIs and the crews tasks run under.</span>
  </a>
  <a class="orbit-stack-layer orbit-stack-layer-guard" href="./policies/">
    <span class="orbit-stack-q">Bounds</span>
    <span class="orbit-stack-name">Policies</span>
    <span class="orbit-stack-desc">Filesystem rules that bound every run.</span>
  </a>
</nav>

## Pages

<div class="orbit-card-grid">
  <a class="orbit-card" href="./tasks/" data-tag="01">
    <h3>Tasks</h3>
    <p>Lifecycle, statuses, the two approval gates, and the transition rules.</p>
  </a>
  <a class="orbit-card" href="./activities-jobs/" data-tag="02">
    <h3>Activities and jobs</h3>
    <p>Reusable execution units, the jobs that chain them, and how a task's required tools reach a run.</p>
  </a>
  <a class="orbit-card" href="./scheduling/" data-tag="03">
    <h3>Routines and auto-tasks</h3>
    <p>The host scheduler clock, routines that fire jobs, auto-tasks that file chores, and the limits on unattended work.</p>
  </a>
  <a class="orbit-card" href="./policies/" data-tag="04">
    <h3>Policies</h3>
    <p>Filesystem profiles and the deny rules that bound what a run can read and write.</p>
  </a>
  <a class="orbit-card" href="./agents/" data-tag="05">
    <h3>Agents</h3>
    <p>Provider CLIs, executors, tool policy, and crews: the named provider and model a task runs under.</p>
  </a>
</div>

## Capabilities

### Plan and govern

- **Durable tasks with intent.** Tasks carry acceptance criteria, a file scope, dependencies, and typed relations. They move through `proposed → backlog → in-progress → review → done`, and that state survives sessions and branches.
- **Structured audit log.** Every tool call, provider exchange, and state transition is recorded as an append-only, queryable event tagged with the agent and model that produced it (`orbit audit`).
- **Friction ledger.** When the work was harder than it should have been, whether from a confusing error, a missing flag, or an undocumented convention, the agent records it with `orbit friction add` instead of quietly working around it. A task that resolves the friction closes it on completion.
- **Local search.** `orbit search` runs fast lexical search (SQLite FTS5) over tasks and frictions. It needs no model download.

### Execute safely in parallel

- **Isolated, sandboxed runs.** Each run gets its own git worktree and runs its agent CLI under `sandbox-exec` on macOS or Bubblewrap on Linux. On Linux, workers use memory-bounded cgroups when a systemd user manager is available; see [worker containment](../reference/config/#settable-keys).
- **Conflict-aware scheduling.** Runs reserve their task's files as locks before starting, so overlapping work waits in line instead of producing merge conflicts later.
- **Gated pipeline.** Each run goes plan → execute → review, with repair budgets and failure recovery. Dependencies gate admission, so you declare the order once and the queue enforces it.
- **Nine agent CLIs, routed by crews.** Claude Code, Codex, Cursor, Copilot, Grok, Gemini, Antigravity, OpenCode, and Pi. Crews pin a provider, model, and effort level. Complexity-tiered, weighted crew pools spread the work across them ([how routing works](../reference/config/#crews)).

### Run unattended

- **Bounded drains.** `orbit run auto --for 4h --concurrency 8` ships the backlog until the time window closes. `orbit run readiness` previews what would run without starting anything. Or ask your agent to run one: the `orbit-orchestrate` skill prepares the backlog, starts the drain, and works through failed runs.
- **Opt-in completion.** `--complete` merges PRs once GitHub allows it and closes tasks after the merge is verified. For distributed handoffs, the owner can instead authorize completion with `workflow.distributed_completion = "done"`; see [delivery workflows](../getting-started/workflows/#completing-work-with---complete).
- **Continuous review.** The shipped `code-review`, `qa-sweep`, and `security-review` auto-tasks read everything that landed since their last run, verify findings against live code, and file confirmed ones as tasks with `file:line` evidence.
- **Recurring work as data.** Scheduled task templates live in `.orbit/auto_tasks/*.yaml`, and one machine scheduler (`orbit clock`) runs routines and auto-tasks.
- **Multi-machine.** A distributed drain spreads a backlog across machines through durable claims. A federated MCP server puts local and SSH-remote workspaces under one namespace.

### Observe and extend

- **Dashboard** (`orbit web serve`). Shows the task backlog, live audit feed, per-agent scoreboard, jobs, frictions, and effective config, with inline editing.
- **Plugins** (`orbit plugin`). A plugin can add its own tools, jobs, routines, auto-tasks, skills, and CLI commands. Plugins run sandboxed under explicit permission grants, and `orbit plugin scaffold` generates a starter.
- **Agent skills.** Three skills ship with Orbit: `orbit` (everyday task work), `orbit-orchestrate` (backlog and dispatch), and `orbit-setup` (machine and repo configuration). `orbit init` links them into your agents.

You can adopt these one at a time. The task layer and audit log work from day one. Parallel drains, auto-tasks, and plugins switch on when you want them.
