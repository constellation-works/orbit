# Orbit

**Your agent files the work. Orbit ships it. You review the pull request.**

<p align="center">
  <a href="https://github.com/constellation-works/orbit/releases"><img src="https://img.shields.io/github/v/release/constellation-works/orbit" alt="Release" /></a>
  <a href="https://www.npmjs.com/package/@orbit-tools/cli"><img src="https://img.shields.io/npm/v/@orbit-tools/cli" alt="npm" /></a>
  <a href="LICENSE.md"><img src="https://img.shields.io/badge/license-MIT-blue" alt="License: MIT" /></a>
  <a href="https://orbit-cli.com"><img src="https://img.shields.io/badge/docs-orbit--cli.com-informational" alt="Docs" /></a>
</p>

<p align="center">
  <img src="docs/assets/orbit-demo.gif" alt="Animated walkthrough of an illustrative session: an agent files task ORB-1042 with orbit.task.add, you approve it, orbit.workflow.ship runs it in an isolated worktree with file locks through plan, execute, and review, a pull request opens for you to merge, a parallel drain queues overlapping work behind file locks, and orbit task show traces the commit back to its task." width="880" />
</p>

Orbit is a local-first runtime for coding agents. You keep using Claude Code, Codex, Cursor, Copilot, or any of the other supported CLIs. Orbit gives them a durable task queue, isolated sandboxed worktrees, file-level locks for parallel runs, a gated pipeline that ends in a pull request, and an audit log of every step.

**Why:** agents are fast enough that planning, review, and traceability are the first things to go. Six months later nobody can say why a line was written. Orbit makes those disciplines cheap and keeps you out of the clerical work. The agent files the task, Orbit runs it, and every commit carries a task ID you can trace back to the prompt, the plan, and the review.

- **Single binary, no cloud.** State lives in `~/.orbit` and `.orbit/`. Nothing phones home.
- **Bring your own agents.** Orbit drives the provider CLIs you already have authenticated, and never asks for API keys.
- **MIT licensed.** No paid tier, no hosted offering.

---

## How it works

```text
$ orbit init                                 # one-time, per machine
$ cd my-repo && orbit workspace init --mcp   # per repo; wires Orbit into your agent CLIs

You:    The fsProfile lookup is undocumented. Get that fixed.
Agent:  orbit.task.add       → ORB-1042 · proposed
        Filed with acceptance criteria. Approve it and ship?
You:    Yes.
Agent:  orbit.task.update    → ORB-1042 · backlog
        orbit.workflow.ship  → worktree isolated · file scope locked · plan → execute → review
        Pull request opened. ORB-1042 is in review. The diff and the merge are yours.

$ orbit task update ORB-1042 --approve       # after you merge: review → done
```

<sub>Illustrative session. The tool names are real, and the IDs are placeholders.</sub>

1. **Nothing starts without you.** New tasks land in `proposed`, and only your approval moves them to `backlog`.
2. **Every run ends at a pull request.** Orbit never merges on its own. Merging the PR and completing the task are separate decisions, unless you explicitly pass `--complete`.
3. **Everything is on the record.** `orbit task show ORB-1042` reconstructs the prompt, plan, execution trace, and review thread, even months later.

---

## Quick start

**You need:** at least one authenticated agent CLI, plus `gh` authenticated if you want pull requests. On Linux, also follow the [sandbox setup](docs/runbooks/linux-sandbox.md).

```bash
# 1. Install (pick one)
npm install -g @orbit-tools/cli                 # Node 18+
brew install constellation-works/tap/orbit
curl -sSf https://raw.githubusercontent.com/constellation-works/orbit/main/install.sh | sh

# 2. Initialize the machine: asks for a machine name and a task-ID prefix, and detects your agent CLIs
orbit init

# 3. Register a repo and connect your agents over MCP
cd <repo> && orbit workspace init --mcp         # add --ship-mode local to skip PRs

# 4. Check everything is healthy
orbit doctor
```

Now open your agent in the repo and ask for something. It files the task, asks for approval, ships it, and reports the PR.

| To… | Run |
|---|---|
| Inspect a task or run | `orbit task show <ID>` · `orbit run show <RUN_ID>` |
| Open the dashboard | `orbit web serve` (remote: `orbit web connect <host>`) |
| Pick the default crew (provider and model) | `orbit config set workflow.default_crew <crew>` |
| Upgrade | `orbit update` |

<details>
<summary><strong>The same loop without an agent</strong></summary>

Every MCP tool has a CLI twin.

```bash
TASK_ID=$(orbit task add --title "..." --description "..." \
  --acceptance-criteria "..." --complexity medium --workspace .)
orbit task update "$TASK_ID" --approve   # proposed → backlog
orbit run ship "$TASK_ID"                # async; prints a run ID
orbit run show <RUN_ID>                  # progress and outcome
orbit task update "$TASK_ID" --approve   # after merging the PR: review → done
```

</details>

---

## Features

### Plan and govern
- **Durable tasks with intent.** Tasks carry acceptance criteria, a file scope, dependencies, and typed relations. They move through `proposed → backlog → in-progress → review → done`, and that state survives sessions and branches.
- **Structured audit log.** Every tool call, provider exchange, and state transition is recorded as an append-only, queryable event tagged with the agent and model that produced it (`orbit audit`).
- **Friction ledger.** When the work was harder than it should have been, whether from a confusing error, a missing flag, or an undocumented convention, the agent records it with `orbit friction add` instead of quietly working around it. A task that resolves the friction closes it on completion.
- **Local search.** `orbit search` runs fast lexical search (SQLite FTS5) over tasks and frictions. It needs no model download.

### Execute safely in parallel
- **Isolated, sandboxed runs.** Each run gets its own git worktree and runs its agent CLI under `sandbox-exec` on macOS or Bubblewrap on Linux. On Linux, worker runs are also memory-bounded in a cgroup.
- **Conflict-aware scheduling.** Runs reserve their task's files as locks before starting, so overlapping work waits in line instead of producing merge conflicts later.
- **Gated pipeline.** Each run goes plan → execute → review, with repair budgets and failure recovery. Dependencies gate admission, so you declare the order once and the queue enforces it.
- **Nine agent CLIs, routed by crews.** Claude Code, Codex, Cursor, Copilot, Grok, Gemini, Antigravity, OpenCode, and Pi. Crews pin a provider, model, and effort level. Complexity-tiered, weighted crew pools spread the work across them.

### Run unattended
- **Bounded drains.** `orbit run auto --for 4h --concurrency 8` ships the backlog until the time window closes. `orbit run readiness` previews what would run without starting anything.
- **Opt-in completion.** `--complete` merges PRs once GitHub allows it and closes tasks after the merge is verified. Nothing else turns this on.
- **Continuous review.** The shipped `code-review`, `qa-sweep`, and `security-review` auto-tasks read everything that landed since their last run, verify findings against live code, and file confirmed ones as tasks with `file:line` evidence.
- **Recurring work as data.** Scheduled task templates live in `.orbit/auto_tasks/*.yaml`, and one machine scheduler (`orbit clock`) runs routines and auto-tasks.
- **Multi-machine.** A distributed drain spreads a backlog across machines through durable claims. A federated MCP server puts local and SSH-remote workspaces under one namespace.

### Observe and extend
- **Dashboard** (`orbit web serve`). Shows the task backlog, live audit feed, per-agent scoreboard, jobs, frictions, and effective config, with inline editing.
- **Plugins** (`orbit plugin`). A plugin can add its own tools, jobs, routines, auto-tasks, skills, and CLI commands. Plugins run sandboxed under explicit permission grants, and `orbit plugin scaffold` generates a starter.
- **Agent skills.** Three skills ship with Orbit: `orbit` (everyday task work), `orbit-orchestrate` (backlog and dispatch), and `orbit-setup` (machine and repo configuration). `orbit init` links them into your agents.

You can adopt these one at a time. The task layer and audit log work from day one. Parallel drains, auto-tasks, and plugins switch on when you want them.

---

## Agent plugins

To give a single agent Orbit's MCP tools and skills without installing the CLI on `PATH`, add the plugin. It launches the pinned npm CLI. The dashboard and cross-agent workspace setup still need the CLI install.

```bash
# Claude Code
/plugin marketplace add constellation-works/orbit
/plugin install orbit

# Codex CLI
codex plugin marketplace add constellation-works/orbit --ref main
codex plugin add orbit@orbit

# Cursor (local plugin from a checkout)
mkdir -p ~/.cursor/plugins/local && ln -sfn "$(pwd)/plugin" ~/.cursor/plugins/local/orbit
```

Want to be walked through setup? Ask your agent to *"set up Orbit for this repo"*, and the bundled `orbit-setup` skill takes it from there.

---

## MCP and authority

`orbit workspace init --mcp` registers `orbit mcp serve --operator` with your agent CLIs. That operator session is the only one that can dispatch workflows, resume runs, or run commands. Agents launched by Orbit get an agent-only surface. Authority is enforced when a tool is called, not by hiding tools, so every session sees the same `tools/list`. Use `orbit mcp init` alone for an agent-only registration, or `orbit mcp init --federated` to put several machines under one namespace ([details](docs/design/federated-mcp/)).

## Where state lives

| Path | Holds |
|---|---|
| `~/.orbit/` | Machine state: task bundles, `orbit.db` (audit, runs, routines, frictions), workspace registry, shipped resources, skills, `config.toml` |
| `<repo>/.orbit/` | Workspace state: identity, local config overrides, auto-tasks, routines, worktrees, and logs. Gitignored, and safe to delete for a clean slate. |

Backups, stuck runs, database recovery, and upgrades are covered in the [runbooks](docs/INDEX.md#runbooks).

---

## Learn more

- **[orbit-cli.com](https://orbit-cli.com):** guides, concepts, and the full CLI and config reference
- **[docs/CONFIG.md](docs/CONFIG.md):** crews, pools, base branch, sandbox
- **[docs/POSITIONING.md](docs/POSITIONING.md):** what Orbit is for, and what it deliberately isn't
- **[ARCHITECTURE.md](ARCHITECTURE.md)** and **[design docs](docs/INDEX.md#designs)**
- **[CHANGELOG.md](CHANGELOG.md):** Orbit is pre-1.0, and breaking changes ship in minor releases

## Contributing

Pull requests are welcome, from typo fixes to new executors. Small fixes can go straight to a PR, and bigger changes start with an issue. See [CONTRIBUTING.md](CONTRIBUTING.md) to get set up.

## License

[MIT](LICENSE.md)
