---
title: Concepts
description: "The parts of Orbit and what each one decides: when work runs, what it is, how it runs, who runs it, and what bounds it."
---

Orbit has five layers. Each answers one question about a piece of work. Read
the layer you are about to configure; the [guides](/how-to/) have the commands.

<nav class="orbit-stack" aria-label="Orbit layers, top to bottom">
  <a class="orbit-stack-layer" href="/concepts/scheduling/">
    <span class="orbit-stack-q">When</span>
    <span class="orbit-stack-name">Routines and auto-tasks</span>
    <span class="orbit-stack-desc">Fire jobs on a schedule; file recurring chores as tasks.</span>
  </a>
  <a class="orbit-stack-layer" href="/concepts/tasks/">
    <span class="orbit-stack-q">What</span>
    <span class="orbit-stack-name">Tasks</span>
    <span class="orbit-stack-desc">The durable, reviewable unit of work and its lifecycle.</span>
  </a>
  <a class="orbit-stack-layer" href="/concepts/activities-jobs/">
    <span class="orbit-stack-q">How</span>
    <span class="orbit-stack-name">Activities and jobs</span>
    <span class="orbit-stack-desc">Reusable execution steps and the jobs that chain them.</span>
  </a>
  <a class="orbit-stack-layer" href="/concepts/agents/">
    <span class="orbit-stack-q">Who</span>
    <span class="orbit-stack-name">Agents and crews</span>
    <span class="orbit-stack-desc">The provider CLIs Orbit drives and the crews tasks run under.</span>
  </a>
  <a class="orbit-stack-layer orbit-stack-layer-guard" href="/reference/policy-format/">
    <span class="orbit-stack-q">Bounds</span>
    <span class="orbit-stack-name">Policies</span>
    <span class="orbit-stack-desc">Filesystem rules that bound every run.</span>
  </a>
</nav>

## What Orbit does

You can adopt these one at a time. Tasks and the audit log work from the first
day; drains, auto-tasks and plugins switch on when you want them.

### Plan and govern

- **Tasks.** A task carries acceptance criteria, a file scope, dependencies and
  relations. It moves through `proposed → backlog → in-progress → review → done`,
  and that state survives sessions and branches. See [Tasks](/concepts/tasks/).
- **Audit log.** Tool calls, provider exchanges and state transitions are
  recorded as append-only events tagged with the agent and model that produced
  them. Query them with `orbit audit`.
- **Friction.** When work is harder than it should be, the agent records it with
  `orbit friction add` instead of working around it. See
  [Report and triage friction](/how-to/friction/).
- **Search.** `orbit search` runs lexical search over tasks and frictions. It
  needs no model download.

### Execute safely in parallel

- **Isolated runs.** Each run gets its own git worktree and runs its agent CLI
  under `sandbox-exec` on macOS or Bubblewrap on Linux.
- **File locks.** A run reserves its task's files before it starts, so
  overlapping work waits instead of producing merge conflicts.
- **A gated pipeline.** Each run plans, executes and reviews, with repair
  budgets and failure recovery. Dependencies gate admission: declare the order
  once and the queue enforces it.
- **Crews.** A crew names a provider, a model and an effort level. Pools pick a
  crew by task complexity. Orbit drives Claude Code, Codex, Copilot, Grok,
  Antigravity and Muse Code. See [Agents and crews](/concepts/agents/).

### Run unattended

- **Drains.** `orbit run auto --for 4h --concurrency 8` ships the backlog until
  the window closes. `orbit run readiness` shows what would start without
  starting anything. See [Run a delivery window](/how-to/continuous-delivery/).
- **Completion on your say-so.** `--complete` merges each pull request once
  GitHub allows it and closes the task. See
  [Delivery workflows](/getting-started/workflows/#completing-work-with---complete).
- **Recurring work.** Routines fire jobs on a schedule, and auto-tasks file
  recurring chores such as code review and QA sweeps as tasks. One scheduler per
  machine, `orbit clock`, drives both. See
  [Routines and auto-tasks](/concepts/scheduling/).
- **Scheduled starts.** A task's `not_before` holds it until its earliest start
  time. See [Scheduled tasks](/concepts/scheduling/#scheduled-tasks).
- **Several machines.** Other machines can pull work from the one that owns the
  backlog. See [Set up a distributed drain](/how-to/distributed-drain/).
- **Residents.** A standing orchestrator works under a mandate you set, and
  `orbit attention` lists what waits on you. See
  [Resident mandates](/reference/resident-mandates/).

### Observe and extend

- **Dashboard.** `orbit web serve` shows tasks, runs, the audit feed, analytics
  and configuration. See [Use the dashboard](/how-to/dashboard/).
- **Experiments.** `orbit experiment` splits real tasks between agent treatments
  and reports each arm. See [Run an experiment](/how-to/experiments/).
- **Plugins.** A plugin adds tools, jobs, routines, auto-tasks, skills and CLI
  commands under permissions you grant. See
  [Install and grant plugins](/how-to/plugins/).
- **Agent skills.** Three skills ship with Orbit and `orbit init` links them
  into your agents: `orbit` for task work, `orbit-orchestrate` for backlog and
  dispatch, and `orbit-setup` for machine and repository setup.
