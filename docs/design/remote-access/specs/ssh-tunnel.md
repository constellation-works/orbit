---
type: design
summary: "Spec: Orbit Web SSH Local Forward"
last_validated: 2026-09-09
tags: [remote-access]
---

# Spec: Orbit Web SSH Local Forward

orbit web connect <ssh-host> exposes a remote machine's loopback Orbit Web server through a foreground SSH local forward. It attaches to an existing healthy server when possible and starts one only when necessary.

This implementation lives entirely in orbit-web: connect.rs owns Web-specific command construction, health probing, browser behavior, and shutdown; ssh_tunnel.rs owns the SSH child, forwarding arguments, port selection, readiness polling, exit classification, and teardown.

It is not used by MCP. MCP remote mode uses direct ssh -T stdio with no -L forward, Web health endpoint, attach probe, TCP listener, or PTY.

## Establishment

1. Select the local port. An explicit --port must be free. Otherwise prefer 7878 and fall back to an ephemeral port.
2. Start a probe child with:

       ssh -N -o ExitOnForwardFailure=yes -L 127.0.0.1:<local>:localhost:<remote> <host>

3. Poll HTTP GET /healthz on the local port for up to five seconds.
4. If the response status is 200, retain the probe child as an attached tunnel. No remote command is sent.
5. If the probe times out while SSH remains alive, stop it and start:

       ssh -tt -o ExitOnForwardFailure=yes -L 127.0.0.1:<local>:localhost:<remote> <host> "<remote command>"

6. The remote command is orbit web serve --no-open --port <remote>. It may include a POSIX-quoted --workspace and the compatibility --global flag.
7. Poll /healthz for up to 30 seconds. Once ready, open the local URL unless local --no-open was requested.

A forward can exist while no service listens behind it, so SSH startup alone never proves readiness.

## Security invariants

- The local -L listener is explicitly bound to 127.0.0.1, and its target is remote localhost.
- The remote Web process separately enforces its own loopback bind.
- Orbit Web adds no token, ACL, or session. SSH supplies authentication, encryption, and host verification.
- The dashboard API includes writes. Access to the forwarded local port carries the remote Web process's authority.
- ExitOnForwardFailure=yes prevents the remote command from continuing when SSH cannot establish the forward.
- The remote command always includes --no-open.
- Every shell-interpolated selector is POSIX-quoted.

The explicit 127.0.0.1 listener keeps the local forward loopback-only independently of the OpenSSH GatewayPorts default.

The Origin middleware remains browser-CSRF mitigation only and does not strengthen this tunnel boundary.

## Ownership and teardown

Attach mode owns only the local SSH forward. Closing it cannot signal the pre-existing remote Web process because no remote command was started.

Spawn mode uses -tt with null stdin. Terminating the local SSH child closes the PTY-backed remote session, causing the orbit web serve process started by that session to receive hangup and exit.

SshTunnel owns the child through RAII. Drop or explicit shutdown sends SIGTERM, waits briefly, then sends SIGKILL when needed. connect returns on Ctrl-C, SIGTERM, or child exit.

## Option behavior

- --port selects the local forwarded port.
- --remote-port selects the remote Web port and forward target.
- --workspace affects the remote server's default workspace only, and only when this invocation spawns it. A top-level --root is rejected: connect reads no local Orbit data directory.
- --global is forwarded in spawn mode for compatibility with older remote binaries; current Web serving is always multi-workspace.
- --no-open controls only the local browser.

Attach mode sends no remote command, so --workspace and --global cannot reconfigure an existing server.

## Failures and accepted races

- SSH exit 127: orbit is missing from the remote non-interactive PATH.
- SSH exit 255: host, authentication, configuration, or network connection failed.
- Other early exit: report the remote Web command and status.
- Probe timeout: not an error; proceed to spawn mode.
- Spawn readiness timeout: fail and tear down the SSH child.
- Local port selection is a time-of-check/time-of-use race; SSH fails loudly if another process claims the port.
- Two simultaneous connects can both miss an absent server and try to spawn; one remote bind may fail. This is surfaced rather than hidden.

The command requires a reachable remote host and a foreground tunnel. It provides no reconnect, offline view, replication, or cross-machine merge behavior.
