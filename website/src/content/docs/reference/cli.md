---
title: CLI Commands
description: "A map of the Orbit CLI command surface, grouped the way orbit --help groups it."
sidebar:
  order: 2
---

This page maps the command surface. For a command's exact options, run
`orbit <command> --help`.

## Global options

| Option | Effect |
|---|---|
| `--root <ROOT>` | Override the Orbit root directory. Highest precedence. |
| `--workspace <SELECTOR>` | Select a workspace by registered name, logical ID (`ws_*`), or absolute checkout path. Only an active workspace can be selected. Unlike `--root`, it does not change the root directory. |
| `--format <MODE>` | `auto` (default: a table on a terminal, plain text when piped), `table`, `plain` (full tab-separated values on any sink), `json`, or `ndjson`. |
| `--json` | Shorthand for JSON output, always pretty-printed. Accepted throughout the command tree, including plugin groups. Plugin-derived `--json` tool-input flags keep their own meaning; select output JSON at the root/group or with `--format json`. A conflicting `--format` produces a JSON usage error on stderr and exit 2. |

Every subcommand accepts these options too, so `orbit --workspace ws_x task list`
and `orbit task list --workspace ws_x` are equivalent. This site puts them
before the subcommand. `audit export` keeps its command-local `--format json|csv`
for the exported file; `--json` selects the output mode independently and
leaves the existing export confirmation unchanged.

## Environment

| Command | Purpose |
|---|---|
| `orbit init` | Set up the global Orbit root, this machine's `[machine]` identity and task-ID prefix, and the default skills. `--machine-name <NAME>` and `--task-prefix <PREFIX>` choose the identity (the prefix is required on first init). `--non-interactive` skips prompts, and then requires `--machine-name` on a fresh machine. `--skip-host-prerequisites` (or `ORBIT_SKIP_HOST_PREREQUISITES`) leaves the Linux Bubblewrap and AppArmor setup to the host's administrator. `--force` resets the global root to shipped defaults. |
| `orbit workspace init` | Register the current repository as a workspace. `--name`, `--base-branch`, `--ship-mode pr\|local`, `--role owner\|replica` (`replica` requires `--owner <machine_id>`), `--task-id-start <N>`, `--mcp`, `--inject-agent-rules`; `--force` reconciles an already registered workspace. |
| `orbit workspace list` \| `show` \| `sync` \| `role` | List and show registered workspaces, converge managed artifacts (`sync`), and validate this checkout's role (`role`). |
| `orbit workspace ship-mode [pr\|local]` | Print the workspace's ship mode, or rebind it in the workspace registry without re-running init. Config files and managed defaults are left untouched. |
| `orbit workspace source-remote show` \| `rebind` | Show the workspace's source-repository identity, or replace the source remote after a repository transfer (`rebind --remote <URL>`; try `--dry-run` first). |
| `orbit workspace publication bind` \| `show` \| `rebind` \| `remove` | Manage this owner's binding to a dedicated task-publication repository. See [Publish and Restore Tasks](../../how-to/task-publication/). |
| `orbit workspace remove` \| `teardown` | `remove` deregisters a workspace; `teardown` removes Orbit artifacts from it. |
| `orbit config show` \| `get` \| `set` \| `keys` \| `path` | Read and write configuration, including this machine's identity under `machine.*`. Rename the machine with `orbit config set --global machine.name <value>`. See [Configuration](../config/). |
| `orbit host add` \| `list` \| `show` \| `rename` \| `remove` | Register and inspect the remote Orbit hosts this machine reaches over SSH. `add <ssh-target> [--name]` reads the host's machine ID, name and task prefix from the host; `list [--no-probe]` and `show <host>` read reachability, version, pull protocol and workspaces live; `remove --force` drops a host a replica or pull drain still uses. Federated MCP, pull drains and replica worktree GC route to these hosts. |
| `orbit plugin add` \| `list` \| `show` \| `upgrade` \| `enable` \| `disable` \| `remove` \| `doctor` \| `sync` | Install and manage plugins. `validate`, `test`, `scaffold`, and `migrate` support plugin authoring. Installed plugins' command groups appear under `Plugins:` in `orbit --help`. |
| `orbit plugin secret set` \| `list` \| `rm` | Manage the secrets a plugin declares in `spec.secrets`. `set <plugin> <name>` reads the value from stdin or a no-echo prompt, never from an argument. `list <plugin>` shows whether each secret is set, never its value. `rm <plugin> <name>` deletes one. |
| `orbit migrate` | List pending `.orbit` layout and store migrations; `--confirm` applies them. |
| `orbit update` | Install a published release and converge to it. `--check`, `--version`, `--allow-downgrade`. `--preflight` checks whether running Orbit processes prevent an upgrade without opening stores, and `--contract` describes the executable admission protocol without opening state. `--local-candidate <PATH> --source-commit <SHA>` installs an operator-attested local build pinned to a full commit, with `--write-candidate-manifest`, then `--candidate-manifest` and `--install-target`. |

A plugin source keeps its plugin in a `.orbit-plugin/` directory that holds
`plugin.yaml`. That directory is the plugin root and the only tree installed.
`plugin add`, `validate`, `test`, `sync`, and `upgrade` refuse a top-level
`plugin.yaml`, with no fallback: move `plugin.yaml` and its tree into
`.orbit-plugin/`, or run `orbit plugin scaffold` to create the current layout.
Plugins install once per machine; a checkout pins the plugins it uses in
`.orbit/plugins.yaml`, which git ignores with the rest of `.orbit/`.

## Knowledge

### Tasks

A task ID's prefix names the host that holds it. A command that addresses one
task by ID (`show`, `update`, `artifact get` and `put`, `review-reset`,
`reconcile-review`, and `orbit tool run` of those tools) goes to that host when
it names no `--workspace`. An ID with this machine's prefix runs here; a
registered host's prefix is answered by that host; any other prefix is refused
with `unknown_task_prefix`. If the holder does not answer, the command fails
with `owner_unreachable` rather than reading a local copy. To read a local
mirror, pass `--workspace` explicitly. `--host <host>` names the host for
`--workspace`, by name or machine ID, so you can write `--host alpha
--workspace orbit` instead of the selector `alpha` lists.

| Command | Purpose |
|---|---|
| `orbit task add` | Create a task. `--title` and `--complexity` are required. |
| `orbit task update <id>` | Update fields. `--approve` takes the next approval step (`proposed → backlog`, `review → done`). `--status` follows the [lifecycle table](../../concepts/tasks/#transition-rules); `--force` overrides it. |
| `orbit task list` | List tasks of every status by default. Filter with `--status`, `--tag`, `--path`, `--ready`, `--ref`. |
| `orbit task eligible` | List `backlog` and `proposed` tasks whose context-file lock surface overlaps no `in-progress` or `review` task, by the same lock test automatic dispatch uses. Nothing else is checked: not dependencies, complexity, groups, crew, or overlap between candidates. `--status backlog\|proposed`, `--path`, `--limit`; `--explain` also lists held-back candidates with the overlapping file and the task holding it. Read-only. JSON shape: [Response shapes](../../how-to/mcp-integration/#response-shapes). |
| `orbit task show <id>` | Show one task: on this machine, found by ID across registered workspaces, or on the host its prefix names. A proposed or backlog task gets a `Readiness:` line naming what blocks drain approval or admission, with a fix for each gap. `--fields` selects specific fields. |
| `orbit task archive <id>` | Archive a task from any status. Archived is terminal; restore with `task update <id> --status <status> --force`. |
| `orbit task artifact` | Manage task artifact files. |
| `orbit task lint [id]` | Flag context declarations that need repair and vague acceptance criteria. Without an ID, sweeps active tasks; `--status` narrows the sweep. `--restore-pruned` re-declares `context_files` entries that an earlier prune recorded in task history. |
| `orbit task reconcile-review inspect <id>` \| `submit <id> --request <KEY>` \| `status <id>` \| `accept-baseline <id>` | Let a `review` task complete when a recovered follower's delivery has already merged. `inspect` shows whether the merged delivery can be reconciled, and why not. `submit` validates and independently reviews the merged head; resubmitting the same `--request` key replays the same reconciliation. `status` shows reconciliation outcomes, their runs, and the next step (`--reconciliation` narrows it to one). `accept-baseline` records an audited disposition of a required command that already fails at the base: `--reconciliation`, `--command`, `--remediation <commit>` (a commit on the landing branch that remediates the failure), and `--reason` are all required. |
| `orbit task flow` | Show filed-versus-closed rates over time, to tell whether the backlog is draining. |
| `orbit task recheck-blocked` | List tasks blocked because dispatch could not find the provider launcher, and whether it resolves now. `--confirm` returns the ones that resolve to `backlog` with an `infra_block_cleared` history note. Tasks blocked by their own failure stay blocked. The launcher is resolved from the invoking shell's `PATH` and `HOME`. |
| `orbit task review-reset <id>` | Reset one review lineage's budget with an audited reason. `--lineage` and `--reason` are required; `--adopt-configured-budget` takes the current configured budget instead of the captured one. |
| `orbit task locks list` \| `contention` \| `reserve` \| `release` | Inspect and manage the file locks that gate parallel dispatch. |
| `orbit task export` \| `import` \| `reindex` | Export and import portable `tar.zst` task bundles, and rebuild the task index. |
| `orbit task publication publish` \| `status` \| `inspect` \| `restore` | Publish, check, read, or restore a task snapshot. Nothing publishes automatically. |

### Friction and search

| Command | Purpose |
|---|---|
| `orbit friction add` \| `list` \| `show` \| `stats` \| `tags` \| `update` \| `resolve` \| `rehome` | Report and triage friction records. `rehome <id> --to-workspace <workspace>` moves a record to the registered workspace that owns it. |
| `orbit search <query>` | Lexical search over tasks and frictions. Task fields use FTS5 BM25 ranking, and query terms need not be adjacent. `--workspaces <SELECTOR>` (repeatable) adds other registered checkouts and is separate from the global `--workspace`; `--all-workspaces` searches every active workspace on this machine. |
| `orbit search reindex` | Rebuild the lexical task index after an import or restore. Reports task and chunk counts. |

## Operate

### Workflows

| Command | Purpose |
|---|---|
| `orbit run ship [task_id ...]` | Ship the selected tasks, or the ready backlog, through the gated pipeline. Returns a run ID immediately. `--base`, `--allow-crew`, `--strict-worker-containment`, `--claim-token`. |
| `orbit run ship --mode local` | Deliver in place instead of opening a pull request. |
| `orbit run auto [--for <duration>]` | Drain the backlog for a time window. `--concurrency`; `--allow-crew`; `--low-complexity-crews`, `--medium-complexity-crews`, `--hard-complexity-crews`, `--xhard-complexity-crews` (crew pools for tasks with no crew, as `crew` or `crew:weight`, overriding the `[workflow]` pools); `--strict-worker-containment` (require a systemd user scope for the coordinator and every worker, overriding `machine.worker_containment_strict` for this drain); `--claim-token` when another operator holds the workspace claim. |
| `orbit run auto --approve-proposed` | Authorize every pass of a local drain to pilot and approve qualifying `proposed` tasks, including tasks filed during the window. Qualifies with `no-diff-expected`, or context files and an assessed complexity; duplicate, already-landed, conflict or warning findings keep it proposed. `no-auto-approve` opts out. Off by default; approval is separate from `--complete`. See [Authorize the backlog](../../how-to/continuous-delivery/#2-authorize-the-backlog). |
| `orbit run auto --pull <SELECTOR>` | On a replica checkout, pull work from the owner named by the host-qualified selector, or by `--pull <workspace> --host <owner>`. Each claim runs as a leaf that ends at a pull request handed back to the owner, which keeps landing authority. Takes `--for` and `--concurrency`; `--allow-crew` limits which crews' tasks are claimed. Conflicts with `--complete`, `--approve-proposed`, `--strict-worker-containment`, the crew-pool flags, `--claim-token`, and `--stop`. See [Set Up a Distributed Drain](../../how-to/distributed-drain/). |
| `orbit run auto --stop` | Stop new admissions for this workspace's active auto coordinator. Admitted workers keep running; this is not cancellation. On a replica it also delivers any recorded pull settlements to the owner, even with no active drain. |
| `orbit run ship --complete` / `orbit run auto --complete` | Also authorize the run to finish delivery and move the tasks it ships from `review` to `done`. Off by default. |
| `orbit run readiness [task_id ...]` | Explain, read-only, why backlog tasks can or cannot start. `--concurrency`, `--allow-crew`, `--limit`. |
| `orbit run task-pilot [task_id ...]` | Preflight `proposed` and `backlog` tasks and save validated selectors. Omit IDs to discover tasks automatically. `--base-branch`, `--max-tasks`, `--max-partition-size`, `--wait`. |
| `orbit run ship-sweep` | Dispatch ship runs in every workspace with `[workflow] auto_ship = true`. `-m`/`--mode pr\|local` overrides the pipeline mode of every dispatched run; without it each workspace uses its own ship mode, `pr` by default. `--dry-run`. |
| `orbit run job <job_id>` | Run any job by ID or YAML path. `--input key=value`, `--wait`. |
| `orbit run agent <prompt>` | Operator only. Run an agent on the host to investigate and report. It runs outside the filesystem sandbox, changes no task, and dispatches nothing. `--cwd`, `--crew`, `--timeout` (default 1800 s, max 7200), `--wait`, `--idempotency-key`, `--provider-sandbox`. Returns a run ID to read with `orbit run show` or `logs`; `--wait` blocks and prints the answer instead. |

See [Delivery Workflows](../../getting-started/workflows/).

### Run inspection

| Command | Purpose |
|---|---|
| `orbit run history` | Recent job runs. `-j <job_id>` filters to one job. |
| `orbit run show [run_id]` | State and step summary for a run; defaults to the most recent. `-s <step_id>` shows one step. |
| `orbit run logs [run_id]` | Raw stdout and stderr captured for a run. `--follow` streams until the run ends. |
| `orbit run events [run_id]` | Audit events recorded for a run. |
| `orbit run trace [run_id]` | Parent/child tree of a run's audit events. |
| `orbit run settlements` | On an owner, the failure and release settlements that claimed leaves on follower hosts sent it, newest first, with each one's evidence class. Those leaves never appear in the owner's `run history`. `--since <time\|duration>`, `--json`, `--no-reconcile`. |

### Maintenance

| Command | Purpose |
|---|---|
| `orbit run cancel <run_id> --confirm` | Cancel a pending or running job run and release its task reservations. A task leaf returns to `backlog` with its candidate resumable; `--block` keeps it blocked for manual recovery. `--reason <TEXT>` records a note with the cancellation audit event. A pull drain cancels gracefully: unlaunched claims return to the owner's backlog, and launched leaves finish and settle. `--force` also stops a drain's in-flight leaves. |
| `orbit run concurrency <run_id> --set N` | Change how many tasks a live drain keeps in flight. `--reason`, `--if-revision`. `--claim-token <TOKEN>` (or `ORBIT_WORKSPACE_CLAIM_TOKEN`) supplies this workspace's exclusive claim when another operator holds one. |
| `orbit gc worktrees` | Report job-run worktrees whose task has settled; `--confirm` reaps them. `--run <ID>` limits to one run; `--older-than-hours <N>` to runs finished at least that long ago. A whole-worktree dry run skips the size estimate unless you pass `--estimate-bytes`. `--reclaim` reports bytes per declared `worktree.reclaim` pattern and reclaims those paths with `--confirm`, keeping the checkout. Terminal runs, registration, worker liveness, confinement, symlinks and Git content are checked. `--target-only` remains an alias. |
| `orbit gc audit` | Report audit rows older than `retention.audit_days` (host-wide command audit and this workspace's run audit) and the audit blobs no remaining row names, with rows and bytes per table and blob root; `--apply` deletes them in batches of 1,000 rows. A blob written in the last 24 hours or marked pending by a write in progress is kept. Reports the store file's freelist; run `VACUUM` yourself to return it. `--older-than-days <N>`. |
| `orbit gc runs` | Report this workspace's terminal runs older than `retention.runs_days` that still keep pipeline state; `--apply` drops that state and stamps the run archived. The run, its steps and its summary stay, so `run show` and run history keep working. Held and non-terminal runs are never touched. `--older-than-days <N>`. |
| `orbit gc tmp` | Report this workspace checkout's scratch contents (`.orbit/tmp/`) and empty it with `--confirm`, keeping the directory. `--confirm` refuses while any job run is pending, running, or retrying, and the command needs Linux or macOS. Without `--confirm` it only reports; `--dry-run` requests that default explicitly. |

On a replica, `orbit gc worktrees` reaps a claimed run whose claim is already
settled with the owner without checking its task. For any other run, it asks
the owner machine for the task's status. A failed lookup is skipped and
reported as `skipped:owner_unreachable` (transport failure),
`skipped:owner_lookup_failed` (any other lookup failure), or
`skipped:no_owner_route` (no route to the owner), with the reason in `detail`.

### Jobs and tools

| Command | Purpose |
|---|---|
| `orbit job list` \| `show` \| `run` \| `replay` \| `resume` | List job definitions and the activity each step runs. Run a job by ID or YAML path, replay a previous run from step 0, or resume an interrupted run from its step checkpoints. See [Activities and Jobs](../../concepts/activities-jobs/). |
| `orbit tool list` \| `show` \| `run` \| `add` \| `scaffold` \| `remove` \| `enable` \| `disable` \| `doctor` | List, inspect, and run registered tools; manage external tools and MCP plugins; check tool health. `scaffold` is a deprecated alias for `orbit plugin scaffold`. |

## Observe

| Command | Purpose |
|---|---|
| `orbit audit list` \| `show` \| `prune` \| `export` \| `stats` | Query the audit event log. |
| `orbit log tail` | Tail the unified Orbit log feed. |
| `orbit doctor` | Check workspace health: config, database, disk, indexes, locks, runs, and tasks blocked by a missing provider launcher (`infra-blocked-tasks`). Terminal tables are followed by full non-ok findings and verbatim `Fix:` remediations, ordered errors before warnings. Truncated tables with findings point to `--format plain` or `--json`; all-ok output has no findings or hint. The `--fix-*` flags are opt-in repairs, and `--remove-graph` removes retired graph state from this worktree and the shared workspace. `--confirm` authorizes the destructive repairs, such as `--fix-orphan-task-stores`. |
| `orbit doctor providers` | Show each executor's provider CLI, whether dispatch can find it (and where), and its resolved `sandbox` mode. |
| `orbit doctor fs-access <profile> <path>` | Dry-run a workspace-relative path against a filesystem profile's read and modify rules. See [Policy Format](../policy-format/) and [Scoping](../scoping/). |

## Scheduler

| Command | Purpose |
|---|---|
| `orbit clock tick` | Run one scheduler pass: fire due routines and mint due auto-tasks. `--dry-run`, `--verbose`. The global `--workspace <SELECTOR>` limits the pass to one registered workspace. |
| `orbit sweep` | Alias for `orbit clock tick`, with the same arguments and output. |
| `orbit routine list` \| `show` \| `pause` \| `resume` | Inspect routines, and pause or resume them on this host only. `list --include-inactive-plugins` (alias `--all`) also shows routines seeded by a plugin that is switched off, marked inactive with the reason. |
| `orbit clock status` \| `pause` \| `enable` \| `set` | Control the host OS scheduler clock. |
| `orbit clock repair` | Rewrite the installed clock unit when it names a missing, moved, or stale program, then re-register it. `orbit update` runs this as its last step. |
| `orbit routine init [--install-clock]` | Read this machine's identity; `--install-clock` also installs the OS clock unit. |
| `orbit auto-task add` \| `list` \| `show` \| `update` \| `toggle` \| `mint` | Define recurring auto-task templates and mint tasks from them. `list --include-inactive-plugins` (alias `--all`) also shows definitions seeded by a plugin that is switched off, marked inactive with the reason. |
| `orbit auto-task delete` \| `restore` | `delete` removes a definition with its scheduler cursor and delivery consumer state (`--force` works even while a minted task is open). `restore` reinstates a deleted shipped default with its shipped content. A deleted shipped default stays out of later reseeds. |
| `orbit auto-task recover` \| `reset` | Preview or apply an audited repair of a delivery consumer. `recover` unsticks one stalled by a settings change and keeps its coverage debt; `reset` forgets the debt and re-baselines at the branch head. |

See [Schedule Recurring Work](../../how-to/recurring-work/).

## Services

| Command | Purpose |
|---|---|
| `orbit mcp init` / `orbit mcp remove` | Register or unregister an MCP client integration. Clients: `claude`, `codex`, `gemini`, `antigravity`, `grok`, `cursor`, `vscode`, `windsurf`, or `--all`. `--scope workspace` (default, repo-local) or `home` (user-level). `--federated` manages the mux entry separately. |
| `orbit mcp serve` | Serve the MCP tool surface over stdio, or act as a client with `--mode remote <SSH_HOST>` or `--mode federated`. `--operator` serves operator authority; in a client mode it is also the operator statement for every SSH destination opened. `--orchestrator <crew>` sets the orchestrator recorded on tasks the session creates and grants nothing. |
| `orbit mcp listen [ADDR]` | Serve the same surface on a TCP socket. Binds `127.0.0.1:7879` unless you pass `--allow-non-loopback`. `--workspace <selector>` binds each accepted session to that workspace by default. |
| `orbit web serve` | Serve the Orbit dashboard for the registry under the resolved root, so `orbit --root <ROOT> web serve` shows only `<ROOT>`'s workspaces. `--workspace <SELECTOR>` preselects one. `--operator` enables Operations controls without a TTY or `ORBIT_OPERATOR`. |
| `orbit web connect` | Open a remote workspace's dashboard over an SSH tunnel. `--workspace <SELECTOR>` preselects the remote workspace; `--root` is refused. The remote server starts with `--operator` unless you pass `--no-operator`, which keeps Operations read-only. |

See [Use the Dashboard](../../how-to/dashboard/) for connection, workspace
scope, Operations controls, and authorization. See
[Connect Your Agent](../../how-to/mcp-integration/) for the agent tool surface.
