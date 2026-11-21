---
title: CLI commands
description: "A map of the Orbit CLI command surface, grouped the way orbit --help groups it."
---

Find the command for a job and see its shape. `orbit <command> --help` is the
full reference for every argument and option.

## Global options

| Option | Effect |
|---|---|
| `--root <ROOT>` | Use this Orbit data directory. Overrides every other source. |
| `--workspace <SELECTOR>` | Select an active workspace by registered name, `ws_*` ID or checkout path. |
| `--format <MODE>` | `auto` (a table on a terminal, plain text when piped), `table`, `plain`, `json` or `ndjson`. |
| `--json` | Shorthand for `--format json`. |
| `--header` | Start `plain` output with a header row. |

Every subcommand accepts these options, before or after it.

## Setup

| Command | Purpose |
|---|---|
| `orbit init` | Set up the global Orbit root (`~/.orbit`), this machine's identity and task-ID prefix, and a [human crew](/reference/config/#human-crews). `--non-interactive` skips prompts; `--force` resets the root to shipped defaults. |
| `orbit workspace init` | Register the current repository as a workspace. Common options: `--name`, `--base-branch`, `--ship-mode pr\|local`, `--role owner\|replica`, `--mcp`, `--preset <file>` ([presets](/reference/config/#workspace-presets)). |
| `orbit workspace list\|show` | List registered workspaces, or show the current one. |
| `orbit workspace rebind --path <dir> [--dry-run]` | [Rebind this host to an already moved checkout](/how-to/workspace-maintenance/#rebind-a-moved-checkout) (distinct from [source-remote rebind](/how-to/workspace-maintenance/#rebind-a-moved-source-git-remote)). `--dry-run` leaves the checkout binding, pending rebind record and audit log unchanged; generation admission and registry lock bookkeeping can still write when the store exists. A missing host store is refused before generation admission and writes nothing. |
| `orbit workspace sync [--check] [--all]` | [Converge managed artifacts](/how-to/workspace-maintenance/#synchronize-shipped-defaults) on the defaults this binary ships. `--check` writes nothing; `--all` covers every workspace on this host. |
| `orbit workspace role <workspace> <owner\|replica>` | Validate or reassert this checkout's role. |
| `orbit workspace ship-mode [pr\|local]` | Print the workspace's ship mode, or change it. |
| `orbit workspace source-remote show` \| `rebind --remote <URL>` | Show the source-repository identity, or replace the remote after a repository transfer. |
| `orbit workspace publication bind\|show\|rebind\|remove` | Manage the binding to a [task-publication repository](/how-to/task-publication/). |
| `orbit workspace remove\|teardown <workspace>` | `remove` deregisters a workspace and keeps its `.orbit`; `teardown --confirm` removes all Orbit artifacts from it. |
| `orbit config show\|keys\|path` \| `get <key>` \| `set <key> <value>` | Read and write [configuration](/reference/config/). `set --global` targets `~/.orbit/config.toml`. |
| `orbit config browser-kit status\|install\|verify\|remove` | [Manage the headless browser kit](/getting-started/install/#prepare-a-browser-kit-for-ui-tasks) that workers use to check UIs. |
| `orbit host add <ssh-target>` \| `list` \| `show <host>` \| `rename <host> <new-name>` \| `remove <host>` | Register and inspect the [remote Orbit hosts](/how-to/multi-host/) this machine reaches over SSH. |
| `orbit plugin add <source>` \| `upgrade\|enable\|disable\|remove\|show <name>` \| `list\|doctor\|sync` | [Install, grant and manage plugins](/how-to/plugins/). |
| `orbit plugin secret set\|rm <plugin> <name>` \| `list <plugin>` | Manage a plugin's [declared secrets](/how-to/plugins/#set-secrets); values are never shown. |
| `orbit plugin validate\|test <dir>` \| `scaffold <namespace>` \| `migrate <binary>` | [Author a plugin](/reference/plugin-manifest/): check a source, run its conformance cases, generate a starter, or convert v1 tool sidecars. |
| `orbit migrate` | List pending `.orbit` migrations; `--confirm` applies them. The `--backfill-*` and `--adopt-authority` options are [repairs](/how-to/workspace-maintenance/). |
| `orbit update` | [Install the newest published release](/getting-started/install/#stay-current), or `--version <version>`. `--check` only reports what is available. |

Installed plugins add command groups under `Plugins:` in `orbit --help`.

## Work

### Tasks

A task ID's prefix names the host that holds the task, and a command given a
task ID goes to that host. Pass `--workspace` to read that workspace instead,
with `--host <host>` when it is on another host. An unknown prefix fails with
`unknown_task_prefix`, an unreachable holder with `owner_unreachable`.

| Command | Purpose |
|---|---|
| `orbit task add --title <title> --complexity <low\|medium\|hard\|xhard>` | Create a task. Common options: `--description`, `--acceptance-criteria`, `--context`, `--crew`. |
| `orbit task update <id>` | Update fields or append a `--comment`. `--approve` takes the next approval step; `--status` follows the [transition rules](/concepts/tasks/#transition-rules) unless you add `--force`. |
| `orbit task repilot <id>` | Request one new pilot assessment. `--full` runs a full assessment instead of a re-check. |
| `orbit task archive <id> [--force]` | Archive a task in any status. If a live local run owns it, archiving requires `--force`. Restore it with `orbit task update <id> --status <status> --force`. |
| `orbit task list` | List tasks, 50 by default. Filter with `--status`, `--tag`, `--type`, `--path`, `--ready` or `--scheduled`. |
| `orbit task eligible` | List `backlog` and `proposed` tasks whose files overlap no `in-progress` or `review` task. `--explain` shows what holds the rest. |
| `orbit task show <id>` | Show one task. `--fields <a,b>` prints only those fields. |
| `orbit task artifact put <id> <source-path>` \| `get <id> <path>` | Store or read a [task artifact](/concepts/tasks/#task-artifacts). |

#### Health

| Command | Purpose |
|---|---|
| `orbit task lint [id]` | Flag context declarations that need repair and vague acceptance criteria. Without an ID it sweeps active tasks. |
| `orbit task flow` | Show filed-versus-closed rates over time, to tell whether the backlog is draining. |
| `orbit task locks list\|contention\|reserve` \| `release <reservation_id>` | Inspect, reserve and release the file locks that gate parallel dispatch. |
| `orbit task requeue <id> --reason <text>` | Return a blocked task to the queue onto its published candidate. `--discard-candidate` reruns it from scratch. |
| `orbit task recheck-blocked` | List tasks blocked by a missing provider launcher or transient infrastructure. `--confirm` returns the cleared ones to `backlog`. |
| `orbit task settle-merged [id]` | Settle tasks to `done` whose delivery pull request was merged outside Orbit. `--recovered-claim` settles a follower delivery rescued by hand. |
| `orbit task review-reset <id> --lineage <lineage> --reason <text>` | Reset one review lineage's budget. |
| `orbit task reconcile-review inspect\|status <id>` \| `submit <id> --request <key>` \| `accept-baseline <id>` | Let a `review` task complete when a recovered follower's delivery has already merged. `accept-baseline` records a required command that already fails at the base. |
| `orbit task retry-handoff <handoff_id>` | Retry a stopped authorized handoff through normal owner landing. |
| `orbit task recover-claim <claim_id>` | Operator only, on the owner: recover a live follower claim to a lifecycle-valid status. |
| `orbit task revoke-handoff <handoff_id>` | Operator only, on the owner: revoke an accepted handoff's completion authority. |

For when to use the recovery commands, see
[Recover from a failed delivery](/how-to/continuous-delivery/#7-recover-from-a-failed-delivery)
and [Stop, cancel and settle](/how-to/distributed-drain/#stop-cancel-and-settle).

#### Bundles

| Command | Purpose |
|---|---|
| `orbit task export --output <archive>` \| `import <archive>` | Export and import portable `tar.zst` [task bundles](/how-to/state-and-backup/#restore-tasks-or-move-them). |
| `orbit task publication publish\|status\|inspect\|restore` | Publish, check, read or restore a [task snapshot](/how-to/task-publication/). |
| `orbit task reindex` | Rebuild the task index from on-disk task bundles. |

### Scheduled tasks

A [schedule](/concepts/scheduling/#scheduled-tasks) is an earliest start, not
an exact time.

| Command | Purpose |
|---|---|
| `orbit task update <id> --not-before <time>` | Set the earliest start: an RFC 3339 instant or a local `YYYY-MM-DDTHH:MM`. `--clear-not-before` clears it. |
| `orbit task list --scheduled` | List future schedules, soonest first. |
| `orbit run ship <id>` | Start the named tasks now, even before their scheduled time. Records `schedule_overridden` and `readiness_overridden` history events. |

### Friction and search

| Command | Purpose |
|---|---|
| `orbit friction add --body <text> --model <model>` \| `list\|stats\|tags` \| `show\|update\|resolve <id>` \| `rehome <id> --to-workspace <workspace>` | [Report and triage friction](/how-to/friction/) records. `rehome` moves a record to the workspace that owns it. |
| `orbit search <query>` | Search tasks and frictions. `--all` includes every status; `--workspaces <selector>` or `--all-workspaces` searches other registered workspaces. |
| `orbit search reindex` | Rebuild the task search index after an import or restore. |

## Operate

### Workflows

| Command | Purpose |
|---|---|
| `orbit run ship [task_id ...]` | [Ship](/getting-started/workflows/) the named tasks, or the ready backlog, through the gated pipeline. Named tasks ship even when a drain would wait on them (scheduled start, unassessed complexity, no context files, context locks another in-flight task holds); their history records the overridden gates and lock holders. Other holds, including an active pilot, still apply. A wholly held request reports known holds immediately; a mixed request starts eligible tasks and records undispatched IDs and reasons in `orbit run show`. Check that run for holds that arise after submission, and retry held tasks when ready. `--mode pr\|local` chooses a pull request or in-place delivery. |
| `orbit run auto [--for <duration>] [--concurrency <n>]` | [Drain the backlog](/how-to/continuous-delivery/) for a time window; one pass without `--for`. |
| `orbit run auto --approve-proposed` | Also [approve `proposed` tasks](/how-to/continuous-delivery/#2-authorize-the-backlog) that pass task-pilot verification. |
| `orbit run auto --auto-fix` | Also [repair failing checks and conflicts](/how-to/continuous-delivery/#repair-blocked-pull-requests) on the pull requests the drain opens. |
| `orbit run auto --pull <selector>` | On a replica, [claim work from the owner's backlog](/how-to/distributed-drain/) and hand results back as pull requests. |
| `orbit run auto --stop` | Stop admitting new work. Running tasks continue. A hand stop also pauses the opt-in [standing-drain keeper](/reference/config/#settable-keys) (`workflow.standing_drain`); only a hand start sets it back on. |
| `orbit run ship\|auto --complete` | Also [finish delivery](/getting-started/workflows/#completing-work-with---complete) and move shipped tasks to `done` instead of `review`. |
| `orbit run ship\|auto --review <layer>` | [Add `before-landing` review](/concepts/tasks/#adding-review-to-one-run) to this run when workspace policy applies none. |
| `orbit run ship\|auto --allow-crew <crew>` | Restrict the run to tasks on these crews. |
| `orbit run readiness [task_id ...]` | [Explain](/how-to/continuous-delivery/#3-check-what-will-actually-start), read-only, why backlog tasks can or cannot start. |
| `orbit run task-pilot [task_id ...]` | Preflight `proposed` and `backlog` tasks and save validated selectors. Omit IDs to discover tasks automatically. |
| `orbit run job <job_id>` | Run any job by ID or YAML path, with `--input key=value`. |

### Run inspection

| Command | Purpose |
|---|---|
| `orbit run history` | Recent job runs. Filter with `-j <job_id>`, `--task`, `--state` and `--since`; `--failures` shows one row per task and root cause. |
| `orbit run show [run_id]` | State and step summary for a run, the most recent by default. `-s <step_id>` shows one step. |
| `orbit run logs [run_id]` | Raw stdout and stderr captured for a run. `--follow` streams it. |
| `orbit run events\|trace [run_id]` | Audit events recorded for a run, as a list or as a parent/child tree. |
| `orbit run profiles` | What each kind of delivery costs on this host in CPU and memory, minute by minute. `--accuracy` compares recorded admission forecasts with the measured load. |
| `orbit run settlements` | On an owner, the failure and release settlements that claimed leaves on follower hosts sent it. |
| `orbit run settlements-hydrate --follower-workspace <workspace>` | Operator only, on the owner: keep each settled claim's leaf run, read from its follower, as evidence. Narrow with `--claim`, `--task`, `--machine` and `--since`. |

### Maintenance

Each `orbit gc` command reports what it would remove; `--confirm` removes it.

| Command | Purpose |
|---|---|
| `orbit run cancel <run_id> --confirm` | Cancel a pending or running run. A cancelled task returns to `backlog` unless you pass `--block`. |
| `orbit run extend <run_id> --for <duration>` \| `concurrency <run_id> --set <n>` | Lengthen a live drain's window, or change how many tasks it keeps in flight, without restarting it. |
| `orbit gc worktrees` | Job-run worktrees whose task has settled. `--reclaim` targets only the `worktree.reclaim` paths and keeps the checkout. |
| `orbit gc tmp` | This checkout's scratch contents (`.orbit/tmp/`), when no job runs are active. |
| `orbit gc deployments` | Deployment directories, executable backups and store backups beyond the newest `--keep <n>` (default 2) of each. |
| `orbit gc audit` | Audit rows older than `retention.audit_days`. |
| `orbit gc runs` | Pipeline state of terminal runs older than `retention.runs_days`. The run and step rows stay. |
| `orbit gc journal` | Applied task commit journal rows older than 14 days, except each task's newest. |

### Jobs and tools

| Command | Purpose |
|---|---|
| `orbit job list` \| `show\|run <job_id>` \| `replay\|resume <run_id>` | List and show [job definitions](/concepts/activities-jobs/), run one, replay a run from step 0, or resume an interrupted run from its checkpoints. |
| `orbit tool list` \| `show\|run <name>` \| `doctor [name]` | List, inspect, run and health-check registered tools. `run --input '<json>'` passes the tool input. |
| `orbit tool add <path>` \| `remove\|enable\|disable <name>` | Register, remove, enable or disable an external tool. |
| `orbit script list` \| `show\|grant\|revoke\|history <name>` | Grant and inspect the [workspace scripts](/reference/activity-job-yaml/#script-steps) a job step may run. Editing a script makes its grant stale. |

### Fleet notices

| Command | Purpose |
|---|---|
| `orbit notice post --kind <warn\|incident\|maintenance\|info> --title <title>` | Post a [fleet notice](/how-to/fleet-notices/), or update the open one with the same key. |
| `orbit notice close [id] --reason <text>` | Close a notice by ID or `--dedupe-key`. |
| `orbit notice list` \| `show <id>` | List open notices, or show one. |

## Observe

| Command | Purpose |
|---|---|
| `orbit attention` | [List what waits on an operator](/how-to/attention/): pilot holds, blocked tasks, failed runs, open decisions, review waits, red CI and more. `--all` reads every workspace and registered host. |
| `orbit attention wait --since <cursor>` | Block until the attention list changes, with no model turn. It needs a wait grant: call `orbit.attention` with `wait_grant` over MCP and export the token as `ORBIT_ATTENTION_WAIT_TOKEN`. `--timeout` defaults to 30m. Exit codes: 0 changed, 3 timeout, 4 degraded set changed, 5 every read failed. |
| `orbit audit list\|stats` \| `show <id>` \| `export --output <file>` \| `prune --older-than <duration>` | Query, export and prune the audit event log. |
| `orbit log tail` | Print recent lines of the unified Orbit log feed. `-f` follows it. |
| `orbit doctor` | Check workspace health: config, database, disk, indexes, locks and runs. `--problems` prints only failures and warnings; the `--fix-*` flags run opt-in repairs. |
| `orbit doctor providers` | Show each executor's provider CLI, sandbox readiness and usage limits. |
| `orbit doctor fs-access <profile> <path>` | Dry-run a path against a [filesystem profile's](/reference/policy-format/) read and modify rules. |
| `orbit doctor admission-receipts [--compact]` | Show owner admission receipt counts and sizes; `--compact` replaces idle and settled receipts with tombstones. |

See [Investigate failures from the terminal](/how-to/diagnostics/).

## Analytics

The [dashboard](/how-to/dashboard/) shows the same rankings.

| Command | Purpose |
|---|---|
| `orbit agents scorecard --role <implementers\|reviewers\|pilots>` | Score one role's rows by model and recorded effort. `--window` (default `30d`), `--until` and `--min-support` select; `--by-type` and `--per-task` break implementers down. |
| `orbit agents show --role <role> --crew <crew>` | One row's terms and the tasks behind each count. |
| `orbit agents rank` \| `rank calibrate` | Rank implementer models by quality and reliability, in tie bands; `--live` skips the settle lag and is provisional. `calibrate` freezes a new season's noise scales. |
| `orbit agents ci-failures` | List required-check failures on candidate heads by cause: `code`, `flaky`, `infrastructure` or `unknown`. |
| `orbit agents vision-probe` | Check that each crew's model [sees images](/reference/executor-yaml/#image-input), with one probe run per crew. |
| `orbit codebase analytics` \| `show <path>` | Rank the repository paths that are risky for agents to change, or show one path's place in every ranking with the tasks behind each count. `--window` (default `90d`), `--min-support` and `--rollup file\|dir\|crate` select. |
| `orbit codebase related <path>` | List files that usually change alongside one file. |

## Scheduler

| Command | Purpose |
|---|---|
| `orbit clock tick` | Run one scheduler pass: fire due routines and mint due auto-tasks. `--dry-run` fires and mints nothing, but still persists checkout validation and opens the scheduler store. Alias: `orbit sweep`. |
| `orbit clock status\|pause\|enable\|repair` \| `set --cadence-seconds <n>` | Inspect and control the host scheduler clock. `repair` rewrites a clock unit that names a missing, moved or stale program. |
| `orbit routine list` \| `show <name>` | List routines with their next-due time and last fire, or show one. |
| `orbit routine enable\|disable\|pause\|resume <name>` | `enable` and `disable` change the routine's versioned definition (operator only); `pause` and `resume` act on this host only. |
| `orbit routine breaker reset\|bypass <name>` | [Reset or bypass a routine's host-local circuit breaker](/how-to/recurring-work/#recover-a-tripped-routine). |
| `orbit routine init [--install-clock]` | Read this machine's identity; `--install-clock` also installs the OS clock unit. |
| `orbit auto-task add --name <name> --title <title>` \| `list` \| `show\|update <name>` | Define and inspect auto-task templates. |
| `orbit auto-task toggle <name> <on\|off>` | Enable or disable a definition. |
| `orbit auto-task mint <name>` | Mint a task from a definition now, ignoring schedule, dedupe and `enabled`. |
| `orbit auto-task delete\|restore <name>` | Delete a definition, or reinstate a deleted shipped default. |
| `orbit auto-task recover\|reset <name>` | Preview or apply an [audited repair](/how-to/recurring-work/#repair-a-delivery-triggered-definition) of a delivery-triggered definition. |
| `orbit automation explain <definition>` | Show what a routine's or auto-task's [condition trigger](/reference/scheduling-yaml/#condition-triggers) matches over live state, writing nothing. |
| `orbit automation clear-stall <definition> --reason <text>` | Clear a stalled condition on the workspace owner. |

See [Schedule recurring work](/how-to/recurring-work/).

## Services

| Command | Purpose |
|---|---|
| `orbit mcp init\|remove` | Register or unregister an [MCP client integration](/how-to/mcp-integration/): `--client <name>`, `--all` or `--auto`, with `--scope workspace\|home`. `init --operator` registers operator authority. |
| `orbit mcp serve` | Serve the MCP tool surface over stdio. `--mode remote <ssh-host>` or `--mode federated` relays to other Orbit servers; `--role` and `--toolset` choose the surface ([roles](/how-to/mcp-integration/#roles), [toolsets](/how-to/mcp-integration/#toolsets)). |
| `orbit mcp listen [addr]` | Serve the same surface on a TCP socket, `127.0.0.1:7879` by default. |
| `orbit web serve` | Serve the [dashboard](/how-to/dashboard/) on `127.0.0.1:7878`. `--operator` grants operator capability for the governed controls. |
| `orbit web connect <ssh-host>` | Open a remote host's dashboard over an SSH tunnel. |

## Advanced

### Applications

| Command | Purpose |
|---|---|
| `orbit application consent --input-file <file> --credential-file <file>` | Operator only. Approve a [consent request](/reference/application-consent/) so a Unix service or CLI child can make scoped calls into a workspace. |
| `orbit application rotate\|revoke <consent_id>` | Replace a consent's credential (same two files as `consent`), or revoke the consent. |

### Decision requests

With `decision_channel.enabled` on, work held on a human decision raises a
[decision request](/how-to/decision-channels/).

| Command | Purpose |
|---|---|
| `orbit decision list` \| `show <id>` \| `answer <id> <answer>` | List open decision requests (`--all` adds answered, superseded and expired ones), show one with its allowed answers, or answer one as the operator. |
| `orbit decision raise --task <id> --question <text> --options <json>` | [Raise a multiple-choice decision](/how-to/decision-channels/#decisions-an-agent-raises-on-a-human-task) on a human-crew task. |
| `orbit decision reverse <id>` | Re-open a request that a precedent or a model answer settled. Operator only. |
| `orbit decision rule-auto <rule> <on\|off>` | Turn auto on or off for one corpus rule. |
| `orbit decision consolidate [--since <YYYY-MM-DD>]` | Propose corpus rule edits from human answers. Writes nothing. |
| `orbit decision submit <id> <answer> --channel <channel> --token <token>` | Submit a human's answer through a registered decision channel. |
| `orbit decision digest --channel <channel>` | Raise any missing approval requests for a channel and show its decision digest. |
| `orbit decision channel register <channel> --human <human> --plugin <plugin>` \| `list` \| `revoke\|rotate-key <channel>` | Register, list, revoke and rotate this host's decision channels. |

### Experiments

| Command | Purpose |
|---|---|
| `orbit experiment create <name>` \| `update <id>` \| `clone <id> --name <name>` | Define, edit or copy a draft [experiment](/how-to/experiments/): `--arms <json>`, `--hypothesis`, `--primary-metric`, `--mode shadow\|live` and more. |
| `orbit experiment start\|pause\|resume\|reweight\|conclude\|abandon <id>` | Move an experiment through its lifecycle. Operator only. `start` needs `experiments.enabled`; `pause`, `reweight`, `conclude` and `abandon` need `--reason <text>`; `conclude --arm <arm>` records the chosen arm. |
| `orbit experiment assign <id> --task <task> --arm <arm>` | Force a task into an active arm of a running experiment. |
| `orbit experiment readout <id>` | Show per-arm outcomes, intervals and the [verdict](/how-to/experiments/#read-the-readout). |
| `orbit experiment show <id>` \| `list` | Show one experiment with its arms and event log, or list the workspace's experiments. |
| `orbit experiment delete <id> --reason <text>` | Delete a finished experiment that never applied a treatment. |

### Crew memory (experimental)

[Crew memory](/how-to/crew-memory/) is off by default. Until
`crew_memory.enabled` is `true`, only `show`, `history` and `export` work. An
operator can
[grant an agent read access](/how-to/crew-memory/#grant-an-agent-read-access).

| Command | Purpose |
|---|---|
| `orbit memory show [<key>\|global] [--revision <n>]` \| `history <key>` | Show a memory space or summarise every space, or list a space's revisions and audit events. A space is keyed by model ID. |
| `orbit memory export [--dir <dir>]` | Write every current space to Markdown files, in `.orbit/memory` by default. |
| `orbit memory write <key> --file <path>` \| `reset <key>` \| `copy <from> <to>` | Replace a space with the given files, empty it, or copy one model's space to another. Operator only. |
| `orbit memory proposals` \| `proposal accept <id> [--edit <file>]` \| `proposal reject <id> --reason <text>` | List open global-memory proposals with their evidence, accept one into a new global revision, or reject it with a reason. |

### Residents and evolution

| Command | Purpose |
|---|---|
| `orbit resident show <task>` \| `list` | Show a [mandate](/reference/resident-mandates/) with its authority and grant history, or list mandates. |
| `orbit resident grant <task>` | Widen (`--add`, `--add-workspace`) or narrow (`--remove`, `--drop-workspace`, `--deadline`) a mandate's authority. |
| `orbit resident relay <task> --quote <words> --source <json>` | Relay a human's words to a resident, verbatim. It never changes authority. |
| `orbit resident control <task> <pause\|resume\|stop\|wake\|takeover\|release> --reason <why>` | Change whether a resident fires. `stop` ends the mandate. |
| `orbit resident checkpoint <task> --record <json>` | Append the live cycle's checkpoint record. |
| `orbit resident metrics\|digest` | Report the resident [metrics or digest](/reference/resident-mandates/#digest-and-metrics): interventions, escalations, reversals, top pains and open proposals. |
| `orbit evolution add --event <json>` \| `list` \| `show <id>` | Record an interactive cost event, list evolution signatures with summed costs, or show one record with its occurrences. |
| `orbit evolution decline <id> [--days <n>]` | Operator only: suppress proposals for a signature. |
