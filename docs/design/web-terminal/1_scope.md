---
title: "Scope: a terminal in the dashboard"
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
stage: building
next: "Phase 1: ORB-18488 browser-session credential (backlog, not started)"
feature: web-terminal
doc_role: scope
type: design
summary: "Scope: an interactive shell on any registered host from the dashboard — a PTY over a WebSocket, carried by the existing host forward, behind a browser-session credential that a confined agent cannot read."
tags: [web-terminal, orbit-web, remote-access, security]
paths: ["crates/orbit-web/**", "crates/orbit-exec/src/credential_paths.rs", "crates/orbit-common/src/governance/authorization/catalog.rs"]
related_features: [remote-access, user-interface, policy-sandbox]
---

# Scope: a terminal in the dashboard

Status: proposed; phases 1–5 filed as ORB-18488, ORB-18489, ORB-18491, ORB-18492 and ORB-18493.
Bearing: the dashboard already reaches every registered host. `/api/on/<host>/…` forwards a
request to that host's own `orbit web serve` over an SSH local forward the serving dashboard
opens on demand, and unsafe methods need the serving dashboard's operator session through
the governed `host.forward` operation ([remote-access](../remote-access/1_overview.md),
[ORB-14679], [ORB-14680]). Each dashboard is loopback-only, guarded against browser CSRF
and DNS rebinding by `api/origin.rs`, and serves a CSP with `script-src 'self'`.
`orbit.command.exec` runs one argv on a host for an operator holding the workspace claim
([ORB-10711]). There is no interactive shell anywhere in Orbit.

## Problem

Installing a toolchain on a follower, reading a unit's journal, or poking at a stuck run's
worktree means leaving the dashboard for a terminal, opening `ssh <host>`, and finding the
path again. The dashboard already knows the host, the worktree and the run, and already has
an authenticated route to the host.

`orbit.command.exec` cannot fill the gap, and should not be stretched to. It is deliberately
the opposite of a terminal ([`command_exec.rs`](../../../crates/orbit-runtime/src/command_exec.rs)):
an argv with no shell, no PTY and no stdin; output buffered and capped at 4 MB per stream and
discarded on timeout; a 10-minute ceiling; a workspace claim; and a working directory confined
to a workspace checkout. `sudo` on dk-server-2 needs an interactive password, so even a plain
`apt install` does not run through it, and neither do `journalctl -f`, `htop`, or an install
script that asks a question.

The fact that shapes the fix is who can reach a loopback dashboard. Its operator capability
is a process-wide flag (`orbit web serve --operator`, `DashboardState::operator_session`), not
a per-client credential, and `api/origin.rs` says plainly that its checks are on headers curl
can set, "NOT an access-control boundary". Agent runs keep the host network namespace on both
platforms ([policy-sandbox](../policy-sandbox/2_design.md): bubblewrap retains it, the macOS
profile allows network), while the sandbox masks `~/.ssh` and other credentials so a confined
worker cannot SSH anywhere. A shell endpoint guarded only by the operator flag would therefore
hand a confined worker an unconfined shell as the operator on every registered host: a sandbox
escape. Unconfined agents already run as the operator and can SSH, so for them it changes
nothing; for confined ones it changes everything.

## Goal

From the dashboard, open an interactive shell on any registered host, in its home directory or
in a registered checkout or live run worktree, with the same authority an `ssh <host>` from the
serving host would have and no more. A process inside an Orbit sandbox on any host cannot open
one. `orbit.command.exec`, the host forward's existing behaviour, and every other dashboard
route stay as they are.

## Shape

### Browser-session credential

The terminal is the first dashboard surface that needs authentication rather than an origin
check, so it gets one, scoped to the terminal routes:

1. At startup, a dashboard with the terminal enabled generates a 256-bit secret from the OS
   CSPRNG and writes it to `$ORBIT_HOME/run/web-session.secret` (mode 0600 in a 0700 directory,
   created atomically, replaced on every start).
2. That path joins the shared credential mask in
   [`credential_paths.rs`](../../../crates/orbit-exec/src/credential_paths.rs), so bubblewrap,
   the macOS profile and the plugin broker all hide it from confined children. A plan that
   cannot mask it is refused, as for every other entry.
3. `orbit web serve` (and `orbit web connect`) opens the browser at a one-time URL carrying the
   secret in the fragment, which never reaches the server or its logs. The page exchanges it
   once at `POST /api/session` for a session cookie (`HttpOnly`, `SameSite=Strict`, `Path=/api`,
   no `Secure` since the origin is loopback HTTP), and strips the fragment from history.
   A new `orbit web open` prints or opens a fresh one-time URL for an already running dashboard.
4. Comparison is constant-time. Sessions live in memory, end with the process, and expire after
   `machine.web_session.ttl`.

Every terminal route requires the cookie as well as the operator session and the origin guard.
A request without it gets 401, whatever its headers.

### PTY sessions on each host

The dashboard that will run the shell owns it:

- `POST /api/terminal/sessions` with `{cwd, cols, rows}` spawns the user's login shell
  (`$SHELL -l`, falling back to the passwd entry) on a PTY. `cwd` is `~`, a registered checkout,
  or a live worktree of a run that host knows about, resolved against its registry and run
  records with the symlink-escape refusals `resolve_workspace_cwd` applies to
  `orbit.command.exec`; anything else is refused before spawn.
- The child gets the dashboard's environment minus Orbit's managed-run and authority variables
  (`ORBIT_AGENT_*`, `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_TASK_ACTOR_KIND`, `ORBIT_OPERATOR`) and
  plus `TERM=xterm-256color`. It never inherits a run's environment or secrets.
- `GET /api/terminal/sessions/<id>/attach` upgrades to a WebSocket. Binary frames carry PTY
  bytes both ways; a small JSON control frame carries resize and close. One attached client per
  session; a second attach takes over and the first is closed with a reason.
- A detached session keeps running for `machine.web_terminal.detach_grace` with a bounded scrollback
  ring (`machine.web_terminal.scrollback_bytes`), replayed on reattach, so a dropped tunnel or a page
  reload does not lose the shell. After the grace period, or after `machine.web_terminal.idle_timeout`
  without input or output, the shell's process group gets SIGHUP, then SIGKILL.
- `machine.web_terminal.max_sessions` caps live sessions per host; one more is refused with 429.
- Sessions do not survive the dashboard process. A restart or handover exec ends them, and the
  page says so. Run `tmux` inside the shell for anything that must outlive it.
- Opening, attaching and ending a session is the governed dashboard operation `terminal.open`
  (operator only), and each writes an audit row: host, session id, cwd, shell, child pid, start,
  end, exit status, bytes in and out. Keystrokes and output are never recorded, because
  passwords are typed into it.

PTY support is Unix-only, which matches Orbit's platforms (Windows is WSL2). The PTY and
WebSocket crates are admitted under cargo-deny like any other dependency; axum's `ws` feature
is the expected WebSocket path.

### Remote hosts through the forward

`/api/on/<host>/terminal/…` carries the same routes to a registered host:

- The forwarder learns to pass an HTTP/1.1 `Upgrade: websocket` through the tunnel and then
  splice the two connections. Today it speaks one request per connection, forwards a fixed
  header set and drops hop-by-hop headers, so upgrade is new work. Every gate in front of it
  (origin guard, `host.forward`, path refusals, host resolution) still runs before SSH starts.
- The remote dashboard needs its own session credential, and the serving dashboard has SSH to
  it, so it reads the remote's `run/web-session.secret` over SSH when it opens the tunnel and
  presents it on forwarded terminal requests. A browser cookie is never forwarded. A remote
  dashboard the forward spawned writes its secret like any other.
- A remote without the terminal enabled, or too old to have it, answers 404 or
  `host_too_old`, which the dashboard shows as "terminal not enabled on <host>".

### Dashboard pane

- xterm.js and its fit add-on, vendored as static assets (the CSP allows only `'self'`), loaded
  only when the pane opens.
- **Open terminal** on Settings › Hosts rows, on a run's detail (in that run's worktree, on the
  run's host), and on a registered checkout. It opens in a dock pane that keeps the session
  while you move between views, with a session list to switch or end sessions.
- The pane names the host and cwd in its title bar, shows connection state, and reconnects to
  the same session on a dropped socket.
- Without the operator session, the cookie, or the feature on that host, the control is
  disabled with the reason as its title, as for other write controls.

### Configuration

Host settings (`machine.*`): each host reads its own, and nothing propagates from the serving
host.

| Key | Default | Meaning |
|---|---|---|
| `machine.web_terminal.enabled` | `false` | Serve the terminal routes on this host's dashboard. |
| `machine.web_terminal.max_sessions` | `4` | Live sessions per host. |
| `machine.web_terminal.idle_timeout` | `30m` | End a session with no input or output for this long. |
| `machine.web_terminal.detach_grace` | `10m` | Keep a detached session alive for reattach. |
| `machine.web_terminal.scrollback_bytes` | `262144` | Scrollback replayed on reattach. |
| `machine.web_session.ttl` | `12h` | Lifetime of a browser session cookie. |

### Surfaces

- HTTP: `POST /api/session`, `POST|GET|DELETE /api/terminal/sessions[/<id>]`,
  `GET /api/terminal/sessions/<id>/attach` (WebSocket), and the same under `/api/on/<host>/`.
- CLI: `orbit web open` for a fresh one-time URL. No terminal client in the CLI; `ssh` is it.
- Audit: `terminal.open` rows in each host's audit log, visible in Health › Incidents › Events.

## Non-goals

- **Session recording or keystroke audit.** Passwords are typed here. A recorder, if ever, is an
  explicit per-session opt-in.
- **Multi-user access.** One operator, their own hosts. Teammates and tokens are the path-leases
  listener's business, which serves no shell.
- **Running as another user, or root.** The shell is the dashboard's user; `sudo` inside it works
  as it does over SSH.
- **A terminal for agents.** No MCP tool, no `orbit tool run` path. Agents have
  `orbit.command.exec` and their own shells.
- **Persistence across dashboard restarts.** `tmux` covers it.
- **Exposing the dashboard beyond loopback.** Unchanged.

## Concerns and honest limitations

- **The operator's unconfined processes can still open a shell.** Anything running as the
  operator outside a sandbox can read `web-session.secret` or simply SSH. The credential exists
  to keep confined processes out, not to separate the operator from their own unconfined agents;
  that separation is an OS user, as the path-leases listener does.
- **Other governed dashboard writes stay reachable from a sandbox.** The same loopback reach lets
  a confined worker call `config.set`, `job.run` or `host.forward` on an `--operator` dashboard
  today. This scope protects only the terminal routes; extending the cookie to every governed
  write is the open question below.
- **XSS becomes a shell on every host.** Agent-written text (titles, comments, run output) is
  rendered in the dashboard. `script-src 'self'` blocks injected and inline script, which is
  the main defence; an audit of `innerHTML` sinks before shipping is part of phase 2.
- **Masks are path-based.** A confined child that reaches the secret through a path the mask
  list does not cover would get in. The mask's existing refusal of alias bind mounts applies.
- **A dashboard restart ends every shell**, including during an Orbit binary update. The pane
  says so; it does not try to hide it.

## Phasing

1. **Browser-session credential.** Secret file, mask entry, one-time URL, cookie exchange,
   `orbit web open`. Nothing uses it yet beyond `/api/session`. ([ORB-18488])
2. **Local terminal.** PTY sessions, WebSocket attach, limits, `terminal.open` audit,
   configuration, and the `innerHTML` audit. ([ORB-18489])
3. **Dashboard pane.** Vendored xterm.js, dock pane and session list, Open terminal on
   Settings › Hosts for the serving host. ([ORB-18491])
4. **Remote hosts.** WebSocket upgrade through the forward and the remote credential hand-over;
   Open terminal on every host row. ([ORB-18492])
5. **Context launch.** Open terminal in a run's worktree on its host and in a registered
   checkout. ([ORB-18493])

## Open questions

1. **Should the session cookie gate every governed write, not only the terminal?** It would close
   the sandbox reach to `config.set` and friends, at the cost of breaking scripts that drive an
   `--operator` dashboard with curl today.
2. **Reuse `orbit-auth`?** The path-leases `orbit-auth` crate ([ORB-18410]) will own token
   generation and constant-time comparison. Phase 1 needs only those two and ships first, so it
   uses `getrandom` and a constant-time compare directly; fold onto `orbit-auth` once it lands.
3. **Should a run-worktree shell get the run's sandbox?** Opening confined to the run's policy
   would make "reproduce what the agent saw" honest, but it is a different feature.

## Acceptance (feature level)

- Given a dashboard served with `--operator` and `machine.web_terminal.enabled = true`, the operator opens
  a shell on the serving host and on a registered remote from the dashboard, runs `sudo -v`,
  answers the password prompt, and runs `journalctl -f` until they close it.
- Given a confined agent run on that host, a request to any terminal route, with Host and Origin
  set to the loopback dashboard, gets 401, and `web-session.secret` reads as empty or absent
  inside the sandbox.
- Given a dropped tunnel, the pane reattaches to the same shell within the grace period and shows
  its recent scrollback.
- Given a dashboard without `--operator`, without the feature, or a host without it, no session
  opens and the control says why.
- Every session leaves one audit row with host, cwd, times, exit status and byte counts, and no
  keystrokes or output.

## Task References

- [ORB-10711] — added claim-gated `orbit.command.exec`
- [ORB-14679] — added the per-host forward behind `/api/on/<host>/…`
- [ORB-14680] — added the dashboard host picker over the forward
- [ORB-18410] — adds the `orbit-auth` crate for path leases
- [ORB-18488] — browser-session credential
- [ORB-18489] — PTY sessions and the local terminal routes
- [ORB-18491] — the dashboard terminal pane
- [ORB-18492] — the terminal through the host forward
- [ORB-18493] — Open terminal in a run worktree or checkout

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
