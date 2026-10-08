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
`artifact.get`, `artifact.put`, `review_reset` and `reconcile_review` (each
under `orbit.task.`). Routing applies only where that tool is advertised and
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
| Friction | `orbit_friction_add/update`; list with `orbit_search` `kind: "friction"` and no `query`; move to the owning workspace with `update` `rehome_to` | `orbit friction list`, `rehome`, and additional show/stats/tags/resolve commands |
| Submit explicit tasks | `orbit_workflow_ship` (review-only; no completion input) | `orbit run ship`, `run auto` |
| Observe/resume workflows | `orbit_workflow_run_show/list/resume`; resize a live auto drain with operator-only `orbit_workflow_auto` `action: "resize"`; its `start` takes `approve_proposed: true` for the same per-pass approval as `run auto --approve-proposed`, which never approves a task tagged `no-auto-approve` | `orbit run show/history/events/trace/logs/cancel`; `orbit run concurrency`; job replay/resume |
| Delivery evidence | `orbit_task_show` with `field: "delivery"` alone and optional `run_id` (read only; needs no operator authority) | What one delivery run committed and landed for one task of this workspace, read only from the host's commit and merge step records: typed status, base/head and landed SHAs, PR number, timestamps and provenance. Missing, inconsistent or foreign evidence is `unavailable` with a reason, never inferred; a local fast-forward records no landed SHA. Without `run_id` it reads the newest task-delivery run submitted with the task. Full run details stay on operator-only `orbit_workflow_run_show` |
| Auto-tasks | `orbit_auto_task_add/list/update/mint`; `update` `enabled` enables or disables a definition | Those four are also CLI commands. `toggle`, `delete`, `show`, `restore`, `recover`, and `reset` are CLI-only (`orbit auto-task`) |
| Routine enablement | `orbit_routine_control` with `action: "list"` or `"toggle"`; operator authority and explicit `workspace` required | `orbit routine list/show` reads definitions and state. MCP toggle needs the observed `expected_enabled` and `target` plus desired `enabled`; after a lost reply, observe state before retrying. `orbit routine pause/resume` controls the separate host-local pause. |
| Review recovery | `orbit_task_reconcile_review` (`inspect`, `submit`, `status`, `accept_baseline`) and `orbit_task_review_reset`; operator-only and unavailable to managed runs | `orbit task reconcile-review` and `orbit task review-reset`. Reconciliation judges a recovered delivery's changed merged head; reset closes an open attempt and renews one selected lineage budget with a reason, preserving history. Inspect authoritative state after a lost reply. |
| Host commands | `orbit_command_exec` when advertised and authorized | Explicit argv and an absolute working directory inside the selected workspace checkout (or a linked worktree under `.orbit/state/worktrees/`); never a shell string |
| Host agent invocation | `orbit_agent_invoke` when advertised and authorized | `orbit run agent <prompt>`; returns a run ID, or the answer with `--wait` |
| Distributed drain | Internal runtime protocol; no ordinary MCP tools | `orbit run auto --pull` uses a launch-selected owner route for probe, receipt lookup, task admission, bind and settle. These five operations have no public schemas, and public calls refuse both canonical and formerly advertised names; client names or initialize metadata cannot enable the route. Matching internal protocol support is required on both endpoints, with no public fallback. Use owner-side `orbit tool run orbit.drain.probe`, `orbit.drain.receipt.lookup` and `orbit.drain.claims` for supported diagnostics under the required identified/operator authority. Do not call pull, bind or settle by hand: admission and claim mutations retain machine/run fences. Handoff approval, revocation and recovery remain owner-dashboard actions. See [distributed-drain.md](setup/distributed-drain.md). |
| Setup and maintenance | Discover any server extensions; do not guess | config, doctor, search reindex, audit, GC, filesystem profiles, skill, routine, sweep, job/activity catalogs, workspace role/sync/publication |

Provider/gateway prefixes are transport wrappers around these names. A connected
server may expose additional tools; use its advertised schema rather than
assuming every installation has that extension.

Bare `orbit mcp serve` and ordinary `orbit mcp init` integrations have agent
capability. `orbit workspace init --mcp` deliberately installs an operator
integration. Governed workflow and command operations need operator authority;
a managed worker cannot dispatch/resume another workflow or use operator command
execution. An allowlisted tool still has to pass runtime policy, filesystem,
subprocess, and external authentication checks.

### Host agent invocation

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
- It is **not** available to a managed run. Any operator session may admit one,
  local or arriving over SSH: a remote caller reached this machine through an
  SSH login that already lets it start any process it likes, so `operator` is
  the whole test. Start the calling federated or remote-proxy server with
  `--operator` and the invocation works; a session served as `agent` is
  refused. Each admission covers one invocation only.
- It is **not** resumable. A resumed run would carry an admission nobody granted
  now; submit a new invocation instead.

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

Track it with the ordinary run surfaces — `orbit_workflow_run_show`, or
`orbit run show|logs|cancel <RUN_ID>`. The full MCP response puts
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
Read the returned run with `orbit_workflow_run_show` using `id` and `workspace`.
List bounded history with `limit`, `job_id`, `state` (including `terminal`), and
RFC3339 `since`. Submission success means a durable run exists, not that the
change landed. Resume returns a new linked run; preserve both IDs.

## Missing operations

A CLI-only operation is not a nonexistent product feature. It requires process
access on the owning host, within the user's authority. Where the session
requires all durable operations through a particular MCP connection, stop and
report a missing operation instead of using CLI, SQLite, HTTP, or another MCP
server as a fallback. Read-only source inspection remains separate from durable
control-plane operations.
