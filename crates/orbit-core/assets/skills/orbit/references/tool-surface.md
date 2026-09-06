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
| Task attachments | `orbit_task_artifact_put` | Task artifact commands; source path is on the executing host |
| Retrieval | `orbit_search` | `orbit search`; semantic install/index is separate |
| Friction | `orbit_friction_add/list/update` | Additional show/stats/tags/resolve commands |
| Submit explicit tasks | `orbit_workflow_ship` (review-only; no completion input) | `orbit run ship`, `run auto` |
| Observe/resume workflows | `orbit_workflow_run_show/list/resume` | `orbit run show/history/events/trace/logs/cancel`; job replay/resume |
| Auto-tasks | `orbit_auto_task_list/mint` | Definition add/show/update/toggle are CLI operations; do not assume they are advertised over MCP |
| Host commands | `orbit_command_exec` when advertised and authorized | Explicit argv and working directory, never a shell string |
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

Remote sessions are additionally capped by the destination's caller policy.
See [remote-access.md](setup/remote-access.md). Do not relaunch a server with
more privileges to work around a denied call.

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
{"workspace":"<selector>","task_ids":["<task-id>"],"mode":"pr","base":"<integration-branch>","model":"<agent-family>"}
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
