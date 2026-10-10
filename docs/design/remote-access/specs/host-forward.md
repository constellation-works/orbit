---
type: design
summary: "Spec: Orbit Web host forward — another registered host's dashboard API through this one"
last_validated: 2026-10-08
tags: [remote-access, host-registry]
---

# Spec: Orbit Web Host Forward

`/api/on/<host>/<path>` lets one dashboard read and, with the operator session, act on another registered host's dashboard. The request goes to `/api/<path>` on that host's own `orbit web serve`, through an SSH local forward the serving dashboard opens on demand. The remote host stays authoritative: it answers from its own stores and enforces its own gates. Nothing is cached, merged or synchronized.

`api/forward.rs` owns the routes. `host_tunnels.rs` owns one tunnel per host, built on the same `ssh_tunnel` establish as `orbit web connect` ([ssh-tunnel.md](./ssh-tunnel.md)), in its unattended mode.

## Routes

| Route | Meaning |
|---|---|
| `GET POST PUT PATCH DELETE /api/on/:host/*rest` | Forward to `/api/<rest>` on `:host` |
| `GET /api/hosts/:host/connection` | The host's connection state, opening its tunnel if needed |

`:host` resolves like CLI `<host>` against the serving host's last valid host file: a registered name (case-insensitive) or a `machine_id`. The serving host's own name or `machine_id` is answered by the local router with no tunnel. Only the listed methods are routed, so `OPTIONS` stays a local 405.

## Order of checks

Each step runs before the next, and every refusal happens before any SSH process starts:

1. The router's origin guard (Host and Origin, as for every `/api` route).
2. An unsafe method (`POST`, `PUT`, `PATCH`, `DELETE`) without the operator session: 403 `authorization_denied` naming the governed dashboard operation `host.forward`. Nothing is sent.
3. Paths that are never forwarded: 400 `invalid_input`. These are an empty path, a `.` or `..` segment, a further `on/<host>/…` hop, and `hosts…` (host-file management stays on the serving host). The check reads the percent-decoded path, so an encoded spelling is refused too.
4. Host resolution: an unregistered host is 404 `unknown_host`. A host file that fails to load is reported with its own code.

## Forwarding

- The method, the still-encoded path, the query string (verbatim) and the body are passed through.
- Request headers sent: `Accept`, `Content-Type`, `Last-Event-ID`. `Host` and `Origin` are set to the remote's own loopback authority (`localhost:<remote-port>`), so the remote's origin guard admits the request as same-origin. Cookies and other headers are not sent.
- Response headers returned: `Content-Type`, `Content-Disposition`, `Content-Security-Policy`, `Cache-Control`. An artifact therefore keeps its sandbox CSP and attachment disposition. Hop-by-hop headers are dropped, and the serving dashboard's own security and `nosniff` headers still apply.
- The remote status and body come back unchanged. Bodies are streamed, never buffered.
- One loopback HTTP/1.1 connection per request, to the tunnel's local port.

## Tunnels

One tunnel per host, keyed by `machine_id`.

- **Establish.** Attach first, then spawn, exactly as `web connect` does. The spawned remote command is `orbit web serve --no-open --port <remote>`, plus `--operator` exactly when the serving session has the operator capability. The local listener is `127.0.0.1` on an ephemeral port; the remote port is the default dashboard port, 7878.
- **Unattended SSH.** A server has no terminal, so every SSH child gets `-o BatchMode=yes -o ConnectTimeout=10`, and the wait for the forward listener is bounded. A host that needs a passphrase or password fails as `unreachable_destination` rather than waiting.
- **Single flight.** Concurrent first requests for one host share one establish. Requests waiting behind a failed attempt get that failure rather than starting another. Establish runs on the blocking pool, so a slow host never stalls other requests.
- **Identity.** On each new tunnel the dashboard reads `GET /api/hosts?probe=false` through it and takes the row marked `local`. A `machine_id` other than the host file's is 409 `host_identity_mismatch`, and the tunnel is torn down. A 404 is 409 `host_too_old`: that dashboard predates the host registry and cannot prove its identity.
- **Reuse and re-establish.** A live tunnel is reused. When its SSH child has exited, or the host's SSH target changed, the next request tears it down and establishes again. There is no reconnect loop and no heartbeat.
- **Idle.** A tunnel with no request or open stream for five minutes is torn down.
- **Shutdown.** Graceful shutdown and the binary-update handover both stop every SSH child this process started, and refuse new tunnels, before the process exits or execs. Open event streams end when shutdown begins. An attached tunnel stops only its forward; a spawned remote dashboard exits with its PTY session, as in `web connect`.

## Bounds and failures

Error bodies are `{error, code, host}`.

| Condition | Status | `code` |
|---|---|---|
| Unknown host | 404 | `unknown_host` |
| SSH could not connect, exited early, or the forward closed | 502 | `unreachable_destination`, carrying the SSH exit classification |
| Establish or request bound exceeded | 502 | `process_timeout` |
| Remote `machine_id` differs from the host file | 409 | `host_identity_mismatch` |
| Remote has no `/api/hosts` | 409 | `host_too_old` |
| Unsafe method without the operator session | 403 | `authorization_denied` |
| Refused path | 400 | `invalid_input` |

A forwarded request is bounded at 60 seconds, covering the response head and, except for an event stream, the whole body. An event stream (`text/event-stream`) has no deadline. It closes when the client goes away, when the remote ends it, or when the serving dashboard begins shutdown. An open stream holds its tunnel open against the idle timer.

## Connection state

`GET /api/hosts/:host/connection` returns:

| Field | Meaning |
|---|---|
| `host`, `machine_id`, `local` | The resolved host |
| `reachable` | A tunnel is up and passed the identity check |
| `origin` | `local`, `attached` (a dashboard was already running) or `spawned` (started by this dashboard) |
| `binary_version`, `protocol_fingerprint` | As the identity read reported them |
| `skew`, `skew_fields` | Version or protocol differs from the serving dashboard. Reported, never refused |
| `error` | `{code, message}` when the host is unreachable |
| `forward_writes` | `{authorized, reason}`: whether this session may forward unsafe methods (`host.forward`), with the refusal's reason when it may not |

Every host field comes from the identity read that opened the tunnel; this route never runs a second probe. `forward_writes` is the serving dashboard's own session capability, so the dashboard can disable write controls instead of sending requests the forward refuses. An unreachable host is a 200 with `reachable: false` and the typed error.

## Authority

The forward carries the serving dashboard's authority to every registered host its SSH identity can reach. An operator session on the serving dashboard can act as operator on each of those hosts, because a host spawned for it is started with `--operator`. A dashboard already running on the remote keeps its own capability. Without the operator session, only safe methods are forwarded. The remote's own authorization still applies to every request ([4_decisions.md](../4_decisions.md#forward-dashboard-requests-to-registered-hosts)).

`hosts.toml` is unchanged: schema version 1, no new fields.
