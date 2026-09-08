---
title: CLI Commands
description: "A map of the Orbit CLI command surface, grouped the way orbit --help groups it."
sidebar:
  order: 2
---

Every command supports `--help`, which is the authority for its exact options.
This page is the map.

## Global options

| Option | Effect |
|---|---|
| `--root <ROOT>` | Override the Orbit root directory. Highest precedence. |
| `--workspace <SELECTOR>` | Select a workspace by registered name, logical ID (`ws_*`), or absolute checkout path. Distinct from `--root`. |
| `--format <MODE>` | `auto` (default — table on a terminal, plain text when piped), `table`, `json`, or `ndjson`. |

Global options go **before** the subcommand: `orbit --workspace ws_x task list`.

## Environment

| Command | Purpose |
|---|---|
| `orbit init` | Initialize the global Orbit root, host identity, task-id prefix, and default skills. |
| `orbit workspace init` | Register the current repository as a workspace. `--mcp`, `--ship-mode`, `--base-branch`, `--inject-agent-rules`. |
| `orbit workspace list` \| `show` \| `sync` \| `role` | Inspect registered workspaces, converge managed artifacts, and validate this checkout's role. |
| `orbit workspace publication bind` \| `show` \| `rebind` \| `remove` | Manage the owner-local binding to a dedicated task-publication repository. See [Publish and Restore Tasks](../../how-to/task-publication/). |
| `orbit workspace remove` \| `teardown` | Deregister a workspace, or remove Orbit artifacts from it. |
| `orbit host show` \| `rename` | Inspect or rename this machine's local host identity. |
| `orbit config show` \| `get` \| `set` \| `keys` \| `path` | Read and write configuration. See [Configuration](../config/). |
| `orbit semantic install` \| `uninstall` \| `stats` \| `index` | Manage the local embedding companion. CLI task mutations do not auto-index; run `orbit semantic index` to refresh. |
| `orbit migrate` | Inspect pending `.orbit` layout and store migrations; `--confirm` applies them. |
| `orbit update` | Install a published release and converge to it. `--check`, `--version`, `--allow-downgrade`. |

## Operate

### Workflows

| Command | Purpose |
|---|---|
| `orbit run ship [task_id ...]` | Ship selected tasks, or the ready backlog, through the gated pipeline. Returns a run ID immediately. |
| `orbit run ship --mode local` | Deliver in place instead of opening a pull request. |
| `orbit run auto [--for <duration>]` | Drain the backlog for a window. `--concurrency`, `--allow-crew`. |
| `orbit run auto --stop` | Stop new admissions for this workspace's active auto coordinator. Already admitted workers keep running — this is not cancellation. |
| `orbit run ship --complete` / `orbit run auto --complete` | Additionally authorize that run to finish delivery and move the tasks it ships from `review` to `done`. Off by default. |
| `orbit run readiness [task_id ...]` | Read-only explanation of why backlog tasks can or cannot start. `--concurrency`, `--allow-crew`, `--limit`. |
| `orbit run triage [task_id ...]` | Re-backlog tasks blocked by environmental run failures. |
| `orbit run ship-sweep` | Dispatch ship runs in every workspace with `[workflow] auto_ship = true`. `--dry-run`. |
| `orbit run job <job_id>` | Run any job by ID or YAML path. `--input key=value`, `--wait`. |

See [Delivery Workflows](../../getting-started/workflows/).

### Tasks

| Command | Purpose |
|---|---|
| `orbit task add` | Create a task. `--title` and `--complexity` are required. |
| `orbit task update <id>` | Update fields. `--approve` takes the next approval step (`proposed → backlog`, `review → done`). |
| `orbit task list` | List tasks. Status-neutral by default; filter with `--status`, `--tag`, `--path`, `--ready`, `--ref`. |
| `orbit task show <id>` | Show one task, found by ID across registered workspaces. `--fields` projects specific fields. |
| `orbit task archive <id>` | Archive a task. A bare `--status archived` update is refused. |
| `orbit task artifact` | Manage task artifact files. |
| `orbit task lint` | Flag stale paths and vague acceptance criteria. |
| `orbit task flow` | Filed-vs-closed rates over time — is the backlog draining? |
| `orbit task locks list` \| `contention` \| `reserve` \| `release` | Inspect and manage the file locks that gate parallel dispatch. |
| `orbit task export` \| `import` \| `reindex` | Portable `tar.zst` task bundles, and index rebuild. |
| `orbit task publication publish` \| `status` \| `inspect` \| `restore` | Publish, verify, read, or restore a task snapshot. Nothing publishes automatically. |

### Knowledge

| Command | Purpose |
|---|---|
| `orbit docs list` \| `show` \| `add` \| `index` \| `migrate` | Manage the indexed Markdown docs corpus. |
| `orbit friction add` \| `list` \| `show` \| `stats` \| `tags` \| `update` \| `resolve` | Report and triage friction records. |

### Maintenance

| Command | Purpose |
|---|---|
| `orbit run cancel <run_id> --confirm` | Cancel a pending or running job run and release its task reservations. |
| `orbit run concurrency <run_id> --set N` | Retune how many tasks a live drain keeps in flight. `--reason`, `--if-revision`. |
| `orbit gc worktrees` | Report job-run worktrees whose task has settled; `--confirm` reaps them. |

## Observe

| Command | Purpose |
|---|---|
| `orbit search <query>` | Search tasks, docs, and frictions. `--hybrid` adds vector ranking; `--workspaces <SELECTOR>` (repeatable) federates across registered checkouts and is distinct from the global `--workspace` routing selector; `orbit search similar <id>` finds task neighbors; `orbit search path <path>` does applicability lookup. |
| `orbit audit list` \| `show` \| `prune` \| `export` \| `stats` | Query the audit event log. |
| `orbit run history` | Recent job runs. `-j <job_id>` filters to one job. |
| `orbit run show [run_id]` | State and step summary for a run; defaults to the most recent. `-s <step_id>`. |
| `orbit run logs [run_id]` | Raw stdout/stderr captured for a run. |
| `orbit run events [run_id]` | Audit events recorded for a run. |
| `orbit run trace [run_id]` | Parent/child run tree. |
| `orbit log tail` | Tail the unified Orbit log feed. |
| `orbit doctor` | Diagnose workspace health: config, database, disk, indexes, locks, runs. The `--fix-*` flags are opt-in repairs. |

## Definitions

| Command | Purpose |
|---|---|
| `orbit activity` | View activity definitions. See [Activities and Jobs](../../concepts/activities-jobs/). |
| `orbit job` | View job definitions. |
| `orbit tool` | View the tool registry. |
| `orbit policy` | View filesystem policies. See [Policy Format](../policy-format/) and [Scoping](../scoping/). |
| `orbit executor` | View executors. |

## Scheduler

| Command | Purpose |
|---|---|
| `orbit sweep` | The scheduler pass: fire due routines on this host. `--dry-run`, `--verbose`, `--json`. |
| `orbit routine list` \| `show` \| `pause` \| `resume` | Inspect routines and pause them host-locally. |
| `orbit routine clock status` \| `pause` \| `enable` \| `set` | Control the host OS sweep clock. |
| `orbit routine init [--install-clock]` | Read host identity and optionally install the OS clock unit. |
| `orbit auto-task add` \| `list` \| `show` \| `update` \| `toggle` \| `mint` | Define recurring auto-task templates and mint from them. |

See [Schedule Recurring Work](../../how-to/recurring-work/).

## Services

| Command | Purpose |
|---|---|
| `orbit mcp init` / `orbit mcp remove` | Register or unregister MCP client integration. Clients: `claude`, `codex`, `gemini`, `grok`, `cursor`, `vscode`, `windsurf`. `--federated` manages the mux entry separately. |
| `orbit mcp serve` | Serve the MCP tool surface over stdio. `--operator` serves operator authority; `--orchestrator <crew>` sets the orchestrator attribution recorded on tasks the session creates, and grants nothing. |
| `orbit mcp listen [ADDR]` | Serve the same surface on a TCP socket. Binds `127.0.0.1:7879` unless `--allow-non-loopback` is passed. |
| `orbit mcp callers` | Inspect and seed which callers this machine serves, and as what. |
| `orbit web serve` | Serve the Orbit dashboard. Serves the registry under the resolved root, so `orbit --root <ROOT> web serve` exposes only `<ROOT>`'s workspaces. `--workspace <SELECTOR>` preselects one of them. |
| `orbit web connect` | Open a remote workspace's dashboard over an SSH tunnel. `--workspace <SELECTOR>` preselects the remote workspace; it takes no `--root`. |

See [Use the Dashboard](../../how-to/dashboard/) for connection, workspace scope, Operations controls, and authorization. See [Set Up MCP](../../how-to/mcp-integration/) for the agent tool surface.
