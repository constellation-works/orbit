# Tools, routing, and authority

Orbit exposes a curated MCP surface and a larger CLI catalog. Use the installed
version's `tools/list`, `orbit tool list`, and command `--help`; a similar name
is not evidence that an operation exists. This skill explains the contracts
without requiring an Orbit source checkout.

## Select the store before reading or writing

Call `orbit_workspace_list({})` on the configured MCP connection first. Inspect
host, workspace, ownership, availability, and capabilities where returned.

- Direct server: pass the returned logical workspace ID as `workspace`.
  The server can also resolve registered names and paths, but IDs avoid
  ambiguity. An explicit selector overrides its session binding.
- Federated server: copy the returned `selector` exactly, including its
  `hm_…/ws_…` qualification. Bare names, bare IDs, and local paths cannot route
  federated calls. The mux is deliberately not bound to one workspace.
- A direct session can bind through `orbit mcp serve --workspace <selector>`
  or initialize metadata. An unbound session requires a per-call selector;
  server cwd never chooses the workspace.
- A managed child inherits trusted workspace and run identity from its envelope.
  Do not replace those with a root or registry from another checkout.

If a host is unavailable, report that fact. Reading a publication is explicitly
labelled snapshot access, not a substitute for live task state. Never create
records in a second store merely to get past a connection error.

## Capability map

| Need | MCP / registered tool | CLI administration |
|---|---|---|
| Workspace discovery | `orbit_workspace_list` | `orbit workspace list/show` |
| Task create/read/update/start/approve | `orbit_task_add/list/show/update/start/approve` | Registered `orbit.task.*` tools preserve agent attribution |
| Task attachments | `orbit_task_artifact_put` | Task artifact commands; source path is on the executing host and must resolve inside the workspace checkout |
| Retrieval | `orbit_search` | `orbit search`; semantic install/index is separate |
| Friction | `orbit_friction_add/list/update` | Additional show/stats/tags/resolve commands |
| Submit explicit tasks | `orbit_workflow_ship` (review-only; no completion input) | `orbit run ship`, `run auto` |
| Observe/resume workflows | `orbit_workflow_run_show/list/resume` | `orbit run show/history/events/trace/logs/cancel`; job replay/resume |
| Operation mode | CLI/dashboard only; agents read grant state via `orbit run readiness` | `orbit operation explain/enable/list/show/stop/revoke`, `orbit run auto --grant` |
| Auto-tasks | `orbit_auto_task_list/mint` | Definition add/show/update/toggle are CLI operations; do not assume they are advertised over MCP |
| Host commands | `orbit_command_exec` when advertised and authorized | Explicit argv and an absolute working directory inside the selected workspace checkout (or a linked worktree under `.orbit/state/worktrees/`); never a shell string |
| Host agent invocation | `orbit_agent_invoke` when advertised and authorized | `orbit run agent <prompt>`; asynchronous, returns a run ID |
| Setup and maintenance | Discover any server extensions; do not guess | config, doctor, semantic, docs, audit, GC, policy, skill, routine, sweep, job/activity catalogs, workspace role/sync/publication |

Provider/gateway prefixes are transport wrappers around these names. A connected
server may expose additional discovery such as crews; use its advertised schema
rather than assuming every installation has that extension.

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
- It is **not** available to a managed run. A local operator may admit one
  directly. A remote operator also needs a callers-file row that explicitly
  enables `agent_invoke` for the resolved workspace. The omitted mode requires
  a destination-issued key-bound SSH identity; an explicit `cooperative` mode
  instead trusts the existing same-OS-account SSH operator channel and records
  its machine ID as self-asserted. Ordinary remote `operator` capability is not
  enough. Each admission covers one invocation only.
- It is **not** resumable. A resumed run would carry an admission nobody granted
  now; submit a new invocation instead.

Required arguments are the `prompt` and an absolute `cwd` inside the workspace's
checkout or a linked worktree under `.orbit/state/worktrees/`. `crew` selects the provider/model, `timeout_seconds` bounds the run
(default 1800, maximum 7200), and `idempotency_key` makes a resubmission resolve
the run the first attempt created rather than starting a second agent.

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
`orbit run show|logs|cancel <RUN_ID>`. `show` carries the invocation's outcome,
whether it terminated its response envelope, a bounded preview of the answer,
the effective `provider_sandbox`, and a durable reference to the full captured
output. A provider that exits zero without terminating its envelope stopped
mid-turn: the run records `failed`, and the exit code alone is never evidence
the investigation succeeded.

Remote sessions are additionally capped by the destination's caller policy.
The durable admission and `trusted_host.execution_admitted` event retain the
destination-resolved caller machine ID, the invocation mode, the actual
identity proof (`key-bound` or `self-asserted`), the workspace checkout, and
cwd. See [remote-access.md](setup/remote-access.md). Do not relaunch a server
with more privileges to work around a denied call.

## Common MCP arguments

These are JSON arguments to the named tool, not shell commands:

```json
{"workspace":"<selector>","query":"<problem terms>","kind":"task","hybrid":true,"limit":5,"model":"<agent-family>"}
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
