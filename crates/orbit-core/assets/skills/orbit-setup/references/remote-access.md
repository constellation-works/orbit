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

A visible dashboard refreshes every 30 seconds. In a background browser tab it
refreshes no panels and instead checks failed runs and the drain once a minute:
the page title gains a `(N failed)` prefix for runs that failed while the tab
was hidden, cleared when the tab is shown again. **Notify** in the top bar opts
in to a browser notification when a drain ends or a task is newly held on a red
base while the tab is hidden; the browser asks for permission only on that
click.

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

Default is loopback port 7879. The socket authenticates no client; use a protected
network path such as an SSH tunnel. Local processes can connect to loopback, and
a browser page can send HTTP requests there; the listener rejects HTTP framing
before its body reaches MCP dispatch. Non-loopback exposure is an explicit choice,
not a remedy for a missing capability. SSH caller-policy configuration does not
turn a raw TCP socket or dashboard into an authenticated per-user service.

## Lease listener user and lease tokens

Path leases let people who have no SSH login to the owner, and their agents,
coordinate through a lease-only listener (`orbit lease listen`) with bearer
**lease tokens**. Agents run as your Unix user,
so anything your user can write, an agent can write too. The tokens, their
store and the commands that mint them therefore belong to a dedicated system
user, `orbit-lease` by convention, that you become through `sudo -u`.

That boundary holds only if becoming `orbit-lease` (or root) takes a fresh
authorization an agent cannot reuse. Password `sudo` caches a timestamp (15
minutes by default) that a later `sudo -n` in the same terminal accepts, so an
agent started there can mint tokens. Before relying on it, make sudoers
root-owned with `timestamp_timeout=0` for every `sudo` your account can run, no
`NOPASSWD`, no `!authenticate` and no askpass helper, and give your account no
other run-as path (`su`, `doas`, `pkexec`, `systemd-run --uid`, setuid helpers,
or a subuid range covering `orbit-lease`). Where the host cannot enforce that,
tokens are only as safe as your sudo timestamp.

Create the user, its Orbit home and its token store (Linux):

```bash
sudo useradd --system --create-home --home-dir /var/lib/orbit-lease \
  --shell /usr/sbin/nologin orbit-lease
sudo passwd -l orbit-lease
sudo chmod 0700 /var/lib/orbit-lease
sudo install -d -m 0700 -o orbit-lease -g orbit-lease /var/lib/orbit-lease/.orbit
sudo install -m 0600 -o orbit-lease -g orbit-lease /dev/null \
  /var/lib/orbit-lease/.orbit/lease-tokens.db
```

The home holds only the listener's configuration and `lease-tokens.db`: no
workspaces and no provider or forge credentials. Orbit never creates the token
store; root provisions it, empty, and Orbit adds its schema on first use. Give
the account no authorized keys.

The listener user runs a root-owned copy of `orbit`, never your
`~/.cargo/bin/orbit`, which an agent could replace:

```bash
sudo install -m 0755 -o root -g root "$(command -v orbit)" /usr/local/bin/orbit
```

Repeat that install after each `orbit update`; the copy is updated separately.

Mint, list and revoke tokens as the listener user. `-H` gives the command that
user's home; the commands use only the token store in the running user's Orbit
home.

```bash
sudo -H -u orbit-lease /usr/local/bin/orbit lease token create \
  --principal alice --workspace ws_example
sudo -H -u orbit-lease /usr/local/bin/orbit lease token create \
  --principal alice/claude-code --class agent --workspace ws_example --expires 7d
sudo -H -u orbit-lease /usr/local/bin/orbit lease token list
sudo -H -u orbit-lease /usr/local/bin/orbit lease token revoke <token-id>
```

- `create` prints the `orbl_…` secret once; hand it to its holder over a
  private channel. The store keeps only its SHA-256, and `list` never shows a
  secret. `--workspace` scopes the token (comma-separate several).
- Give each person one token and each of their agents its own, named under the
  person (`alice/claude-code`, `--class agent`), so revoking one agent leaves
  the others working.
- Defaults: `--class human`, `--verbs lease` (which includes `read`),
  `--max-priority` `high` for a human and `normal` for an agent, `--hard allow`,
  `--expires 30d` (at most `365d`). The workspace's own `lease.hard` and
  `lease.max_priority` still apply; the stricter wins.
- Revocation and expiry take effect on the token's next use. The store audits
  every mint, revocation and refused verification, never with a secret.

The commands refuse to run:

- as root, or in an agent context;
- as any user whose Orbit home has no `lease-tokens.db`, such as your own,
  with a message to run them as the listener user (they never create a store
  there);
- when the store or its directory is group- or world-accessible or owned by
  another user;
- when their executable, the Orbit home, or any directory above either can be
  written by a user other than root or the running user. The message names the
  path; fix its owner or mode.

On macOS the same checks apply; create the user with your usual tooling.

### Run the lease listener

The listener serves `/healthz` and the lease routes under `/api/leases`,
including notice polling and the SSE event stream, and nothing else, to token
holders. It runs as `orbit-lease` and hands each
authenticated operation to Orbit Web over the lease intake socket, so it never
opens your Orbit store. Your dashboard stays loopback-only. Leave all of this
unset unless the sudo prerequisite above holds on this host.

1. As yourself, turn on the intake socket and restart Orbit Web. Your user
   must be a member of `orbit-lease`'s primary group, and `orbit-lease` needs
   search permission on every directory above `~/.orbit/run`:

   ```bash
   sudo usermod -aG orbit-lease "$USER"   # then log in again
   orbit config set --global lease.intake.peer_user orbit-lease
   orbit web serve                        # or restart its service; it prints the socket path
   ```

2. Write the listener's settings into its own Orbit home, which your user
   cannot write:

   ```bash
   sudo install -m 0600 -o orbit-lease -g orbit-lease /dev/stdin \
     /var/lib/orbit-lease/.orbit/config.toml <<'EOF'
   [lease.listener]
   bind = "100.64.0.5:7480"     # a tailnet or private address
   intake_socket = "/home/<you>/.orbit/run/lease-intake.sock"
   # Behind a TLS-terminating proxy instead:
   # bind = "127.0.0.1:7480"
   # behind_proxy = true
   # trusted_proxy = ["127.0.0.1"]
   # Native TLS instead (required outside loopback/tailnet without a proxy):
   # bind = "0.0.0.0:7480"
   # tls = { cert = "/var/lib/orbit-lease/tls/fullchain.pem", key = "/var/lib/orbit-lease/tls/key.pem" }
   EOF
   ```

   Keys and defaults are in `orbit config keys` under `lease.listener.`.
   Limits below are read from the listener user's global configuration,
   validated at load and reset on restart. Put overrides in
   `[lease.listener.limits]`; invalid values name the key. Defaults suit one
   small-team listener process:

   | Key under `lease.listener.limits` | Default | Range |
   |---|---|---|
   | `failed_auth` | 10 | 1–1000 |
   | `failed_auth_window_seconds` | 300 | 1–86400 |
   | `address_per_minute` | 120 | 1–100000 |
   | `ipv6_prefix_len` | 64 | 0–128 |
   | `token_reads_per_minute` | 60 | 1–100000 |
   | `token_writes_per_minute` | 20 | 1–100000 |
   | `max_streams_per_token` | 2 | 1–100 |
   | `max_in_flight_per_token` | 8 | 1–1000 |
   | `max_connections` | 64 | 1–10000 |
   | `max_in_flight_requests` | 16 | 1–1000 |
   | `max_body_bytes` | 65536 | 1–1048576 |
   | `header_timeout_seconds` | 5 | 1–300 |
   | `request_timeout_seconds` | 10 | 1–300 |
   | `max_tracked_keys` | 4096 | 3–65536 |

   All routes use the address budget before token lookup. A 401 or 403 also
   counts against `failed_auth`; token reads (`GET` and `POST …/check`) and
   writes have separate minute budgets that follow the token across networks.
   IPv6 shares a /64 by default; IPv4-mapped addresses count as IPv4. Trusted
   proxies must append the real client address; changing entries to its left
   never gives a new budget. At the tracked-key cap, new addresses share a
   conservative overflow bucket; new tokens share a separate one. Live
   budgets are never evicted to admit new identities.

   Rate limits return 429 with `Retry-After`, body limits 413, handler/body
   timeouts 408 and concurrency limits 503. Connection limits include TLS
   handshakes; both handshakes and HTTP headers have the header timeout.
   Connections close after their response; headers allow at most 64 fields
   and a 16 KiB parsing buffer. One extra bounded refusal lane can complete
   TLS and return 503; further saturated TLS connections close immediately
   (plain HTTP gets a best-effort 503). An open SSE notice stream
   (`/api/leases/events`) holds its connection and global and token
   concurrency, and counts toward `max_streams_per_token`, until it closes;
   it closes at listener shutdown.

   Refusals never reach the owner's store. The listener coalesces them by
   canonical key, limit/reason and window in `lease_listener_limit_audit` in
   its own `lease-tokens.db`, recording count and first/last times without
   principals, secrets, paths or queries. First snapshots are prompt and
   subsequent counts flush every second and at graceful shutdown; abrupt
   termination can lose unflushed counts. Token administration retains its
   `lease_token_audit` trail. Use one listener process to share these budgets.

   For native TLS, provision a PEM certificate chain (leaf first) and its
   matching unencrypted PEM key at those paths, readable by `orbit-lease`.
   Keep the key private: `sudo chmod 0600 /var/lib/orbit-lease/tls/key.pem`
   and `sudo chown orbit-lease:orbit-lease /var/lib/orbit-lease/tls/key.pem`.
   The listener uses rustls with ring, supports TLS 1.2 and 1.3, and accepts
   HTTPS only on a TLS-configured port. Missing, unreadable, invalid or
   mismatched material, or any group/other key permissions, refuses startup
   with a message naming `lease.listener.tls`.

   Certificates and keys load once at startup. After renewal, restart the
   listener with `sudo systemctl restart orbit-lease-listener`; SIGHUP does
   not reload certificates. Use `https://<certificate-hostname>:7480/healthz`
   and `https://<certificate-hostname>:7480/api/leases?workspace=ws_example`
   in the checks below for native TLS, with your CA trusted by the client
   (`curl --cacert /path/ca.pem` for a private CA).

3. Run it from a systemd unit, `/etc/systemd/system/orbit-lease-listener.service`:

   ```ini
   [Unit]
   Description=Orbit lease listener
   After=network-online.target
   Wants=network-online.target

   [Service]
   User=orbit-lease
   Group=orbit-lease
   Environment=HOME=/var/lib/orbit-lease
   ExecStart=/usr/local/bin/orbit lease listen
   Restart=on-failure
   RestartSec=5
   NoNewPrivileges=yes
   PrivateTmp=yes
   ProtectSystem=strict
   ReadWritePaths=/var/lib/orbit-lease/.orbit

   [Install]
   WantedBy=multi-user.target
   ```

   ```bash
   sudo systemctl daemon-reload
   sudo systemctl enable --now orbit-lease-listener
   journalctl -u orbit-lease-listener -f
   curl -s http://100.64.0.5:7480/healthz          # {"status":"ok"}
   curl -s -H "Authorization: Bearer orbl_…" \
     "http://100.64.0.5:7480/api/leases?workspace=ws_example"
   # Follow notices; a reconnecting client sends Last-Event-ID instead of after.
   curl -sN -H "Authorization: Bearer orbl_…" \
     "http://100.64.0.5:7480/api/leases/events?workspace=ws_example&after=0"
   ```

   It runs the root-owned `/usr/local/bin/orbit`. After `orbit update`,
   reinstall that copy and restart the unit; until then the listener and
   Orbit Web refuse each other with a protocol-version message.

`orbit lease listen` refuses to start, with a message naming the reason:

- as root; as the user that owns the intake socket (that is you); or when the
  executable, the listener's Orbit home or a directory above either is
  writable by another user;
- when `lease.listener.bind` is unset, when it is `0.0.0.0` or `::` without
  native `tls` or `behind_proxy = true`, when `behind_proxy` is set without
  `trusted_proxy`, or when `lease.listener.tls` names unusable material or
  a key with group/other permissions.

**Behind a proxy.** The listener believes `X-Forwarded-For` only from a peer
in `trusted_proxy`, and takes the rightmost entry that is not a trusted proxy
as the client; `Forwarded` is never read. So the proxy must append the address
it received the connection from (nginx `$proxy_add_x_forwarded_for`) or
overwrite the header with it (nginx `$remote_addr`). It must never pass a
client's `X-Forwarded-For` through as the only entry, or clients choose their
own address. A malformed entry is answered with 400, never replaced by the
proxy's address.

**What clients see.** No token, or an unknown, revoked or expired one: 401
`{"error":"unauthorized"}`. A token without the verb or workspace: 403. A
`principal` in a request is ignored; the token's is used. Requests are the
same JSON as Orbit Web's `/api/leases` routes; add `?workspace=<id>` when a
token names several workspaces.

**Audit.** Granted and refused operations that reach Orbit are in your audit
log as `lease.intake.<operation>`. Refusals that stop at the listener are rows
in `lease_token_audit` in its `lease-tokens.db`, with remote address, route
and reason, never a secret:

```bash
sudo -u orbit-lease sqlite3 /var/lib/orbit-lease/.orbit/lease-tokens.db \
  "select at, event, reason, remote_address, route, token_id
   from lease_token_audit order by audit_id desc limit 20"
```
