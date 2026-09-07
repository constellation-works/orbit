# Remote access, federation, and caller permissions

Remote access reads or operates the accepting host's live store. It does not
replicate tasks. First identify the owning host and workspace; see
[multi-host.md](multi-host.md) for owner/replica setup and
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

Copy machine IDs from `orbit host show` on the corresponding hosts. Local
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
On SSH, a request for operator capability is capped by the destination's
`~/.orbit/mcp-callers.toml`; the caller cannot grant itself that authority.

```bash
orbit mcp callers list
orbit mcp callers check <caller-machine-id>
orbit mcp callers init
```

`init` seeds known callers with agent grants only. Operator grants are deliberate
operator edits on the destination. For example:

```toml
default = "deny"

[[callers]]
machine_id = "<caller-machine-id>"
label = "<display-name>"
capabilities = ["agent", "operator"]
workspaces = ["<allowed-workspace-id>"]
```

`default` accepts `agent` or `deny`, never `operator`. A missing callers file
means agent by default. `workspaces` optionally narrows a row to specific logical
IDs. Effective permissions are the intersection of the requested capability and
the destination grant, with ordinary tool policy still enforced. A malformed
file fails closed. Inspect the check result and session audit evidence when a
workflow tool is absent or denied; do not treat changing remote argv as a fix.

### Remote host-agent invocation

Remote `orbit_agent_invoke` is a narrower grant than remote operator access.
The matched caller row must explicitly enable the operation on named
workspaces and grant operator capability. Omission of `agent_invoke_mode`
preserves the strict key-bound mode:

```toml
default = "deny"

[[callers]]
machine_id = "<caller-machine-id>"
label = "<display-name>"
capabilities = ["agent", "operator"]
ssh_key_fingerprint = "SHA256:<caller-public-key-fingerprint>"
agent_invoke = true
agent_invoke_workspaces = ["<invocation-workspace-id>"]
```

The destination can instead opt one row into the existing cooperative SSH
operator channel without Tier-2 infrastructure:

```toml
default = "deny"

[[callers]]
machine_id = "<caller-machine-id>"
label = "<display-name>"
capabilities = ["agent", "operator"]
agent_invoke = true
agent_invoke_mode = "cooperative"
agent_invoke_workspaces = ["<invocation-workspace-id>"]
```

This is an explicit destination-owner statement that callers using the same
SSH OS account cooperate. The machine ID remains self-asserted: any peer able
to start the same non-interactive SSH operator session can name another row.
The policy prevents accidental use and limits Orbit's admission to the named
workspace and one invocation; it does not isolate mutually malicious peers who
share the account and already control that account's files and processes.
Never describe a cooperative admission as key-bound.

`agent_invoke_workspaces` is independent from ordinary `workspaces`. When a
caller already has general operator access, add the operation scope without
narrowing that access. Leave its existing capabilities and absent `workspaces`
unchanged, then add only:

```toml
agent_invoke = true
agent_invoke_mode = "cooperative"
agent_invoke_workspaces = ["ws_orbit"]
```

Omitting `agent_invoke_workspaces` preserves the older behavior where
`workspaces` is also the invocation scope. Empty, malformed, or unknown scope
shapes fail closed.

For either mode, install the updated Orbit binary on the destination, review
and edit `~/.orbit/mcp-callers.toml` there, run `orbit mcp callers check
<caller-machine-id>`, then close and recreate the calling MCP connection. A
live session keeps the policy loaded at establishment. For strict mode, first
generate and install the destination-issued forced command with `orbit mcp
callers authorize`. To revoke either mode, remove the row or set
`agent_invoke = false`, then reconnect; the next invocation is denied.

After reconnecting, a minimal smoke is one short, read-only invocation with an
explicit destination workspace selector, checkout `cwd`, bounded timeout, and
the configured Luna crew. Ask it to report one harmless fact and explicitly not
to modify files or start other work. Verify the submission and
`trusted_host.execution_admitted` event report `agent_invoke_mode = "cooperative"`
with `caller_identity = "self-asserted"` (or both fields as `key-bound` in
strict mode), and verify the run reaches a terminal state.

Strict mode is an authenticated caller and accident-resistant admission path;
neither mode is process isolation. The invoked provider runs outside Orbit's
filesystem sandbox as the same operating-system user as the destination Orbit
process and can access everything that user can. Keep the prompt read-only when
that is the intent; a tool declaration does not confine the provider's own
shell.

There are two identity strengths. Ordinary SSH proxy identity is a self-asserted
audit label, suitable for cooperative operators with shell access; it is not
proof against a caller naming a different machine. Key-bound acceptance uses a
forced command generated on the destination:

```bash
orbit mcp callers authorize --machine-id <caller-machine-id> \
  --key <public-key-path> --launcher <protected-orbit-launcher>
```

This prints configuration; it does not install it. The protected path requires
a dedicated Linux login account, a root-managed `AuthorizedKeysFile`, per-key
environments enabled, and a root-owned mode-2555 setgid Orbit copy configured as
that account's login shell. The launcher must match the running Orbit binary and
use a group different from the account's primary group. Follow the installed
command's help and host administration procedure; an ordinary shell wrapper is
not equivalent. Refresh the protected launcher after binary upgrades. Do not
infer verified identity merely from a configured key fingerprint: inspect the
recorded identity proof.

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
network path such as an SSH tunnel. Non-loopback exposure is an explicit choice,
not a remedy for a missing capability. SSH caller-policy configuration does not
turn a raw TCP socket or dashboard into an authenticated per-user service.
