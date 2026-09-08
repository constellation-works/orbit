# Orbit — The engineering framework for your AI coding agents

<p align="center">
  <img src="docs/assets/orbit-dashboard-hero.gif" alt="Orbit dashboard: task backlog, agent execution, and live audit log" width="600" />
</p>

<p align="center">
  <em>The Orbit dashboard (<code>orbit web serve</code>) — task backlog, live audit log, per-agent scoreboard.</em>
</p>

**Orbit takes review off the critical path, so throughput and rigor stop trading against each other. A durable task for every change, structured audit of every tool call and provider exchange, conflict-aware parallel dispatch, and continuous review sweeps that file what they find straight back into the backlog — local-first.**

You drive Claude Code, Codex, Grok Build, or Gemini CLI against real code, often in parallel. Agents make it easy to skip the disciplines that keep code maintainable, and six months later nobody can reconstruct why an agent wrote a given line. Orbit makes those disciplines cheap: tasks before edits, every tool call in a structured audit log, parallel runs sandboxed into worktrees with file-level locks, and your own design docs retrievable by the agents doing the work.

Conventional practice buys safety with blocking gates, which caps how many agents you can run at once. Orbit merges on a quick direction check, then scheduled `code-review`, `qa-sweep`, and `security-review` passes read the merged window and file every confirmed finding back into the backlog as an ordinary task. Every commit carries its task ID, so the history stays reconstructable.

---

## Features

- **Durable, intent-tracked task layer.** Lifecycle (`proposed → backlog → in-progress → review → done`) survives sessions and branches; every commit carries the `task_id`, so `orbit task show` reconstructs prompt, plan, execution trace, and review threads months later. → [docs/design/task-artifacts/](docs/design/task-artifacts/)
- **Structured audit log.** Every tool call, provider exchange, and task transition is a queryable, append-only event with agent identity attached. → [docs/design/auditability/](docs/design/auditability/)
- **Conflict-aware parallel execution.** `orbit run ship` gives each run its own git worktree and reserves task `context_files` as locks before fanning out, rejecting overlapping reservations up front instead of producing merge conflicts later. → [docs/design/activity-job/](docs/design/activity-job/)
- **Continuous review, not blocking review.** Shipped `code-review`, `qa-sweep`, and `security-review` auto-tasks read the window since their last cursor, verify findings against live code, and file confirmed ones as tasks with `file:line` evidence. A clean window is a successful no-op. → [docs/design/auto-tasks/](docs/design/auto-tasks/)
- **Sandboxed-by-default execution.** Dispatched agent CLIs run under `sandbox-exec` on macOS and Bubblewrap on Linux; the Linux boundary enforces writes only, leaving host reads and network open. Unsupported platforms keep the in-process filesystem guards. → [docs/design/policy-sandbox/](docs/design/policy-sandbox/)
- **A searchable docs corpus — your conventions, not Orbit's.** Register the markdown you already write with `orbit docs add`; agents retrieve it by concept via `orbit search --kind doc`, with `--hybrid` adding embedding recall. → [docs/design/orbit-docs/](docs/design/orbit-docs/)
- **A friction ledger for what made the work harder than it should have been.** A confusing error, a missing flag, an undocumented convention — Orbit's own tooling being one case among many — the agent files it (`orbit friction add`) instead of silently working around it. A task carrying a `resolves` relation closes its friction on reaching `done`.
- **Dependency-ordered execution.** Tasks carry `dependencies` and typed `relations`; the pipeline gates admission on them, so declare the order once and let the queue enforce it.
- **Recurring work as data.** `orbit auto-task` defines `.orbit/auto_tasks/*.yaml` templates with a cron or interval schedule and a dedupe policy; a seeded scheduler mints tasks from the due ones, and `orbit auto-task mint <name>` mints one on demand.

Everything is incremental: the task layer and audit log work on day one; the docs corpus, friction ledger, auto-tasks, and parallel dispatch switch on as you adopt them.

---

## Quick Start

**Prerequisites:** at least one supported agent CLI (Claude Code, Codex, Cursor, or Gemini CLI), authenticated. `orbit run ship` in its default `--mode pr` needs `gh` authenticated; otherwise use `--mode local`. On Linux, complete the [Linux sandbox runbook](docs/runbooks/linux-sandbox.md) after `orbit init`.

```bash
curl -sSf https://raw.githubusercontent.com/constellation-works/orbit/main/install.sh | sh
# or: brew install constellation-works/tap/orbit

orbit init                                 # global state (~/.orbit)
cd <repo> && orbit workspace init --mcp    # workspace state + operator-authorized MCP integration

TASK_ID=$(orbit task add --title "..." --description "..." \
  --acceptance-criteria "..." --complexity medium --workspace .)
orbit task update "$TASK_ID" --status backlog   # approve into the backlog

orbit run ship                             # conflict-aware parallel flush of the backlog to PRs
orbit run show <RUN_ID>                    # step-by-step progress
orbit web serve                            # dashboard over every registered workspace
orbit web connect my-server                # ...or a remote workspace over an SSH tunnel
```

Or ask your agent: "create an orbit task to refactor the authentication logic in …". Full command reference: `orbit --help` and [orbit-cli.com](https://orbit-cli.com). Crews (which provider-model runs a task) and the base branch: [docs/CONFIG.md](docs/CONFIG.md).

### Agent plugins

Use a plugin to attach Orbit's MCP tools and `orbit` skill to one agent without the CLI on `$PATH`; use the CLI install for the dashboard and cross-agent workspace setup. All three load `plugin/skills/orbit` and launch the npm CLI pinned to that release (`npx -y @orbit-tools/cli@<version> mcp serve`). Cursor's public marketplace listing is a separate human-reviewed catalog update; a git tag does not publish it.

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

### Clone and customize

Cloning gives you a framework to mold to your team's conventions; everything under `.orbit/` carries over from a binary install. Paste the prompt below into your agent from inside the repo where you want Orbit.

<details>
<summary><strong>Agent setup prompt</strong> (click to expand)</summary>

> You are helping me set up Orbit, a local governance and audit layer for coding agents, inside this repository so I keep my existing agents while gaining durable tasks, structured audit, a searchable docs corpus, and safe parallel execution.
>
> 1. Ask me where to clone the Orbit repository (suggest `~/code/orbit`).
> 2. Verify the Rust toolchain: Orbit's MSRV is `rust-version = "1.89"`. If cargo is missing or older, **stop and ask me before installing anything** (`rustup` modifies the shell profile).
> 3. Clone `https://github.com/constellation-works/orbit` into that location and run `make install` (copies `orbit` to `$INSTALL_BIN_DIR`, default `~/.cargo/bin`). Confirm the install path with me first. Verify with `orbit --version`.
> 4. Run `orbit init` for global state at `~/.orbit`. On Linux, follow `docs/runbooks/linux-sandbox.md` and require its probe to pass before dispatching agents.
> 5. From *this* repository, run `orbit workspace init --mcp`. It creates `.orbit/` and registers an **operator-authorized** MCP server with installed agent CLIs. Tell me first if you'd rather it stay agent-only (`orbit mcp init`).
> 6. Ask me whether to enable semantic search (optional): `orbit semantic install` downloads an embedder companion plus the default model under `~/.orbit/embed/` (macOS arm64 or Linux x86_64/aarch64 with glibc >= 2.38). If I accept and tasks already exist, run `orbit semantic index`.
> 7. Read `README.md`, `docs/POSITIONING.md`, `CLAUDE.md`, `ARCHITECTURE.md`, `docs/design/CONVENTIONS.md`, and `docs/CONFIG.md`.
> 8. Run `orbit task list` and `orbit semantic stats` and show me the output.
> 9. Ask me what my first real task should be and create it with the `orbit` skill.
>
> Rules: never run destructive commands, install rustup, install outside `~/.cargo/bin`, or modify a shell profile without confirmation. If anything is unclear or fails, stop and ask. Do not simplify or hide Orbit's conventions; I am choosing this because I want the discipline.

</details>

---

## Search

`orbit search` queries tasks, docs, and frictions (`--kind task|doc|friction|all`), lexical by default. `--hybrid` adds embedding ranking over tasks and indexed docs, and `orbit search similar <task-id>` finds cosine-neighbor tasks. The embedder is a separate companion subprocess, so semantic search costs nothing when unused.

```bash
orbit semantic install    # one-time: companion + default model (bge-small)
orbit semantic index      # backfill tasks; CLI mutations do not auto-index — re-run after edits
orbit docs index          # backfill docs for --kind doc --hybrid
orbit search "race in the scheduler when locks overlap" --hybrid --kind task
```

The companion is released for macOS arm64 and Linux x86_64/aarch64 (glibc >= 2.38); Intel macOS runs the CLI without semantic search. Frictions are never embedded.

---

## Agent Skills

`orbit workspace init` seeds the `orbit` skill under `~/.orbit/skills/` and symlinks it into `~/.claude/skills/` and `~/.agents/skills/`, so Claude Code, Codex, and Gemini CLI discover it with no per-agent configuration. Its `SKILL.md` routes to on-demand references for working through Orbit (tasks, search, friction, `orbit run`, debugging) and setting it up (crews, routines, auto-tasks, GC, remote access). `orbit skill doctor` reports drift from the shipped copy; edit the seeded `SKILL.md` to customize it.

---

## MCP Surface

`orbit workspace init --mcp` registers `orbit mcp serve --operator --workspace <ws_id>` with the local agent CLIs. That server holds **operator authority**: dispatching a workflow (`orbit.workflow.ship`), observing or resuming a run, and `orbit.command.exec` are authorized through it. Bare `orbit mcp serve`, and every agent-launched session, stays agent-only and is refused those tools. Authorization is enforced at call time, not by hiding names: `tools/list` shows every session the same surface and is the authoritative reference.

`--workspace` binds the server to one workspace so scoped tools route without a selector on every call. An explicit `workspace` argument overrides it; server cwd is never a fallback, so do not pass `--root` to `orbit mcp serve`. An unbound client calls `orbit_workspace_list` first and reuses a returned `ws_*` ID.

`orbit workspace sync` converges Orbit-managed shipped definitions in a registered workspace (preserving operator edits; `--check` is read-only), and `orbit doctor` diagnoses health with narrow repairs. Neither pulls the repo, upgrades the binary, or applies migrations.

**Federated MCP** (opt-in) presents one namespace over local workspaces plus SSH remotes declared in `~/.orbit/mcp-destinations.toml`. Register it with `orbit mcp init --federated --client <client>`, then copy the owner row's host-qualified `selector` from `orbit_workspace_list` onto workspace-scoped calls. The mux does no placement or failover; task reads are owner-only. → [docs/design/federated-mcp/](docs/design/federated-mcp/)

---

## Workspace Layout

```
.orbit/                          # workspace-local (safe to delete → clean slate)
├── config.toml                  # workspace-local runtime overrides
├── config.yaml                  # workspace_id only
├── auto_tasks/                  # recurring task definitions
├── routines/                    # routine definitions
├── tasks/                       # projection of ~/.orbit/tasks/workspaces/<id>/
├── resources/                   # activities, jobs, executors, policies (customizable)
└── state/                       # worktrees, logs, job-runs, audit spool, scoreboard, semantic.db

~/.orbit/                        # global (machine-level, survives repo moves)
├── tasks/                       # ORB-XXXXX index + canonical task bundles per workspace
├── orbit.db                     # host-global store: audit events, job runs, routines, frictions
├── workspaces.json              # workspace registry for this machine
├── resources/                   # shipped activities, jobs, executors, policies
├── skills/                      # SKILL.md files
├── embed/                       # semantic companion + models
├── config.toml                  # global settings
└── host.toml                    # machine identity: machine_id, host_id, task_prefix
```

`orbit workspace init` seeds a `.gitignore` that keeps runtime state out of the repo while `auto_tasks/`, `resources/`, `routines/`, and `config.toml` stay in. Day-2 operations (backup, stuck runs, DB recovery, health checks, upgrades) are in the [runbooks](docs/INDEX.md#runbooks).

---

## Status

Pre-1.0 and under active development. Breaking changes ride a minor bump; see [CHANGELOG.md](CHANGELOG.md) and [RELEASING.md](RELEASING.md). 0.11.0 removed the native `orbit adr` and `orbit learning` stores and the parsed code-graph subsystem; durable know-how lives in your own markdown registered into the docs corpus.

## Contributing

Contributions especially welcome on locking, worktree/session management, execution primitives, reconciliation, audit coverage, and tool-calling interfaces. Read [docs/INDEX.md](docs/INDEX.md#designs), [docs/design/CONVENTIONS.md](docs/design/CONVENTIONS.md), and [CLAUDE.md](CLAUDE.md) first.

## License

MIT
