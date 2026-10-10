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
legacy membership and version/protocol differences on other hosts. Invalid
or conflicting host files, identity mismatch and version/protocol skew on a
replica's owner are errors. Use `orbit host list` for live details, register
missing owners, restore SSH reachability and deploy matching builds where
needed. `host list` is a report and exits zero when the registry loads even
if a host is down; doctor is the health gate.

### Legacy migration (one release)

An older `~/.orbit/mcp-destinations.toml` is still read while it is the only
file. The first `orbit host add`, `rename` or `remove` migrates its rows (every
retained row must answer; `remove` never contacts the row it drops) and deletes
it, and `orbit host add <ssh-target of a listed host>` is the direct way to
migrate. If both files exist every consumer refuses with
`host_file_conflict`, naming each legacy row the host file lacks; delete the
legacy file, then run the `orbit host add` the error names for each of them.
Legacy rows contribute no task prefix until migrated. The next release drops
the legacy reader and retains the both-files conflict check one release longer.

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
| `legacy_host_unreachable` | Restore the named retained host, or remove a decommissioned legacy row with `orbit host remove <host>`; the removed row is not probed. |
| `host_file_conflict` | Follow the migration diagnostic above; every consumer refuses while both files exist. |

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

### Remote host-agent invocation

Remote `orbit_agent_invoke` is admitted when the session holds `operator` — the
same test as a local invocation, because the caller reached this machine through
an SSH login that already lets it start any process it likes. Start the calling
federated or remote-proxy server with `--operator` and the invocation works; an
agent-served session is refused.

The durable admission and the `trusted_host.execution_admitted` event record
`caller_machine_id` for attribution. There is no identity proof or trust mode to
check any more: there is only one.

After changing the calling side's authority, close and recreate the MCP
connection — a live session keeps the authority it was established with.

A minimal smoke is one short, read-only invocation with an explicit destination
workspace selector, checkout `cwd`, bounded timeout, and the configured crew.
Ask it to report one harmless fact and explicitly not to modify files or start
other work, then verify the run reaches a terminal state.

Admission is accident resistance, not process isolation. The invoked provider
runs outside Orbit's filesystem sandbox as the same operating-system user as the
destination Orbit process and can access everything that user can. Keep the
prompt read-only when that is the intent; a tool declaration does not confine
the provider's own shell.

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
  The serving host's own name selects the serving host.
- Every panel, action, log tail and resource chip follows the selected host,
  and the workspace picker lists that host's workspaces. Settings › Hosts does
  not: it keeps editing the serving host's `hosts.toml`.
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

Default is loopback port 7879. The socket authenticates no client; use a protected
network path such as an SSH tunnel. Local processes can connect to loopback, and
a browser page can send HTTP requests there; the listener rejects HTTP framing
before its body reaches MCP dispatch. Non-loopback exposure is an explicit choice,
not a remedy for a missing capability. SSH caller-policy configuration does not
turn a raw TCP socket or dashboard into an authenticated per-user service.
