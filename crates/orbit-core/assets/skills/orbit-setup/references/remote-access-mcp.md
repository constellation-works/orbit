# Remote access: MCP and destination authority

## Contents

- [Direct and federated MCP](#direct-and-federated-mcp)
  - [Role and toolset selection](#role-and-toolset-selection)
  - [Retired destinations file](#retired-destinations-file)
  - [Registration failures and remedies](#registration-failures-and-remedies)
- [Authority on the destination](#authority-on-the-destination)
- [TCP MCP](#tcp-mcp)

Remote access reads or operates the accepting host's live store. It does not
replicate tasks. First identify the owning host and workspace; see
[multi-host.md](multi-host.md) for owner/replica setup,
[distributed-drain-setup.md](../../orbit/references/setup/distributed-drain-setup.md) for
the single-owner drain preflight and recovery, and
[publication-bind.md](publication-bind.md) for offline snapshots.

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
toolsets](configuration-mcp-roles.md#mcp-roles-and-toolsets). Both `serve` and `listen`
read their defaults from this machine's global `machine.mcp.role` and
`machine.mcp.toolsets` settings. Their `--role` and `--toolset` flags override
the corresponding setting for that server. Remote mode forwards the selected
role and toolsets to its destination; federated mode applies the selection to
each destination.

A directly launched server on another machine uses that
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
local-only mux; an invalid one fails closed.

A registered unreachable host
remains visible in discovery rather than disappearing. `orbit host list` flags a
host whose `binary_version` or `protocol_fingerprint` differs from this
machine's, and `orbit doctor` reports it in its `hosts` row.

The doctor's `hosts` row warns on unreachable transports
(`unreachable_destination`), local or other probe failures (`probe_failed`),
missing replica owners, and version/protocol differences on other hosts. A
Doctor probe has a two-second session budget per host; a slow but reachable
host can therefore appear unreachable in this snapshot. `orbit host list`
uses the ordinary, longer probe budget. SSH configuration remains in effect.
A message such as `probe failed (probe_failed): write SSH defaults …` means
local SSH setup failed before Orbit contacted the destination; check the local
path and error named in the diagnostic. It does not establish whether the
destination is reachable. By contrast, `… is unreachable
(unreachable_destination)` reports a failed transport probe. Invalid or
conflicting host files, identity mismatch (`host_identity_mismatch`) and
version/protocol skew on a replica's owner are errors. An identity mismatch
means the remote answered but its machine ID or task prefix differs from the
registered entry. Use `orbit host list` for live details, register missing
owners, restore SSH reachability and deploy matching builds where needed.
The local `[machine]` identity must exist before `orbit host list` can succeed
(run `orbit init` first); without it, listing fails even with `--no-probe` and a
valid host registry. Once that prerequisite is met, `orbit host list` is a
report and exits zero when the registry loads, including when remote probes
fail; those failures appear in the affected host rows. `orbit doctor` is the
health gate.

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
[tool-surface-routing.md](../../orbit/references/tool-surface-routing.md#routing-failures-and-remedies).

```bash
orbit mcp init --federated --client codex
```

This creates a separate client integration and preserves the ordinary one.
Federation is session-unbound: for workspace-scoped calls, use
`orbit_workspace_list` and pass its host-qualified `selector` unchanged.
Single-task calls can instead omit the selector and route by task prefix;
see [tool-surface-routing.md](../../orbit/references/tool-surface-routing.md#task-ids-and-host-selection).
Do not pass `--workspace` or a
positional SSH destination to federated mode.

On a direct server, a session
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
HTTP framing before its body reaches MCP dispatch.

Non-loopback exposure is an
explicit choice, not a remedy for a missing capability, and it does not admit
other users. SSH caller-policy configuration does not turn a raw TCP socket or
dashboard into an authenticated per-user service.
