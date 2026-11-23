# Tools: transport, store and routing

Contents:

- [MCP schema shape](#mcp-schema-shape)
- [Tool families](#tool-families)
- [Select the store before reading or writing](#select-the-store-before-reading-or-writing)
- [Task IDs and host selection](#task-ids-and-host-selection)
  - [Routing failures and remedies](#routing-failures-and-remedies)
- [Common MCP arguments](#common-mcp-arguments)

Orbit exposes a curated MCP surface and a larger CLI catalog. Use the installed
version's `tools/list`, `orbit tool list`, and command `--help`; a similar name
is not evidence that an operation exists. This skill explains the contracts
without requiring an Orbit source checkout.

## MCP schema shape

Property descriptions on `orbit_task_add`, `orbit_task_update` and
`orbit_task_list` are one sentence. Relation types, `regression_from`
attribution, status transitions, context-file creation intent, granted and
required tools, capability labels, OS tags, field projection and the review
verdict are in [task-fields-projections.md](task-fields-projections.md). Those three tools link that
reference from their tool description.

MCP fields that take several strings advertise an array of strings. The CLI
still accepts a repeated flag or a comma-separated string for the same
fields; that string form is not the published MCP schema.

Every workspace-scoped tool uses one `workspace` description: `Workspace
selector from orbit_workspace_list.` Whether the selector is required is the
schema's `required` array, not the description.

## Tool families

Related operations share one tool and a required `action` field:

| Tool | Actions |
|---|---|
| `orbit_task_recover` | `recover_claim`, `requeue`, `resume_delivery`, `repilot`, `retry_handoff`, `revoke_handoff`, `review_reset`, `reconcile_review` |
| `orbit_run` | `list`, `show`, `observe`, `resume` |
| `orbit_workflow` | `auto`, `ship`, `land`, `refresh` |
| `orbit_auto_task` | `list`, `add`, `update`, `mint` |
| `orbit_decision` | `list`, `show`, `raise` |
| `orbit_codebase` | `show`, `related`, `analytics` |
| `orbit_friction` | `add`, `update` |
| `orbit_task_artifact` | `get`, `put` |
| `orbit_ui` | `open`, `inspect` |

The `action` description lists each action's required fields, and a call
missing one is refused with a message naming them. `orbit_auto_task`
(`automation`), `orbit_codebase` (`codebase`) and `orbit_ui` (`ui`) are opt-in
in the operator MCP profile (see [MCP toolsets](tool-surface-mcp-serve.md#mcp-toolsets)); use their CLI
commands or enable their toolsets.

`orbit_workflow` action
`auto` and `orbit_task_recover` action `reconcile_review` take their own
sub-command as `mode` (`status`, `start`, `stop`, `resize`; `inspect`,
`submit`, `status`, `accept_baseline`). The CLI is the same:
`orbit tool run orbit.run --input '{"action":"show","id":"<run-id>"}'`.

Grants, `required_tools` and activity allow or deny lists name a whole family
(`orbit.run`) or one action as `family:action` (`orbit.run:show`). A name
that is neither is not a tool and matches nothing in a list.

## Select the store before reading or writing

For a single task addressed by ID, use the prefix-routing rules below. For
workspace-scoped calls, select the store explicitly.
In a managed activity, use the injected task and inherited workspace binding;
its allowlist may omit `orbit.workspace.list`. For an unbound MCP session, call
`orbit_workspace_list({})` on the configured connection first.

For CLI-only use,
inspect `orbit workspace list` and `orbit workspace show` on the intended host,
then use its registered workspace in tool calls. MCP setup is not a prerequisite
for local task tracking. Inspect host, workspace, ownership, availability and
capabilities where returned.

Workspace discovery checks checkout roots against the destination's filesystem
on each call without saving status changes. A removed checkout disappears from
a direct server's list. Federated discovery keeps its descriptor with
`checkout_health: invalid`; restore or re-register the checkout before using it.

- Direct server: pass the returned logical workspace ID as `workspace`.
  The server can also resolve registered names and absolute checkout paths;
  a linked Git worktree resolves to its registered checkout. IDs avoid
  ambiguity, and an explicit selector overrides its session binding.
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

For workspace-optional tools, omitting `workspace` reads the bound workspace or,
when unbound, every workspace homed on the authoritative server; the federated
mux aggregates reachable owners and reports unreachable owners as degraded.
An ID-routed call on an unbound direct server can omit the selector to resolve
the task in that server's registry. A direct server never relays remote-prefix
IDs and names the remote host in its `task_prefix_remote` refusal.

A task-show
workspace filter is explicit and fail-closed: a different owning workspace is
not-found, and an unknown selector is rejected by name. Do not pass a cwd,
session metadata or linked-worktree runtime identity as an explicit filter.
Presentation tools require the explicit registered selector when addressing a
task or run; they never infer it from the session or cwd.

If discovery lists no workspace, run `orbit init` and then `orbit workspace init`
from the project directory.

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
orbit tool run orbit.task.show --input '{"id":"<task-id>"}'
orbit tool run orbit.task.update --input '{"id":"<task-id>","comment":"<progress>"}'
```

The ID-routed tools are `orbit.task.show`, `update`, `reject`, `delete`,
`artifact.get`, `artifact.put`, `review_reset`, `reconcile_review` and
`requeue` (each under `orbit.task.`). Routing applies only where that tool is advertised and
authorized.

`orbit.task.add`, `list`, `eligible`, `lint`, `pull` and `locks*`
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
orbit tool run orbit.task.list --host <name> --workspace <workspace-name-or-ws_id>
```

`--host` accepts an exact registered host name or `machine_id`. Orbit reads
that host's live workspace list, matches the name or `ws_*` ID, and copies
its selector; never build a selector by concatenation. The flag is accepted
on `orbit tool run` and, with `--workspace`, on `orbit task show`, `update`,
`artifact put|get`, `review-reset` and `reconcile-review` subcommands. Put it
after the task subcommand.

Agents keep writes on registered tools for
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
| `unknown_task_prefix` | Check `orbit host list`, then register the task's host with `orbit host add <ssh-target>`. |
| `task_prefix_remote` | Use federated MCP or the CLI's ID route, or connect directly to the named host and select its workspace. A direct MCP server does not relay. |
| `unknown_host` | Copy the exact name or machine ID from `orbit host list`; register a missing host with `orbit host add <ssh-target>`. |
| `owner_unreachable` | Restore SSH and the task host's Orbit process. If the error names a local mirror, select it explicitly for a labelled snapshot read. |
| `unreachable_destination` | Check that the registered SSH target logs in without a prompt and has `orbit` on PATH, then retry discovery. |
| `stale_route` | Select a workspace the host's live list reports; repair its registration on that host if it should exist. |
| `unknown_selector` | For an ambiguous name, use the host's exact `ws_*` ID. For bare `--pull`, supply `--host` or the full discovered selector. |
| `outcome_unknown` | The call was dispatched but its reply was lost. Inspect live state or the operation's receipt before retrying a write; do not mint a replacement request key. |
| `tool_not_on_this_host` | Discover the destination's tool catalog and use a supported operation or deploy the required matching build. |
| `capability_refused` | Follow the named authority/owner restriction. A replica or local-host pull is not an owner route; routing grants no extra capability. |
| `protocol_skew` | Deploy matching Orbit builds and restart long-lived processes on both endpoints before a new pull drain. |

Registration errors, the doctor's `hosts` row and the retired destinations file
recovery are in [remote-access-mcp.md](../../orbit-setup/references/remote-access-mcp.md).

## Common MCP arguments

These are JSON arguments to the named tool, not shell commands:

```json
{"workspace":"<selector>","query":"<problem terms>","kind":"task","limit":5}
```

Use with `orbit_search` before filing a task. Search closed history as well with
`all: true` when looking for already-delivered work.

```json
{"workspace":"<selector>","id":"<task-id>","fields":["status","description","acceptance_criteria","execution_summary"]}
```

Use with `orbit_task_show`. A task title or an agent's narrative is not completion
evidence; inspect status and the recorded implementation/validation outcome.

```json
{"workspace":"<selector>","action":"ship","task_ids":["<task-id>"],"mode":"pr","base":"<integration-branch>","allowed_crews":["<configured-crew>"]}
```

Use with `orbit_workflow` action `ship` only when execution is authorized. At least one
explicit ID is required; MCP does not offer the CLI's no-ID discovery mode.
It also intentionally accepts no completion authorization, so it always
submits review-only work. When an authorized operator has access to the owning
host, use `orbit run ship <task-id> --complete` there for a one-run completion
authorization; do not use a local shadow store as a substitute.

Otherwise
report the MCP completion capability gap rather than inventing a `completion`
argument.
A ship receipt includes `workflow`, `job_id`, `run_id`, `state`, `submitted_at`
(the submission timestamp), and `review` (the captured `{timing, source}` layer,
or null). These fields agree with CLI `orbit run ship --json` and HTTP
`POST /workflows/ship`; only the CLI also keeps `wait_timeout`, `attempt`,
`error_code` and `error_message` from its generic dispatch envelope.

A ship receipt, like an `orbit_workflow` action `auto` start, carries the notices the
CLI and the dashboard report for the same submission: `warning` (one line per
notice: an explicit ship's host resource throttle, a validation environment
that may not find the toolchain, unset `execution.env.pass` names) and
`validation_env_warning`.

A drain start also carries `resource_throttle`, a
`warning` line naming backlog tagged for another OS, and a `note` when no
required validation is configured. None of them refuses the submission.
An operator session reads the returned run with `orbit_run` action `show`
using `id` and `workspace` (operator authority; denied for agent sessions);
a session with a shell on the owning host uses `orbit run show <run-id> --json`.

An operator session lists bounded history with `limit`, `job_id`, `state`
(including `terminal`), and RFC3339 `since` (`orbit_run` action `list`);
a session with a shell on the owning host uses `orbit run history --json`.
Submission success means a durable run exists, not that the
change landed. An operator session resumes with `orbit_run` action `resume`;
it returns a new linked run; preserve both IDs.
