# Remote access, federation, and remote authority

Remote access reads or operates the accepting host's live store. It does not
replicate tasks. First identify the owning host and workspace; see
[multi-host.md](multi-host.md) for owner/replica setup,
[distributed-drain.md](../../orbit/references/setup/distributed-drain.md) for
the single-owner drain preflight and recovery, and
[publication.md](publication.md) for offline snapshots.

## Direct and federated MCP

```bash
orbit mcp serve --workspace <workspace-id>
orbit mcp serve --mode remote <ssh-host>
orbit mcp serve --mode federated
```

Local stdio is what a client launches. Remote mode relays one non-interactive,
non-PTY SSH connection to a destination Orbit. Federated mode combines the local
host with every registered remote host into one tool surface. There is no
`orbit mcp connect` command; use `serve --mode remote`.

### Role and toolset selection

Choose the role and additional toolsets as described in [MCP roles and
toolsets](configuration.md#mcp-roles-and-toolsets). Both `serve` and `listen`
read their defaults from this machine's global `machine.mcp.role` and
`machine.mcp.toolsets` settings. Their `--role` and `--toolset` flags override
the corresponding setting for that server. Remote mode forwards the selected
role and toolsets to its destination; federated mode applies the selection to
each destination. A directly launched server on another machine uses that
machine's own global settings.

This selection controls which tools MCP advertises and dispatches. It does not
grant authority: even the `operator` role needs the separate `--operator`
choice for operator-only capabilities. To inspect what a remote or federated
client actually receives, initialize that MCP connection and call
`tools/list`; `orbit tool list --role <role>` reports the built-in role
baseline, not remote-specific or extra configured toolsets.

For federation, register each remote host on the calling machine:

```bash
orbit host add <ssh-config-alias>          # reads machine_id, name, task prefix from the host
orbit host add user@10.0.0.7 --name build  # name defaults to the remote's machine.name
orbit host list                            # live reachability, version, protocol, workspaces
orbit host show <name|hm_id>               # one host and what here depends on it
orbit host rename <name|hm_id> <new-name>
orbit host remove <name|hm_id> [--force]   # refused while a replica or pull drain uses it
```

`orbit host add` probes the host over the same SSH session federation uses and
writes `~/.orbit/hosts.toml`; never edit that file to register a host. It
refuses a duplicate machine id, name or task prefix, this machine itself, an
unreachable target and a remote too old to report its task prefix. Local
membership is automatic and needs no entry. A missing host file gives a
local-only mux; an invalid one fails closed. A registered unreachable host
remains visible in discovery rather than disappearing. `orbit host list` flags a
host whose `binary_version` or `protocol_fingerprint` differs from this
machine's, and `orbit doctor` reports it in its `hosts` row.

The doctor's `hosts` row warns on unreachable hosts, missing replica owners,
and version/protocol differences on other hosts. Invalid
or conflicting host files, identity mismatch and version/protocol skew on a
replica's owner are errors. Use `orbit host list` for live details, register
missing owners, restore SSH reachability and deploy matching builds where
needed. `host list` is a report and exits zero when the registry loads even
if a host is down; doctor is the health gate.

### Retired destinations file

`~/.orbit/mcp-destinations.toml` is no longer read. Its presence, alone or
beside `hosts.toml`, fails every host-registry load with `host_file_conflict`;
`orbit doctor` reports an error. Move it aside, run `orbit host add <ssh-target>`
for each `ssh` row to register the hosts in `hosts.toml`, then delete the moved
file. Automatic migration is no longer available.

### Registration failures and remedies

| Code | Remedy |
|---|---|
| `host_exists` | Use the existing entry; use `orbit host rename` to change its display name. |
| `host_name_conflict` | Pick an unused `--name`, or rename the conflicting registered entry. |
| `task_prefix_conflict` | Reach the intended host or initialize a distinct host with an unused prefix; existing prefixes are immutable. |
| `host_is_local` | Use the automatically listed local host; it needs no registration. |
| `host_too_old` | Upgrade the remote to a build that reports its machine identity and task prefix, then add it again. |
| `host_identity_mismatch` | Verify the SSH alias reaches the intended machine. If replacing a host deliberately, reconcile its dependents before removing and re-adding the entry. |
| `host_in_use` | Inspect `orbit host show` and reconcile the named replica checkouts or pull drains before removal. `--force` deliberately leaves those dependents without a route. |
| `host_file_conflict` | Move the retired destinations file aside, re-register its hosts as described above, then delete it. |

Routing and reachability errors have their own remedies in
[tool-surface.md](../../orbit/references/tool-surface.md#routing-failures-and-remedies).

```bash
orbit mcp init --federated --client codex
```

This creates a separate client integration and preserves the ordinary one.
Federation is session-unbound: for workspace-scoped calls, use
`orbit_workspace_list` and pass its host-qualified `selector` unchanged.
Single-task calls can instead omit the selector and route by task prefix;
see [tool-surface.md](../../orbit/references/tool-surface.md#task-ids-and-host-selection).
Do not pass `--workspace` or a
positional SSH destination to federated mode. On a direct server, a session
binding via `--workspace` is valid, and explicit per-call selectors take
precedence. `--root` is not an MCP workspace-routing mechanism.

## Authority on the destination

Bare local `serve` and ordinary `mcp init` are agent-only.
`workspace init --mcp` deliberately installs a local operator integration.

Over SSH, `--operator` travels. Orbit is a single-user tool and an SSH login to
a machine is ownership of it: anyone who can run
`ssh box "orbit mcp serve --operator"` can equally run
`ssh box "ORBIT_OPERATOR=1 orbit tool run …"`. The destination therefore serves
the authority in the argv it was started with, the same way it does locally, and
there is no destination-side callers file, forced command, or per-caller setup:

```bash
orbit mcp serve --mode federated --operator
orbit mcp serve --mode remote <ssh-host> --operator
```

Without `--operator` on the calling side, remote sessions hold `agent`. Orbit
governs agents, not people: a client running inside a managed run, or with an
agent envelope in its environment, never propagates operator into a
destination's argv, whatever the process that launched it held. To deny a
caller, remove its key from the destination's `~/.ssh/authorized_keys` — that
is the only boundary the destination ever had.

`--remote-caller-machine-id` is a caller-chosen machine label. Its presence
marks the session's transport as SSH MCP without proving SSH origination. It
names the calling machine in audit rows and selects the remote drain receipt
namespace. Claim bind/settle uses it as the machine fence: the journal compares
it with the claim's execution machine, bound run and phase. An initialize
`_meta.orbit.worker_invocation` must name that same execution machine.

The label grants no capability and authenticates no machine. This fence is
acceptable within Orbit's single-user trust model because local account access
and SSH login already establish owner access. It prevents accidental mixing of
attempts among cooperating executors; a caller able to start a server can choose
another machine's label, including locally from a managed agent context. It
does not isolate mutually untrusted callers sharing that account.

A destination upgraded from the older model may still carry
`~/.orbit/mcp-callers.toml` or `~/.orbit/mcp-ssh-acceptance/`. Both are ignored:
startup names them once in a warning, `orbit doctor` reports them, and deleting
them is the whole migration. `orbit mcp callers` no longer exists.

## Dashboard

```bash
orbit web serve --no-open
orbit web serve --port 8080 --no-open
orbit web connect <ssh-host>
orbit web connect <ssh-host> --remote-port 7878 --port 9000
orbit web connect <ssh-host> --workspace <remote-workspace-selector>
```

`orbit --root <ROOT> web serve` serves `<ROOT>/workspaces.json` and nothing from
the machine-global registry, so an explicit root isolates the dashboard the same
way it isolates every other command. `--workspace` picks which of the served
workspaces the dashboard opens on, and is what `connect` forwards to the remote
server; `connect` itself rejects `--root`.

The default dashboard port is 7878. `connect` reuses an existing remote loopback
server when available; otherwise it starts one and owns that process's lifetime.
The browser offers workspace selection, task detail and lifecycle controls,
run/step inspection, routines, knowledge/frictions, audit, and metrics. Verify the
selected workspace before any mutation; a dashboard aggregate or metric is not
proof that a particular task or run succeeded.

In the left rail, the **Workspace** picker is single-select. It lists the workspaces
for the selected host, marks unavailable ones *(unavailable)* and leaves them
unchosen, and offers **All workspaces** as its first option. Its button shows the
chosen workspace's name, or **All workspaces** when none is chosen. Choose a
workspace to open its board, or choose **All workspaces** for the aggregate view.

In the Tasks list, the **Workspaces** chip sits beside **More** and appears only
under **All workspaces** when the host has more than one active workspace. It is
multi-select: check or uncheck workspaces, and the Tasks board then contains only
the checked workspaces' tasks. Each task's workspace badge shows which workspace
it belongs to. The chip reads **Workspaces** when every active workspace is
checked, and **N workspaces** while the selection narrows the board; its tooltip
lists the chosen names. Narrowing the checks to one workspace opens that
workspace's board.

Each Tasks row can show a task riskiness percentage: an estimate of whether a
task of that size, once done, will later be named by a regression. The estimate
uses task size alone, fitted on done tasks from the trailing 30 days. Before a
diff exists, size is the declared path count; once a review candidate or landed
diff resolves, size is the lines changed if a lines-based fit is available;
otherwise the estimate falls back to the declared path count. The 30-day window
and the minimum of five done tasks are fixed. A `~` before the percentage means some selectors
name no path. If no estimate is available, the row has no risk part. Task detail
shows the estimate's basis and size, plus how many recent done tasks of that
size or larger regressed. When there is no estimate, detail gives one of four
reasons: **no files**, **no paths**, **thin history**, or **unavailable**.
Analytics uses this score. `@auto` effort instead uses configured bands based
on complexity and declared files (or review diff lines), with the crew's
configured effort as a fallback; it does not read the riskiness score. To verify
the display, open one task with an estimate and one without; check each Tasks row and task detail
for the percentage or the matching no-estimate state and reason. There is
nothing to configure.

Under **All workspaces**, each status section lists tasks workspace by workspace
in Workspace-picker order, each workspace pill uses that workspace's colour, and
paging continues in that order. There is nothing to configure: choose **All
workspaces** to open the view. The **Workspaces** chip can narrow which
workspaces appear; the remaining rows stay in picker order. To verify the order,
open Tasks under **All workspaces** and confirm each status section keeps one
workspace's rows together in picker order, and the next page continues in that
same order. To verify the colours, open the **Workspaces** chip: each workspace
pill matches its entry's colour in that list, which is the colour key. The rail
Workspace picker has no workspace colours, so use it for order only.

Under **All workspaces**, the Tasks dock adds a **Fleet** tab between its **Drain**
and **Locks held** tabs. Open it to see the **Fleet drains** pane, which
lists drain rows across the registered hosts and their workspaces. The tab is
present only while **All workspaces** is selected in the rail picker; choosing a
single workspace removes it.

The Tasks dock's **Drain** pane holds the **Auto-drain** card and, beside it, the
quick-start card headed **Start a window**. When the target is a replica checkout,
the heading and the toggle read **Start pulling on** followed by the host name. On
narrow layouts (1000px wide or less), a live window folds the quick-start card
behind **Start another window**, which expands the form. Under **All workspaces**,
pick the target in the Auto-drain panel's **Drain workspace** and **Drain host**
selects before starting a window.

A visible dashboard with a workspace selected opens a change stream for that
workspace; **All workspaces** has no single-workspace stream. In the Tasks tab,
the connected stream sends change notices to the panels that subscribe to them,
and a full refresh uses a three-minute safety interval while refreshes succeed.
Other tabs keep the regular 30-second refresh even while a workspace stream is
open. **All workspaces** has no stream and uses that same 30-second interval.
If the stream is unavailable, Tasks also keeps polling at the regular cadence;
a fully closed stream is retried after 60 seconds. A failed full refresh doubles
the normal 30-second interval up to five minutes, and a successful refresh
resets that normal interval to 30 seconds.
With the Tasks stream connected, the safety poll still keeps a three-minute
floor between periodic full refreshes.

In a background browser tab, the dashboard closes the stream and refreshes no
panels. It checks failed runs and, for a selected workspace, drain state and
red-base holds once a minute. The page title gains a `(N failed)` prefix for
runs that failed while the tab was hidden, cleared when the tab is shown again.
**Notify** in the top bar opts in to a browser notification when a drain ends or
a task is newly held on a red base while the tab is hidden; the browser asks for
permission only on that click. Showing the tab refreshes the dashboard and
reopens the stream when a concrete workspace is selected.

### Switching hosts in the dashboard

A host registered with `orbit host add` appears in the dashboard's **Host**
picker, above the workspace picker, so one dashboard can show each registered
machine without an `orbit web connect` tab per machine. Prefer `connect` when
the machine is not registered, when you need `--no-operator` or a non-default
`--remote-port`, or when the dashboard must not depend on the serving machine.

- The serving dashboard reaches the host over its own SSH identity, not the
  browser's. It attaches to a dashboard already running on the remote's default
  port 7878 or starts one, runs SSH with `BatchMode=yes`, and closes an idle
  tunnel after five minutes. A host that needs a passphrase or password
  therefore reports `unreachable_destination`.
- `?host=<name|machine_id>` selects the host in the URL and wins over the
  browser's remembered last choice, which only fills a URL with no `?host=`.
  The serving host's own name selects the serving host. Back and Forward
  restore the host each history entry names, and an entry without `?host=`
  is the serving host.
- Every panel, action, log tail and resource chip follows the selected host,
  and the workspace picker lists that host's workspaces. Settings › Hosts
  keeps editing the serving host's `hosts.toml`. It also shows each reachable
  registered host's load, memory, disk, admission throttle, and drain or
  pull-drain state, and **Show** on a row switches the dashboard to that host.
  An unreachable host shows its error instead of readings.
- Version or protocol skew shows a persistent banner and is never refused.
  An unreachable host replaces the panels with one state carrying a code:
  `unknown_host`, `unreachable_destination`, `process_timeout`,
  `host_identity_mismatch` or `host_too_old`, with Retry and a way back to the
  serving host.
- Where a task's execution line says which machine ran it and that machine is
  registered on the serving host, the name links to the run on that host.

Remote writes need an operator session on the serving dashboard
(`orbit web serve --operator`, or `connect` without `--no-operator`). Without
one the dashboard shows `Read-only on <host>` and disables write controls; a
direct request gets `403 authorization_denied` for `host.forward` before any SSH
starts. A remote dashboard Orbit starts gets `--operator` exactly when the
serving session has it, and one already running keeps its own capability.

The dashboard refuses non-loopback binds and has no application login. Origin
checks and `Sec-Fetch-Site` checks mitigate browser CSRF, not unauthorized port
access. When present, `Sec-Fetch-Site` must be `same-origin` or `none`; direct
CLI/curl requests without the header continue to work. Older browsers that
omit both Fetch Metadata and `Origin` on GETs do not receive the additional
Fetch Metadata protection. Anyone with access to the forwarded port can reach
mutation endpoints with the server's application authority. Keep access within
the intended operator boundary.

## TCP MCP

```bash
orbit mcp listen
orbit mcp listen 0.0.0.0:7879 --allow-non-loopback
```

Default is loopback port 7879. A connection is accepted only when the peer's
user id is the server's. A different user is refused before any MCP message is
read. A remote TCP peer has no local user id and is refused; reach the listener
through an SSH tunnel as this user. Local same-user processes can connect to
loopback, and a browser page can send HTTP requests there; the listener rejects
HTTP framing before its body reaches MCP dispatch. Non-loopback exposure is an
explicit choice, not a remedy for a missing capability, and it does not admit
other users. SSH caller-policy configuration does not turn a raw TCP socket or
dashboard into an authenticated per-user service.
