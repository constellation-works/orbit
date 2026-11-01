# Tools, routing, and authority

Orbit exposes a curated MCP surface and a larger CLI catalog. Use the installed
version's `tools/list`, `orbit tool list`, and command `--help`; a similar name
is not evidence that an operation exists. This skill explains the contracts
without requiring an Orbit source checkout.

## Select the store before reading or writing

For a single task addressed by ID, use the prefix-routing rules below. For
workspace-scoped calls, select the store explicitly.
In a managed activity, use the injected task and inherited workspace binding;
its allowlist may omit `orbit.workspace.list`. For an unbound MCP session, call
`orbit_workspace_list({})` on the configured connection first. For CLI-only use,
inspect `orbit workspace list` and `orbit workspace show` on the intended host,
then use its registered workspace in tool calls. MCP setup is not a prerequisite
for local task tracking. Inspect host, workspace, ownership, availability and
capabilities where returned.

Workspace discovery checks checkout roots against the destination's filesystem
on each call without saving status changes. A removed checkout disappears from
a direct server's list. Federated discovery keeps its descriptor with
`checkout_health: invalid`; restore or re-register the checkout before using it.

- Direct server: pass the returned logical workspace ID as `workspace`.
  The server can also resolve registered names and paths, but IDs avoid
  ambiguity. An explicit selector overrides its session binding.
- Federated server: copy the returned `selector` exactly, including its
  `hm_…/ws_…` qualification for workspace-scoped calls. Bare workspace names,
  bare workspace IDs, and local paths cannot route federated calls. Single-task
  calls without a selector route by task prefix. The mux is deliberately not
  bound to one workspace.
- A direct session can bind through `orbit mcp serve --workspace <selector>`
  or initialize metadata. An unbound session requires a per-call selector for
  workspace-scoped calls; server cwd never chooses the workspace.
- A managed child inherits trusted workspace and run identity from its envelope.
  Do not replace those with a root or registry from another checkout.

If a host is unavailable, report that fact. Reading a publication is explicitly
labelled snapshot access, not a substitute for live task state. Never create
records in a second store merely to get past a connection error.

## Task IDs and host selection

Register a remote with `orbit host add <ssh-target>` and inspect `orbit host
list`. On the CLI and federated MCP, a single-task call without an explicit
workspace goes to the host its task prefix names: the local prefix runs here,
a registered remote prefix goes there, and an unknown prefix fails closed.
Show or update another host's task directly, without SSH or `--workspace`:

```bash
orbit tool run orbit.task.show --input '{"id":"<task-id>","model":"<agent-family>"}'
orbit tool run orbit.task.update --input '{"id":"<task-id>","comment":"<progress>","model":"<agent-family>"}'
```

The ID-routed tools are `orbit.task.show`, `update`, `reject`, `delete`,
`artifact.get`, `artifact.put`, `review_reset`, `reconcile_review` and
`requeue` (each under `orbit.task.`). Routing applies only where that tool is advertised and
authorized. `orbit.task.add`, `list`, `eligible`, `lint`, `pull` and `locks*`
keep workspace selection; search, friction and workflow-run tools do not route
by task prefix. There is no fan-out search across hosts. Direct local/remote
MCP servers and `mcp listen` do not relay: an ID-only call for a registered
remote prefix returns `task_prefix_remote`.

An explicit `workspace` or CLI `--workspace` wins over the prefix: a read can
address that store's mirror, while a write must still pass its sole-writer
check. An explicit CLI root also selects a store. Routing never falls back to
a mirror when a host is down. Claimed workers retain their owner binding and
cannot use `--host` to replace it.

For workspace-scoped CLI tool calls, name a host and a workspace as that host
lists it:

```bash
orbit tool run orbit.task.list --host <name> --workspace <workspace-name-or-ws_id> --input '{"model":"<agent-family>"}'
```

`--host` accepts an exact registered host name or `machine_id`. Orbit reads
that host's live workspace list, matches the name or `ws_*` ID, and copies
its selector; never build a selector by concatenation. The flag is accepted
on `orbit tool run` and, with `--workspace`, on `orbit task show`, `update`,
`artifact put|get`, `review-reset` and `reconcile-review` subcommands. Put it
after the task subcommand. Agents keep writes on registered tools for
attribution. Remote creation also uses `orbit tool run orbit.task.add` with
`--host` and `--workspace`; human `task add` and `task list` are host-local.
Other host-local commands, including `workspace`, `config`, `doctor`,
`run show/history/logs` and `update`, reject the routing flag. Their diagnostic
names the command to run on that host over SSH.

On a replica, `orbit run auto --host <owner-name> --pull <workspace-name-or-ws_id>`
uses the same live selector resolution. Without `--host`, `--pull` requires
the full host-qualified selector copied from discovery. Pulling from the local
host is refused; use plain `orbit run auto` for a local backlog. Starting a
drain still requires operator authorization.

### Routing failures and remedies

| Code | Remedy |
|---|---|
| `unknown_task_prefix` | Check `orbit host list`, then register the task's host with `orbit host add <ssh-target>`. Legacy rows have no prefix until migrated. |
| `task_prefix_remote` | Use federated MCP or the CLI's ID route, or connect directly to the named host and select its workspace. A direct MCP server does not relay. |
| `unknown_host` | Copy the exact name or machine ID from `orbit host list`; register a missing host or migrate its legacy row with `orbit host add`. |
| `owner_unreachable` | Restore SSH and the task host's Orbit process. If the error names a local mirror, select it explicitly for a labelled snapshot read. |
| `unreachable_destination` | Check that the registered SSH target logs in without a prompt and has `orbit` on PATH, then retry discovery. |
| `stale_route` | Select a workspace the host's live list reports; repair its registration on that host if it should exist. |
| `unknown_selector` | For an ambiguous name, use the host's exact `ws_*` ID. For bare `--pull`, supply `--host` or the full discovered selector. |
| `outcome_unknown` | The call was dispatched but its reply was lost. Inspect live state or the operation's receipt before retrying a write; do not mint a replacement request key. |
| `tool_not_on_this_host` | Discover the destination's tool catalog and use a supported operation or deploy the required matching build. |
| `capability_refused` | Follow the named authority/owner restriction. A replica or local-host pull is not an owner route; routing grants no extra capability. |
| `protocol_skew` | Deploy matching Orbit builds and restart long-lived processes on both endpoints before a new pull drain. |

Registration errors, the doctor's `hosts` row and the one-release legacy-file
migration are in [remote-access.md](../../orbit-setup/references/remote-access.md).

## Capability map

| Need | MCP / registered tool | CLI administration |
|---|---|---|
| Workspace discovery | `orbit_workspace_list`; `include: ["crews"]` adds each workspace's configured crews (or `crews_error`) | `orbit workspace list/show`; `orbit config show` |
| Task create/read/update | `orbit_task_add/list/show/update` | Registered `orbit.task.*` tools preserve agent attribution; lifecycle writes use `orbit.task.update` with `status` |
| Pick up work without collisions | `orbit_task_eligible` (read only; `explain: true` names each blocking selector and holder) | `orbit task eligible`: backlog/proposed tasks whose lock surface overlaps no in-progress or review task. Lock overlap is the only test; check dependencies yourself |
| Task attachments | `orbit_task_artifact_put`, `orbit_task_artifact_get` | Task artifact commands; source path is on the executing host and must resolve inside the workspace checkout |
| Retrieval | `orbit_search` | `orbit search`; `orbit search reindex` rebuilds the index |
| Agent scorecards | `orbit_agents_scorecard` and `orbit_agents_show` (`role`: `orchestrators`, `implementers`, `reviewers`, or `pilots`) | `orbit agents scorecard --role <role>` and `orbit agents show --role <role> --crew <crew>`. `orbit.orchestrator.scorecard` and `orbit.orchestrator.show` stay advertised as deprecated aliases of the orchestrators role and are removed in the next release |
| Codebase risk | `orbit_codebase_analytics`, `orbit_codebase_show`, and `orbit_codebase_related` | `orbit codebase analytics`, `orbit codebase show <path>`, and `orbit codebase related <path>`. The analytics and show metrics, attribution, filters, and response fields are described below. Regression-risk prompt marks count declared footprints; related paths are landed co-change |
| Friction | `orbit_friction_add/update`; list with `orbit_search` `kind: "friction"` and no `query`; move to the owning workspace with `update` `rehome_to` | `orbit friction list`, `rehome`, and additional show/stats/tags/resolve commands |
| Submit explicit tasks | `orbit_workflow_ship` (review-only; no completion input) | `orbit run ship`, `run auto` (`--auto-fix` also has an operator-only dashboard checkbox; no MCP auto-fix start input) |
| Forecast mode for a local drain | `orbit_workflow_auto` `action: "start"` accepts `auto_concurrency: "off"`, `"shadow"` or `"enforce"`; omission follows the configured host mode each admission pass. Pull drains refuse this override and use the executor host mode. | `orbit run auto --auto-concurrency <MODE>`; omission has the same live host default. Explicit overrides and inheritance source (`explicit` or `configured`) survive restart/resume without writing config; host thresholds and provider-limit policy refresh next pass, caching unchanged config by mtime and retaining the previous policies with a warning on invalid config; run show and readiness expose `mode`, `source` and the current `configured_mode`. This selects forecast policy only: normal operator authority, worker ceiling and host thresholds still apply. |
| Submit a catalog job | `orbit_pipeline_invoke` (`job_name` plus `input`, or operator-only `default_input: true` with `workspace`). Returns a run ID; a managed run needs a host-authorized child admission. No MCP wait — an operator session polls `orbit_workflow_run_show` (operator authority; denied for agent sessions); a session with a shell on the owning host uses `orbit run show <run-id>` instead | `orbit run job <id>` (`--wait` blocks). `orbit.pipeline.wait` is CLI-only |
| Observe/resume workflows | Operator-only `orbit_workflow_run_show/list/resume` (denied for agent sessions; a session with a shell on the owning host uses `orbit run show/history/events/trace/logs/cancel`); resize a live auto drain with operator-only `orbit_workflow_auto` `action: "resize"`; its `start` takes `approve_proposed: true` for the same per-pass approval as `run auto --approve-proposed`, which never approves a task tagged `no-auto-approve` | `orbit run show/history/events/trace/logs/cancel`; `orbit run concurrency`; `orbit run extend`; job replay/resume |
| Delivery evidence | `orbit_task_show` with `field: "delivery"` alone and optional `run_id` (read only; needs no operator authority) | What one delivery run committed and landed for one task of this workspace, read only from the host's commit and merge step records: typed status, base/head and landed SHAs, PR number, timestamps and provenance. Missing, inconsistent or foreign evidence is `unavailable` with a reason, never inferred; a local fast-forward records no landed SHA. Without `run_id` it reads the newest task-delivery run submitted with the task. Inherited commits require a bounded (64-parent), same-workspace/job/submitted-task retry chain and matching deterministic producer checkpoints/definitions; `provenance.source_run_id` and the source timestamp retain the original commit, while a resumed landing requires its own current host evidence. Execution identity/time can come from bounded (256-event) host step lifecycle audit metadata when the run record has no activity steps; audit metadata never supplies commit/landing bytes. Refreshed candidate heads never replace `commit.head_sha`. Full run details stay on operator-only `orbit_workflow_run_show` |
| Auto-tasks | `orbit_auto_task_add/list/update/mint`; `update` `enabled` enables or disables a definition | Those four are also CLI commands. `toggle`, `delete`, `show`, `restore`, `recover`, and `reset` are not on MCP (`orbit auto-task`, or `orbit tool run`); `delete` and `reset` need operator authority |
| Routine enablement | `orbit_routine_control` with `action: "list"` or `"toggle"`; operator authority and explicit `workspace` required | `orbit routine list/show` reads definitions and state. MCP toggle needs the observed `expected_enabled` and `target` plus desired `enabled`; after a lost reply, observe state before retrying. `orbit routine pause/resume` controls the separate host-local pause. |
| Review recovery | `orbit_task_reconcile_review` (`inspect`, `submit`, `status`, `accept_baseline`) and `orbit_task_review_reset`; operator-only and unavailable to managed runs | `orbit task reconcile-review` and `orbit task review-reset`. Reconciliation judges a recovered delivery's changed merged head; reset closes an open attempt and renews one selected lineage budget with a reason, preserving history. Inspect authoritative state after a lost reply. |
| Blocked-task requeue | `orbit_task_requeue` (`id`, `reason`, optional `discard_candidate`); operator-only and unavailable to managed runs | `orbit task requeue <id> --reason <why> [--discard-candidate]`. Returns a blocked task whose run has stopped to the backlog so its next run resumes the candidate and pull request its failure handoff published, after re-authenticating that publication, or the latest candidate a claim of the task kept, which a later claim that ended without one does not replace (`resumes.source: claim`); a live run or unsettled claim is refused. A changed task specification is refused; `discard_candidate` closes the task's undelivered Orbit pull requests (as `orbit task update --status backlog --discard-candidate` does) and reruns fresh. Nothing is written on a refusal. |
| Human decision requests | `orbit.decision.list` and `orbit.decision.show` (read only; an agent may see a request, never answer it or set `auto` on a rule). `orbit.task.show` with `fields: ["open_decisions"]` lists one task's open requests. A current rule the operator marked `auto` may answer an exact topic match and records that answer as precedent | `orbit decision list` and `orbit decision show` (read). `orbit decision answer <id> <answer> [--note]` is operator-only, from the CLI or the dashboard. `orbit decision reverse <id>` re-opens a precedent answer. `orbit decision rule-auto <rule> <on\|off>` is the only way to set `auto`, and it is operator-only. `orbit decision consolidate` prints a rule proposal and writes nothing. `orbit decision submit` and `orbit decision channel` manage registered decision channels |
| Experiments | `orbit.experiment.show`, `orbit.experiment.list` and `orbit.experiment.readout` (read only; an agent may see an experiment's arms, event log and per-arm outcomes, never change it) | `orbit experiment show <id>`, `orbit experiment list` and `orbit experiment readout <id> [--metric <name>]` (read). `create`, `update`, `clone`, `start`, `pause`, `resume`, `reweight`, `conclude` and `abandon` are operator-only, as is the CLI-only `orbit experiment delete`, which removes a finished experiment that applied no live treatment. `conclude` stores the readout at that moment on the conclusion event and, when an arm is chosen, files one proposed task that names the experiment, arm, verdict and overlay. It changes no configuration or repository file. |
| Crew ladder | `orbit.crew.ladder` (read only) | `orbit crew ladder [--json]`: per complexity tier, the pool, floor, candidate rung and the `system:crew-ladder` shadow trial with its would-be count, or why the tier has none. Trials are created, started and concluded at delivery admission, never by this read. |
| Condition explain | `orbit.automation.explain` (read only; `definition`, optional `subject`) | `orbit automation explain <definition> [--subject <key>] [--json]`: a routine's or auto-task's condition over live state with its scope, prefilter, ownership verdict, up to 20 matches (with arm state and remaining hold) and closest misses, one subject's clause tree with operand values, and its durable tracking: stall, last pass and skip, recent fires with their upstream chain, and `self`. Writes nothing. `job:<name>` shows a job's auto-task steps instead: outer bound, revision pin, each step's resolved target auto-task (digest, enabled, own condition guard) and its waits and step reads; it evaluates no wait. An enabled job's executions run detached and park while a step waits. |
| Condition stall clear | `orbit.automation.clear_stall` is operator-only; agents report a stalled condition instead | `orbit automation clear-stall <definition> --reason <text>`: clears `match_storm`, `too_many_matches`, `loop_suspected` or `oscillating` on the workspace owner, audited. |
| Host commands | `orbit_command_exec` when advertised and authorized | Explicit argv and an absolute working directory inside the selected workspace checkout (or a linked worktree under `.orbit/state/worktrees/`); never a shell string |
| Host agent invocation | Deprecated; opt-in `agent-invoke` MCP toolset | `orbit run agent <prompt>` still works and warns; dispatch work through tasks and `orbit run ship`, or run your provider's CLI directly for a one-off question |
| Distributed drain | Internal runtime protocol; no ordinary MCP tools | `orbit run auto --pull` uses a launch-selected owner route for probe, receipt lookup, task admission, bind and settle. These five operations have no public schemas, and public calls refuse both canonical and formerly advertised names; client names or initialize metadata cannot enable the route. Matching internal protocol support is required on both endpoints, with no public fallback. Use owner-side `orbit tool run orbit.drain.probe`, `orbit.drain.receipt.lookup` and `orbit.drain.claims` for supported diagnostics under the required identified/operator authority. Do not call pull, bind or settle by hand: admission and claim mutations retain machine/run fences. Handoff approval is an owner-dashboard action with no CLI verb or tool. Handoff revocation and claim recovery are operator-only: `orbit task revoke-handoff` and `orbit task recover-claim`, with the tools `orbit.task.revoke_handoff` and `orbit.task.recover_claim`, which managed runs cannot call. See [distributed-drain.md](setup/distributed-drain.md). |
| Setup and maintenance | Discover any server extensions; do not guess | config, doctor, search reindex, audit, GC, filesystem profiles, skill, routine, sweep, job/activity catalogs, workspace role/sync/publication |

### Codebase analytics and path detail

Use `orbit_codebase_analytics` to compare repository paths across the codebase
risk rankings. Pass a `path` returned in a ranking row to
`orbit_codebase_show` to inspect that path's standing in every ranking and
follow its counts to tasks, regression sites, lock waiters, or coupled paths.
Both tools use the same selection: `window` (`7d`, `30d`, `90d`, or `all`,
default `90d`), `min_support` (default `15`: minimum denominator for the rate
rankings, minimum shared-reservation count for change coupling) and `rollup`
(`file`, parent `dir`, or `crate`, default `file`). `touched` means the task's
declared footprint (`context_files`, including widened selectors), not the
landed diff.

Analytics returns six path rankings and an `overlap` summary. Each rate row
includes its numerator and denominator. Rankings use distinct tasks as their
counts, except lock contention, which counts distinct requesting identities,
and change coupling, which counts reservations. The rate rankings keep rows
with a positive numerator and at least `min_support` denominator support, are
sorted by rate then numerator then path, and analytics returns at most 25 per
ranking; `blocked` additionally keeps a supported row whose attributed
numerator is zero when the path has unclassified blocks, ranking at rate zero
so that bucket stays visible per path. Change coupling instead keeps pairs
with at least `min_support` reservations naming both paths, regardless of the
Jaccard `either` denominator, sorted the same way with the shared count
standing in for the numerator. `below_support` counts rows omitted for low
support (pairs below shared-count support for coupling). For the rate
rankings, `show` reports the path's 1-based rank over the full ranking and
whether its denominator is below support; a missing ranking row is `null`.

The ranking sections mean:

- `regressions`: done tasks with an incoming regression link whose recorded
  site attributes the task to the path, divided by done tasks whose declared
  footprint touched it. A task attributed by a site outside its footprint is
  added to that path's denominator, so its rate stays at most 1. Recorded
  directory sites cover their descendant keys. The response includes task
  counts, sites, and the link breakdowns described below.
- `footprint_misses`: tasks whose declared footprint was widened onto the
  path during the window, divided by active tasks whose footprint touched it.
  `widenings` counts widening events, while the rate counts each task once.
- `blocked`: tasks blocked by a run failure or recorded base conflict on the
  path, divided by active tasks whose footprint touched it (plus any
  conflicting paths outside that footprint). Unclassified blocks are reported
  separately, including per-path counts; infrastructure, manual,
  base-conflict, run-failure, and non-failure totals are also exposed.
- `rejected`: tasks rejected after a job run, divided by tasks with a run
  whose footprint touched the path. Proposal-stage rejections without a run
  are excluded.
- `lock_contention`: distinct requesters refused a lock on the path divided
  by distinct requesters with a reservation or refusal there. The response
  includes reservation and denial counts, wait summaries, and a second list
  ranked by total wait time.
- `change_coupling`: pairs of paths reserved together, ranked by Jaccard rate
  (`reservations naming both ÷ reservations naming either`). Pair support is
  the number of reservations naming both, and `min_support` gates that shared
  count rather than the `either` denominator. Reservations covering more than
  12 files are excluded.

`overlap` lists paths appearing in at least two of the first 25 rows of
`regressions`, `footprint_misses`, `blocked`, and `rejected`, plus each path's
rankings and its highest non-rejection rate. `ranked_paths` is the number of
distinct paths across those four lists.

Regression attribution and filters apply to `regressions`, its path detail,
and the crew and complexity rates. A link's `nature` and `escape` come from
its recorded classification; a missing classification is `absent`, distinct
from an explicit `unclassified`. `layer` describes where the fix landed,
using landed paths or, when unavailable, its declared footprint. `detected_by`
comes from the regression task's tags or its filer. Filters `nature`,
`escape`, `layer`, and `detected_by` combine: every selected dimension must
match. `absent` selects links without a recorded classification.

The regression section's `breakdowns` count links by nature, escape, layer,
and detector while retaining filters on the other dimensions. `cross_tabs`
contains link counts for nature × escape and escape × layer under the full
filter. `by_crew` and `by_complexity` are rates of done tasks with a matching
regression link within each crew or complexity group; missing values appear
as `unassigned` and `unset`. Each path row also has per-crew numerator,
denominator, and rate. A regressed task entry includes its crew, complexity,
regression task IDs, matching regression links, and sites attributing it to
that path. `unsited` counts regressed tasks and links whose kept links have no
valid recorded site; they are not attributed across the task's footprint.

In `show`, each task-rate ranking entry carries its rate, numerator,
denominator, rank, and support status, plus the evidence behind its numerator.
The regression detail lists the recorded site selectors under the path and
their regressed and regression tasks. Its `unsited` task count is limited to
regressed tasks that declared the path but have no site attributing them to
any path. Lock detail lists refused requesters and wait episodes.
`change_coupling` reports the path's reservation count and ranked pairs; each
pair includes its Jaccard rate, rank, reservation counts for both paths, and
requesters. The same four regression filters narrow regression counts and
drill-downs; they do not change the other rankings.

`orbit_workflow_run_show` and each `orbit_workflow_run_list` item are
operator-only (denied for agent sessions) and carry the same run projection
as `.run` in `orbit run show --json` and the dashboard; a session with a
shell on the owning host reads that projection with `orbit run show --json`:
state and timing, `error_code`/`error_message` from the step that explains the
outcome, `root_cause` for a run that failed only because a child did,
`run_role`, crew, `trigger`, `child_dispatches` and drain controls. They add
the submitted `input`, the worker's `pid`/`pid_start_time`, `drain_last_pass`,
`drain_approvals` and `recovery_attempts`; `show` also adds
`activity_provenance` and `execution_progress`. `steps` are the steps every
run view displays, and `steps_source` says whether they came from the run
record or, when the record is empty or holds only a run-level step, its audit
trail. A step still open when its run ended reads as the run's terminal state
with `outcome: "interrupted"`, never `running`.

Provider/gateway prefixes are transport wrappers around these names. A connected
server may expose additional tools; use its advertised schema rather than
assuming every installation has that extension.

Bare `orbit mcp serve` and ordinary `orbit mcp init` integrations have agent
capability. `orbit workspace init --mcp` deliberately installs an operator
integration. Governed workflow and command operations need operator authority;
a managed worker cannot dispatch/resume another workflow or use operator command
execution. An allowlisted tool still has to pass runtime policy, filesystem,
subprocess, and external authentication checks.

### MCP toolsets

`orbit mcp serve` and `orbit mcp listen` advertise the `default` toolset plus
the opt-in toolsets the host enables: `leases` (path leases), `experiments`
(experiment tools), `analytics` (agent scorecards, orchestrator scorecard and
show, crew ladder), `decision-channel` (decision submit and digest; decision
list and show stay default) and `agent-invoke` (`orbit.agent.invoke`). Enable them per server with
`--toolset leases,analytics` (or `all`), or for the host with
`orbit config set --global machine.mcp.toolsets '["experiments"]'`; the flag
replaces the config. A call to a tool outside the server's toolsets is refused
`toolset_not_enabled`, naming the tool's `toolset` and the `enable` flag and
config key. A federated mux filters every row the same way. The toolset of each
tool is in `orbit tool show <name> --json` (`mcp_toolset`). A managed run
offered explorer delegation gets `agent-invoke` added to its own server: the
dispatch stamps it in `ORBIT_ACTIVITY_MCP_TOOLSETS`, which adds to the
server's selection.

Toolsets change only what an MCP server lists. `orbit tool run`, CLI commands,
activity `required_tools` and task grants, and a decision-channel plugin's
callback session reach every tool regardless.

### Host agent invocation

**Deprecated for operators.** The operator path will be removed: it is in the
opt-in `agent-invoke` toolset, which a default server does not list, and
`orbit run agent` warns on every submission. Dispatch work through tasks and
`orbit run ship`, or run your provider's CLI directly for a one-off question.
A managed run offered read-only explorers (below) gets the toolset on its own
server.

`orbit_agent_invoke` submits one asynchronous agent run for exploration or
debugging and returns its run ID. It is the surface for a question that cannot
be answered from inside a managed run's sandbox — why a host is behaving the
way it is.

Be clear-eyed about what it does. The invoked agent runs **outside Orbit's
filesystem sandbox**, as the same operating-system user as Orbit, so it can read
and write anything that user can. Withholding capabilities from the child is not
an isolation boundary, and neither is the activity's declared program list: the
provider harness owns its own shell tool. Treat an invocation the way you would
treat running a command yourself on that machine.

What it is not:

- It is **not** a task, and it performs no task transition. It does not commit,
  push, open or merge a pull request, or dispatch further work.
- It is **not** a trusted-host invocation for a managed run. Any operator session may admit one,
  local or arriving over SSH: a remote caller reached this machine through an
  SSH login that already lets it start any process it likes, so `operator` is
  the whole test. Start the calling federated or remote-proxy server with
  `--operator` and the invocation works; a session served as `agent` is
  refused. Each admission covers one invocation only.
- It is **not** resumable. A resumed run would carry an admission nobody granted
  now; submit a new invocation instead.

**Read-only explorers.** A managed `agent_implement` or `agent_review_repair`
run may call the same tool when `workflow.explore.enabled` is on and
`workflow.purpose_crews.explorer` has an eligible member. That call gets a
different, narrower run: an `agent_explore_pipeline` explorer drawn from the
explorer pool and sandboxed with no writes, starting in a `cwd` inside the
caller's own worktree, with every task, lifecycle, dispatch, commit and
invocation tool denied. Codex uses its read-only inner sandbox when Orbit's
executor wrapper is off; a provider with no read-only mode is refused before
it starts unless that wrapper is on. It never gets a
trusted-host admission. The answer adds `paths_read`. Per-run call and
concurrency budgets, a timeout ceiling and an answer-size cap apply; each
refusal names its reason and setting, and the caller continues without
delegating. Every other managed run is refused. See
[CONFIG.md](https://github.com/constellation-works/orbit/blob/main/docs/CONFIG.md#workflowexplore--read-only-exploration-for-implementing-agents).

Required arguments are the `prompt` and an absolute `cwd` inside the workspace's
checkout or a linked worktree under `.orbit/state/worktrees/`. `crew` selects the provider/model, `timeout_seconds` bounds the run
(default 1800, enforced ceiling 7200, excluding queue time), and `idempotency_key` makes a resubmission resolve
the run the first attempt created rather than starting a second agent.

The CLI accepts `--wait` to block until terminal and print the same `answer`
projection; a failed, timed-out, cancelled or interrupted run exits nonzero.
`--timeout` bounds provider execution, accepts seconds or durations such as
`30m` and `2h`, and excludes queue time. MCP `wait_seconds` is an optional
integer from 0 to 600: a finished run returns `answer` and `agent_invocation`;
an unfinished one returns its run ID and actual state for later observation.
The wait deadline never cancels or changes the invocation outcome.

Submissions report `queued` and `queue_position` (one-based among waiting
runs, null if runnable) plus a warning when the concurrency limit is saturated.
These describe admission time; the run may start before the response arrives.
The shipped job retains eight concurrent provider processes as a host resource
guard because these processes have no executor sandbox or memory budget.

`orbit run logs <RUN_ID> --follow` streams retained redacted provider tracing
lines and stops at any terminal outcome. JSON modes emit JSONL records
`{run_id, provider, stream, text}`. Completed captures supply output not yet
streamed; with `--step`, each capture is emitted when its invocation finishes.
Provider lines are retained in the JSONL feed by default while stderr diagnostics
stay at WARN. Explicit `RUST_LOG` and tracing retention limit live history;
if the live feed differs from the capture, follow mode replays that capture with
a diagnostic (some lines may repeat). Use the ordinary
`orbit run logs` command to read durable captures. Ctrl-C ends observation
without cancelling the run.

`provider_sandbox` is an optional per-invocation override of the provider's own
inner sandbox (not Orbit's executor sandbox — that is already off, reported as
`sandboxed: false`). Pass a mode the provider accepts, such as Codex
`read-only` or `workspace-write`, to run an exploration tighter than the crew
default without editing config. Values the provider does not support are
refused. The submission result and `orbit run show` report the effective mode
as `provider:mode` (for example `codex:danger-full-access`, `claude:default`).
When that mode is the provider's least-restrictive inner sandbox
(`danger-full-access` for Codex), the result includes a `warnings` entry and
Orbit logs the same at WARN: the provider may use host integrations (browser,
computer use, …) beyond the working directory.

An operator session tracks it with the ordinary run surfaces —
`orbit_workflow_run_show` (operator authority; denied for agent sessions), or
`orbit run show|logs|cancel <RUN_ID>`; a non-operator session with a shell on
the owning host uses `orbit run show|logs|cancel <RUN_ID>`. The full MCP response puts
`agent_invocation` at the top level; `view: "bounded"` and
`orbit run show --json` put it at `.run.agent_invocation`:

- `answer` is the result: `summary`, `findings`, `next_steps`, every other
  field the agent put in its envelope `result` under `extra` (a
  `report_markdown`, for instance), and a bounded `final_message` where the
  surface includes it. The full MCP response omits `answer.final_message` and
  the raw `preview` once an answer exists, while retaining
  `final_message_blob_ref` and `stdout_blob_ref`; use `orbit run logs <RUN_ID>`
  to read the complete captured output.
- `progress` is what the agent is doing while it runs: its newest
  `latest_message` and `last_activity_at`, sampled about every ten seconds
  while it writes output. A provider that prints nothing until it exits shows
  none.
- `outcome`, `failure_reason`, `completed_envelope`, the effective
  `provider_sandbox`, and `stdout_blob_ref`, the durable reference to the full
  captured output — present for a failed invocation too.

The response envelope is required. A provider that exits zero without
terminating it stopped mid-turn, and one whose envelope has no `result` object
returned no answer: either way the run records `failed` with a
`failure_reason` naming why, and the exit code alone is never evidence the
investigation succeeded.

A session that arrived over SSH is admitted on the same terms as a local one.
The durable admission and `trusted_host.execution_admitted` event retain the
forwarded caller machine ID — attribution, not a grant — plus the workspace
checkout and cwd. See [remote-access.md](../../orbit-setup/references/remote-access.md). Do not
relaunch a server with more privileges to work around a denied call.

### Pilot-warning triage

`orbit_task_block_triage` (`orbit.task.block_triage`) serves pilot-warning
triage only. A blocked task is decided by final recovery, which opens a decision
request an operator answers, so `subject: block` is refused with a message
naming final recovery. The default subject is `pilot_warning`: the actions read
`proposed` tasks whose latest task-pilot assessment holds promotion on warning
findings.

`candidates` lists each held assessment that is not yet triaged and has no
operator decision, quoting its holding findings. `propose` opens a decision card
(a summary, two or three options with their consequences, and a recommendation;
an option approves the task, re-pilots it or rewrites a field) and records one
comment quoting those findings plus a `pilot_warning_triaged` history marker,
which excludes that assessment afterwards. `report` records the human action the
same way. Both refuse a task that no longer qualifies. `requeue`, `resume_run`,
`rerun_ci` and `friction_reported` are refused. A pilot-warning triage never
approves, promotes, clears the hold or edits any other task field.

Inspection proof: a decision on a task that a live escalation triage run holds
must carry that run's proof, in three fields:

- `source_revision`: the commit the run pinned.
- `observed_revision`: `git rev-parse HEAD` in the run's inspection checkout.
- `observed_tree`: `git rev-parse 'HEAD^{tree}'` in the same checkout.

Core checks that `observed_revision` equals the pinned revision and that
`observed_tree` equals that revision's tree. In a non-Git workspace the run pins
no revision: pass `source_revision` as null and omit both observed values. A
missing or mismatched proof is refused, naming the run and the expected values,
and nothing is recorded. A proof passed on any other decision is checked the same
way.

### Decision requests

`orbit_decision_submit` (`orbit.decision.submit`) and `orbit_decision_digest`
(`orbit.decision.digest`) are governed. Over MCP they are in the `decision-channel`
toolset. Both are callable only by the plugin the
channel is bound to, through its callback session, or by the operator; an agent
session is refused `caller_not_allowed`.

**Submit.** Core verifies that the channel is registered and unrevoked, that the
request is still open against its binding, that the answer is allowed, and the
per-request token: single-use, expiring with the request, and keyed to the
channel's current key epoch. A submission with `raw_message` also gets core's own
DKIM check: core reads the selector key from DNS, verifies the body hash and the
header signature, checks signing-domain alignment with the sender, and refuses
when its result contradicts the forwarded verdicts. A channel registered with
`require_dkim_verification` refuses a submission without a raw message
(`dkim_verification_required`). Only then is the answer applied as the registered
human, with the channel and the token fingerprint recorded as provenance.

A refusal applies nothing. The refusal codes are `channel_not_registered`,
`channel_revoked`, `caller_not_allowed`, `sender_not_allowed`,
`authentication_failed`, `dkim_verification_failed`,
`dkim_verification_unavailable`, `dkim_verification_required`,
`raw_message_too_large`, `token_invalid`, `token_consumed`, `request_expired`,
`request_superseded`, `answer_not_allowed`, `answer_not_applicable`,
`request_not_found` and `decision_channel_disabled`. A parse-level
refusal does not consume the token. `dkim_verification_unavailable` is the only
retryable refusal: retry the same submission unchanged; it consumes nothing.

**Digest.** `orbit_decision_digest` takes one `channel`. For every proposed task
without an open `proposed_approval` request, and every review task without an
open `review_approval` request, it raises one open request per (task, gate,
binding), with answers `approve` and `reject` and `raised_by` `digest`. A plugin
the channel is not bound to is refused `channel_not_registered`; an unregistered
or revoked channel is refused with `channel_not_registered` or `channel_revoked`.

- `proposed_approval` binds to the task's description and acceptance criteria
  plus its proposed status.
- `review_approval` also binds to the candidate under review.
- Either request reads `superseded` once its binding stops matching.

The result carries `requests` (the digest), `raised` (what this call raised) and
`expiring` (every still-open request expiring within 24 hours). Repeated calls
reuse requests for unchanged task bindings, while newly eligible tasks can add
requests. With the decision channel disabled in this workspace, the call raises
nothing, writes nothing and returns no requests.

### Raising a decision

`orbit_decision_raise` (`orbit.decision.raise`) is the one decision tool an agent
session writes with. It raises a multiple-choice request on a task assigned to a
human crew that is not `done`, `archived` or `rejected`. Inputs:

- `task_id` and `question`: the decision needed, in one or two plain sentences.
- `options`: two to four `{label, consequence, recommended}` objects. `label` and
  `consequence` are one line each, and exactly one option has
  `recommended: true`; it is listed first as answer `1`, the rest follow as `2`
  to `4`.
- `topic` (optional): a structured token of 1-64 characters (letters, digits and
  `. _ : / -`), or a corpus rule id. `orbit_decision_list` filters by exact
  `topic`.
- `model`: your agent family. It names the filer the per-filer cap counts.

The request has gate `agent_request` and `raised_by` `agent_request`. It is
bounded: the same open question (same text ignoring case and spacing, same
topic) returns the existing request with `deduplicated: true` and writes nothing;
a task holds one open request; and open agent-raised requests are capped at 20
per workspace and 5 per filer. Past a cap, or on a task that is not a human
crew's, the call is refused with the reason and nothing is written; answering a
request frees its slot.

Answering stays the operator's (`orbit decision answer`, the dashboard or a
registered channel), except when the request's `topic` names a `file/rule`, or a
rule id that appears in exactly one decision file, and that rule is a current
corpus rule the operator marked `auto` with an option you offered. That match
answers immediately. The history actor is `precedent`, the record names the rule id and the rule's citations, and it
states that the operator approved the rule. A comment tells the operator how
to reverse it with `orbit decision reverse`. Reversal re-opens the request and
restores the task status. An agent label cannot answer, cannot reverse, and
cannot set `auto` (`orbit decision rule-auto` is operator-only). A non-auto,
stale, superseded, or ambiguous rule does not answer; neither does a topic
that is missing, only similar, or just a decision file's name. The answer is
recorded in the task's history (who, when, which option, with the option's consequence in a comment), the
task moves to `done`, and `blocked_by` dependents become eligible. Raw
decision history is not authority. Only a merged `current` rule whose review
date has not passed and whose `depends_on` digests still match may be cited.
A weekly consolidation proposes rule edits as a pull request and never edits
a rule outside that pull request; a human merge is what makes a rule current.
An answer that changes lasting behaviour or policy is carried by the task it
unblocks into the doc, config or code that owns it, in that task's PR.

### Agent scorecards

`orbit_agents_scorecard` ranks one agent role's crews and models over the window. Over
MCP it is in the `analytics` toolset.

- **Score.** Each term's count per 100 units of work is weighted and summed.
  Implementers start at 100 and score actual counts against fleet expectations
  for the same tier mix; other roles sum weighted actual rates. A row above the
  fleet average is pulled down toward it. Tasks tagged `no-diff-expected`, when
  an attempt was admitted, are not implementer work and count nowhere on the
  implementers scorecard.
- **Ranking.** Rows below the minimum support are listed last without a rank. A
  reviewer row none of whose work is judged yet is `unjudged`: no rank, null
  `raw` and `score`, listed after every other row and left out of the fleet
  average. A role whose terms are not computed yet answers
  `status: not_computed`, with its terms and no rows.
- **Switches.** `by_type` returns one cell per (crew, model, tier, type) with
  `n`, `raw`, `crew_mean` and `score`, plus the type list used. `shadow` (with
  `by_type`) returns each crew's draw count and its static, drawn and would-be
  share per pool and type over the window; draws with no performance record are
  counted separately as static. `per_task` returns one row per implemented task
  with its crew, model, tier, configured type, tags, context-file selectors, path
  prefixes and per-term counts, plus the type list used. Without these switches,
  the ranking is returned.
- **Execution.** Each task's execution is derived from its runs, never set by
  an agent: `managed` when a job (a local delivery run or a pulled claim)
  worked its final implementation attempt, `interactive` when it reached review
  or done without one, `unclassified` otherwise. Implementer counts read managed
  tasks by default; `execution: all` adds interactive tasks and
  `execution: interactive` shows them alone. Unclassified tasks count in
  neither. Cost and run time count managed tasks whatever the scope, and the
  implementers header's `execution_split` lists each crew and model's managed,
  interactive and unclassified tasks with the interactive ones' landed and
  regressed counts. Other roles ignore the scope.
- **Recorded effort.** Each ranking/detail row includes `effort_breakdown` of
  completed invocations by immutable dispatch crew, model and reasoning effort,
  with counts, linked tasks, duration, tokens and provider-reported cost.
  `null` means provider default; `"unknown"` means no dispatch record exists.
  Scores retain their crew/model population. The top-level breakdown covers
  completed workspace invocations in the selected window; `effort_population`
  identifies that separate descriptive population. Orchestrator authorship stays
  unknown because authorship does not record an execution effort. Never infer
  historical effort from the crew's current configuration or an audit event.
  The ladder's `effort_breakdown` covers 90 days (`effort_window_days`);
  experiment arms break down invocations linked to their enrolled tasks.
  Run show exposes `agent_executions` on the run and displayed steps, captured
  before launch so interrupted runs remain inspectable.
- **Cost.** Each call scans the workspace once (`cache_ttl_seconds` is 0), except
  the `shadow` report, which reads recorded draws only.
- The dashboard's Agents view reads this payload.

## Common MCP arguments

These are JSON arguments to the named tool, not shell commands:

```json
{"workspace":"<selector>","query":"<problem terms>","kind":"task","limit":5,"model":"<agent-family>"}
```

Use with `orbit_search` before filing a task. Search closed history as well with
`all: true` when looking for already-delivered work.

```json
{"workspace":"<selector>","id":"<task-id>","fields":["status","description","acceptance_criteria","execution_summary"],"model":"<agent-family>"}
```

Use with `orbit_task_show`. A task title or an agent's narrative is not completion
evidence; inspect status and the recorded implementation/validation outcome.

```json
{"workspace":"<selector>","task_ids":["<task-id>"],"mode":"pr","base":"<integration-branch>","allowed_crews":["<configured-crew>"],"model":"<agent-family>"}
```

Use with `orbit_workflow_ship` only when execution is authorized. At least one
explicit ID is required; MCP does not offer the CLI's no-ID discovery mode.
It also intentionally accepts no completion authorization, so it always
submits review-only work. When an authorized operator has access to the owning
host, use `orbit run ship <task-id> --complete` there for a one-run completion
authorization; do not use a local shadow store as a substitute. Otherwise
report the MCP completion capability gap rather than inventing a `completion`
argument.
A ship receipt, like an `orbit_workflow_auto` start, carries the notices the
CLI and the dashboard report for the same submission: `warning` (one line per
notice: an explicit ship's host resource throttle, a validation environment
that may not find the toolchain, unset `execution.env.pass` names) and
`validation_env_warning`. A drain start also carries `resource_throttle`, a
`warning` line naming backlog tagged for another OS, and a `note` when no
required validation is configured. None of them refuses the submission.
An operator session reads the returned run with `orbit_workflow_run_show`
using `id` and `workspace` (operator authority; denied for agent sessions);
a session with a shell on the owning host uses `orbit run show <run-id> --json`.
An operator session lists bounded history with `limit`, `job_id`, `state`
(including `terminal`), and RFC3339 `since` (`orbit_workflow_run_list`);
a session with a shell on the owning host uses `orbit run history --json`.
Submission success means a durable run exists, not that the
change landed. An operator session resumes with `orbit_workflow_run_resume`;
it returns a new linked run; preserve both IDs.

## Path leases

`orbit.lease.request`, `.renew`, `.expand`, `.release`, `.show`, `.list`,
`.check` and `.notices` are `orbit tool run` tools, and MCP tools (advertised as
`orbit_lease_request`, etc.) on a server that enables the `leases` toolset. They all answer for the owner's coordination store and
require an identified agent, operator or runner. Federated calls must target
the owner; replicas refuse the `control_plane` class, including lease reads.
Enable `lease.enabled` in the workspace first.

Request takes `principal`, `selectors` (`file:...` or `dir:...`), optional
`mode` (`advisory` or `hard`), `priority` (`low`, `normal`, `high`),
`ttl_seconds` (1–14400; default 1800), `wait` (default true), tags and
branch/machine attribution. It returns `granted` and a `lease` with `lease_id`,
mode, principal, priority and expiry. Advisory never blocks; a hard request
overlapping task locks, reservations or other hard leases returns
`granted: false, queued: true` with the holders and `queue_position`, and its
lease (state `queued`) holds nothing until a `granted` notice; `wait: false`
returns `queued: false` and the holders instead. The queue orders by priority,
then age, and `lease.aging_minutes` lifts a long waiter one level. A lease
never preempts an in-progress or review task. Workspace
policy may downgrade hard to advisory or refuse priority above the caller's
ceiling. Over SSH, principal/class policy is attribution and a guard.

Renew, release and show take `lease_id`; renew optionally takes `ttl_seconds`.
Expand takes `lease_id` and `selectors`: free paths are `added` at once, and
on a hard lease blocked ones are `queued` while the lease keeps what it holds.
A mutual expansion wait refuses the expansion ranked last in queue order
(lowest effective priority, then youngest) with a `deadlock` notice
(`deadlock` is set in that caller's answer).
List returns active leases, queued ones included. Check takes `paths` and optional `lease_id` and
reports coverage, unlocked policy and overlapping holders without mutations.
Show and list expire stale leases and can write expiry audit events.

The CLI is `orbit lease request <selector>... --principal <name>
[--mode advisory|hard | --hard] [--priority low|normal|high] [--ttl 30m]
[--no-wait]`, `renew <lease-id> [--ttl 30m]`, `expand <lease-id> <selector>...`,
`release <lease-id>`, `show <lease-id>`, `list`, and
`check <path>... [--lease-id <lease-id>]`. All support `--json`.
A refused or queued request, and a deadlock-refused expansion, exit 3. `orbit task locks list` shows leases alongside
reservations with mode and principal; task/reservation counts retain their
existing meaning, with `by_lease` and `total_leases` added separately; a queued request holds
nothing and is not listed there. Preemption belongs to a later phase.

## Missing operations

A CLI-only operation is not a nonexistent product feature. It requires process
access on the owning host, within the user's authority. Where the session
requires all durable operations through a particular MCP connection, stop and
report a missing operation instead of using CLI, SQLite, HTTP, or another MCP
server as a fallback. Read-only source inspection remains separate from durable
control-plane operations.
