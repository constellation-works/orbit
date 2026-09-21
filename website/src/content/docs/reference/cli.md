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
| `--workspace <SELECTOR>` | Select a workspace by registered name, logical ID (`ws_*`), or absolute checkout path. Distinct from `--root`. Only active workspaces may be bound. |
| `--format <MODE>` | `auto` (default — table on a terminal, plain text when piped), `table`, `json`, or `ndjson`. |

Every subcommand also accepts these options, so `orbit --workspace ws_x task list`
and `orbit task list --workspace ws_x` are equivalent. This site writes them
before the subcommand.

## Environment

| Command | Purpose |
|---|---|
| `orbit init` | Initialize the global Orbit root, this machine's `[machine]` identity, task-id prefix, and default skills. |
| `orbit workspace init` | Register the current repository as a workspace. `--name`, `--base-branch`, `--ship-mode pr\|local`, `--role owner\|replica` (`replica` requires `--owner <machine_id>`), `--task-id-start <N>`, `--mcp`, `--inject-agent-rules`; `--force` reconciles an already registered workspace. |
| `orbit workspace list` \| `show` \| `sync` \| `role` | Inspect registered workspaces, converge managed artifacts, and validate this checkout's role. |
| `orbit workspace publication bind` \| `show` \| `rebind` \| `remove` | Manage the owner-local binding to a dedicated task-publication repository. See [Publish and Restore Tasks](../../how-to/task-publication/). |
| `orbit workspace remove` \| `teardown` | Deregister a workspace, or remove Orbit artifacts from it. |
| `orbit config show` \| `get` \| `set` \| `keys` \| `path` | Read and write configuration, including this machine's identity under `machine.*`. Rename the machine with `orbit config set --global machine.name <value>`. See [Configuration](../config/). |
| `orbit migrate` | Inspect pending `.orbit` layout and store migrations; `--confirm` applies them. |
| `orbit update` | Install a published release and converge to it. `--check`, `--version`, `--allow-downgrade`. |

## Operate

### Workflows

| Command | Purpose |
|---|---|
| `orbit run ship [task_id ...]` | Ship selected tasks, or the ready backlog, through the gated pipeline. Returns a run ID immediately. |
| `orbit run ship --mode local` | Deliver in place instead of opening a pull request. |
| `orbit run auto [--for <duration>]` | Drain the backlog for a window. `--concurrency`, `--allow-crew`, `--low-complexity-crews` / `--medium-complexity-crews` / `--hard-complexity-crews` / `--xhard-complexity-crews` (crew pools for unassigned tasks, `crew` or `crew:weight`; override the `[workflow]` pools), `--claim-token` when another operator holds the workspace claim. |
| `orbit run auto --stop` | Stop new admissions for this workspace's active auto coordinator. Already admitted workers keep running — this is not cancellation. |
| `orbit run ship --complete` / `orbit run auto --complete` | Additionally authorize that run to finish delivery and move the tasks it ships from `review` to `done`. Off by default. |
| `orbit run readiness [task_id ...]` | Read-only explanation of why backlog tasks can or cannot start. `--concurrency`, `--allow-crew`, `--limit`. |
| `orbit run task-pilot [task_id ...]` | Preflight proposed/backlog tasks and persist validated selectors. Omit IDs for automatic discovery. `--base-branch`, `--max-tasks`, `--max-partition-size`, `--wait`, `--json`. |
| `orbit run ship-sweep` | Dispatch ship runs in every workspace with `[workflow] auto_ship = true`. `--dry-run`. |
| `orbit run job <job_id>` | Run any job by ID or YAML path. `--input key=value`, `--wait`. |
| `orbit run agent <prompt>` | Operator-only: invoke an agent on the host to investigate and report. It runs outside the filesystem sandbox, changes no task, and dispatches nothing. `--cwd`, `--crew`, `--timeout` (default 1800 s, max 7200), `--idempotency-key`, `--provider-sandbox`. Returns a run ID; read it with `orbit run show` / `logs`. |

See [Delivery Workflows](../../getting-started/workflows/).

### Tasks

| Command | Purpose |
|---|---|
| `orbit task add` | Create a task. `--title` and `--complexity` are required. |
| `orbit task update <id>` | Update fields. `--approve` takes the next approval step (`proposed → backlog`, `review → done`); `--status` follows the [lifecycle table](../../concepts/tasks/#transition-rules), and `--force` overrides it. |
| `orbit task list` | List tasks. Status-neutral by default; filter with `--status`, `--tag`, `--path`, `--ready`, `--ref`. |
| `orbit task show <id>` | Show one task, found by ID across registered workspaces. `--fields` projects specific fields. |
| `orbit task archive <id>` | Archive a task from any status. Archived is terminal: restore to any other status with `task update <id> --status <status> --force`. |
| `orbit task artifact` | Manage task artifact files. |
| `orbit task lint [id]` | Flag context declarations that need repair and vague acceptance criteria. Omit the ID to sweep active tasks; `--restore-pruned` re-declares `context_files` entries an earlier prune recorded in task history; `--status` narrows the sweep. |
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
| `orbit gc worktrees` | Report job-run worktrees whose task has settled; `--confirm` reaps them. `--run <ID>` restricts to one run, `--older-than-hours <N>` to runs finished at least that long ago. Dry-run skips the recursive byte estimate unless `--estimate-bytes`. |

## Observe

| Command | Purpose |
|---|---|
| `orbit search reindex` | Rebuild the lexical task index after imports or restores; reports task and chunk counts. |
| `orbit search <query>` | Search tasks and frictions using lexical matching; `--workspaces <SELECTOR>` (repeatable) federates across registered checkouts and is distinct from the global `--workspace` routing selector; `--all-workspaces` searches every active workspace on this machine; task fields use FTS5 BM25 with non-adjacent term matching. |
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
| `orbit job list` \| `show` \| `run` \| `replay` \| `resume` | View job definitions; run one by ID or YAML path, replay a previous run from step 0, or resume an interrupted run from its step checkpoints. |
| `orbit tool list` \| `show` \| `run` \| `add` \| `scaffold` \| `remove` \| `enable` \| `disable` \| `doctor` | View and run registered tools, manage external tool and MCP plugins, and validate tool health. |
| `orbit policy list` \| `show` \| `check <profile> <path>` | View filesystem policies; `check` dry-runs a path against a profile. See [Policy Format](../policy-format/) and [Scoping](../scoping/). |
| `orbit executor` | View executors. |

## Scheduler

| Command | Purpose |
|---|---|
| `orbit clock tick` | The scheduler pass: fire due routines and mint due auto-tasks. `--dry-run`, `--verbose`, `--json`. The global `--workspace <SELECTOR>` restricts the pass to one registered workspace. |
| `orbit sweep` | Compatibility alias for `orbit clock tick`, with identical arguments and output. |
| `orbit routine list` \| `show` \| `pause` \| `resume` | Inspect routines and pause them host-locally. |
| `orbit clock status` \| `pause` \| `enable` \| `set` | Control the host OS scheduler clock. |
| `orbit clock repair` | Rewrite the installed clock unit when it names a missing, moved, or stale program, then re-register it. Run automatically as the last `orbit update` convergence step. |
| `orbit routine init [--install-clock]` | Read this machine's identity and optionally install the OS clock unit. |
| `orbit auto-task add` \| `list` \| `show` \| `update` \| `toggle` \| `mint` | Define recurring auto-task templates and mint from them. |
| `orbit auto-task recover` \| `reset` | Preview or apply audited repair of a delivery consumer: `recover` unsticks one stalled by a settings change and keeps its coverage debt; `reset` forgets the debt and re-baselines at the branch head. |

See [Schedule Recurring Work](../../how-to/recurring-work/).

## Services

| Command | Purpose |
|---|---|
| `orbit mcp init` / `orbit mcp remove` | Register or unregister MCP client integration. Clients: `claude`, `codex`, `gemini`, `antigravity`, `grok`, `cursor`, `vscode`, `windsurf`, or `--all`. `--scope workspace` (default, repo-local) or `home` (user-level). `--federated` manages the mux entry separately. |
| `orbit mcp serve` | Serve the MCP tool surface over stdio, or act as a client with `--mode remote <SSH_HOST>` / `--mode federated`. `--operator` serves operator authority, and on a client mode it is also the operator statement for every SSH destination opened; `--orchestrator <crew>` sets the orchestrator attribution recorded on tasks the session creates, and grants nothing. |
| `orbit mcp listen [ADDR]` | Serve the same surface on a TCP socket. Binds `127.0.0.1:7879` unless `--allow-non-loopback` is passed. |
| `orbit web serve` | Serve the Orbit dashboard. Serves the registry under the resolved root, so `orbit --root <ROOT> web serve` exposes only `<ROOT>`'s workspaces. `--workspace <SELECTOR>` preselects one of them. `--operator` grants Operations controls without a TTY or `ORBIT_OPERATOR`. |
| `orbit web connect` | Open a remote workspace's dashboard over an SSH tunnel. `--workspace <SELECTOR>` preselects the remote workspace; it takes no `--root`. Spawns the remote server with `--operator` by default; `--no-operator` restores read-only Operations. |

See [Use the Dashboard](../../how-to/dashboard/) for connection, workspace scope, Operations controls, and authorization. See [Set Up MCP](../../how-to/mcp-integration/) for the agent tool surface.
