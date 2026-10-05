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
host with configured SSH destinations into one tool surface. There is no
`orbit mcp connect` command; use `serve --mode remote`.

For federation, put remote membership in the calling machine's
`~/.orbit/mcp-destinations.toml`:

```toml
[[destinations]]
ssh = "<ssh-config-alias>"
machine_id = "<destination-machine-id>"
```

Copy machine IDs from `orbit config get machine.id` on the corresponding machines. Local
membership is automatic and needs no row. Missing/empty configuration gives a
local-only mux; malformed or ambiguous configuration fails closed. A configured
unreachable destination remains visible in discovery rather than disappearing.

```bash
orbit mcp init --federated --client codex
```

This creates a separate client integration and preserves the ordinary one.
Federation is session-unbound: call `orbit_workspace_list` and pass each
returned host-qualified `selector` unchanged. Do not pass `--workspace` or a
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

`--remote-caller-machine-id` remains an audit label. It marks the session's
transport as SSH and names the calling machine in the destination's
`authorization` audit rows; it authorizes nothing.

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

The dashboard refuses non-loopback binds and has no application login. Its
Origin checks mitigate browser CSRF, not unauthorized port access. Anyone with
access to its forwarded port can reach mutation endpoints with the server's
application authority. Keep access within the intended operator boundary.

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
