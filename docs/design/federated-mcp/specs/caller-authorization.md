---
type: design
summary: "Spec: destination-side caller authorization — the callers file (Tier 1), explicit cooperative host invocation, and key-bound caller identity via an authorized_keys forced command (Tier 2)."
last_validated: 2026-09-07
title: Spec — Destination-side caller authorization
owner: claude
status: Draft
feature: federated-mcp
tags: [federated-mcp, mcp, mcp-bridge, authorization, spec]
related_features: [federated-mcp, mcp-bridge, host-registry, mcp-session-context]
related_artifacts: [ORB-11184, ORB-11134, ORB-11053, ORB-11052, ORB-11044, ORB-11023, ORB-11017, ORB-11015, ORB-11013, ORB-11012, ORB-11010, ORB-11009, ORB-11008]
---

# Spec: Destination-side caller authorization

The authority an MCP session holds on a destination is **declared by that destination**, not requested by the caller. A remote-originated session's `--operator` argv becomes a *request*; the destination's machine-global callers file is the *ceiling*; the session's effective capabilities are the intersection. **Tier 1 is implemented** [ORB-11052] and so is **Tier 2** [ORB-11053], but they are different guarantees and must not be conflated: Tier 2 is opt-in per destination, and which one answered a given call is recorded, never assumed. This contract governs session authority (`agent` / `operator`) only, and does not touch capability class (`control_plane` / `execute`), which is already destination-derived and specified in [federated-workspace-mcp.md](./federated-workspace-mcp.md).

## Why This Exists

Two independent axes decide whether a federated call runs. Both are now answered by the machine that executes the work; before [ORB-11052], only the first was.

| Axis | Question | Decided by, today |
|---|---|---|
| Capability class | Is this checkout a control plane, an execution binding, or neither? | The destination's own catalog role — `CapabilityClasses::for_checkout` in `crates/orbit-mcp/src/federated/capability.rs`, enforced before the tool body runs in `crates/orbit-cli/src/command/mcp/server.rs` |
| Session authority | May this caller perform governed operations at all? | The **destination's callers file** for a remote-originated session, intersected with the caller's argv request — `crates/orbit-mcp/src/remote/callers.rs`, resolved at session establishment by `mcp_serve_session_policy`. Before [ORB-11052] this was the caller's argv alone. |
| Caller identity | Which row may this caller select? | The caller's own `--remote-caller-machine-id` label under Tier 1; under Tier 2 the identity this destination wrote beside a key in its own `authorized_keys`, which sshd authenticated before running the forced command — `crates/orbit-mcp/src/remote/ssh_auth.rs` [ORB-11053]. |

On an SSH destination the caller writes the remote argv. Before Tier 1, any caller with shell access therefore wrote `ssh <host> "orbit mcp serve --operator"` and stamped its own session `Operator`, which satisfies every entry in `GOVERNED_OPERATIONS` — `orbit.command.exec`, `orbit.workflow.ship`, `orbit.task.delete`, `orbit.workspace.claim.release`. The destination consulted no caller identity when deciding this. `--remote-caller-machine-id` remains an audit label the proxy forwards, explicitly "not an authenticated principal"; under Tier 1 it selects a row and still grants nothing. Under Tier 2 the row is selected instead by `--caller`, which is not the caller's to write: it comes from the destination's own `authorized_keys` line, and sshd runs that line only for whoever holds the key beside it.

This is the unresolved transport-authentication question in [3_vision.md §1](../3_vision.md), narrowed to the part that can be answered without a fleet registry: not "who is this caller, globally" but "what has *this destination* agreed to serve *this caller*".

## Scope, and the boundary this does not claim

[`crates/orbit-common/src/governance/authorization.rs`](../../../../crates/orbit-common/src/governance/authorization.rs) states the governance kernel is an accident guard, not a security boundary, because every agent on a development box runs as one OS user and can bypass Orbit with `git` and `rm`. That reasoning is local. It does not carry across a machine boundary: a caller on another host does *not* already have shell on the destination except through SSH, and SSH authenticates.

This spec therefore has two tiers, and they must not be conflated:

- **Tier 1 — the callers file (implemented, [ORB-11052]).** Moves the authorization *statement* to the destination. The caller identity it keys on is self-asserted, so Tier 1 alone remains an accident guard, in keeping with the kernel's doctrine. It is still strictly stronger than the prior behavior, where the caller's request *was* the grant.
- **Tier 2 — key-bound caller identity (implemented, [ORB-11053], hardened by [ORB-11057], [ORB-11134], and [ORB-11184]).** Makes the identity authenticated by delegating key admission to sshd and requiring a destination-issued forced-command capability before Orbit honors the caller name. The bearer is absent from argv and accepted only when a Linux setgid exec protected the initial environment before any userspace startup. It is opt-in per destination and requires a dedicated login UID plus the protected launcher described below; a destination running Tier 1 alone stays valid, with the weaker guarantee stated above.

Tier 2 persists only a SHA-256 capability digest under `~/.orbit/mcp-ssh-acceptance/`; the bearer value exists only in the root-managed authorized-keys entry. Orbit does not hold the caller's SSH private key or any reusable login credential.

## The callers file

Destination-side authorization is declared in the machine-global operator file `~/.orbit/mcp-callers.toml`. It is the mirror of `~/.orbit/mcp-destinations.toml`: destinations declare who this machine may call, callers declare who may call this machine and as what. Both files are machine-global; neither belongs in workspace `config.toml`, `workspaces.json`, or host-registry.

```toml
# Capabilities served to a caller that matches no row below.
# Permitted values: "agent" or "deny". Operator is never a default.
default = "agent"

[[callers]]
machine_id   = "hm_alpha"
label        = "daniels-mac-mini"
capabilities = ["agent", "operator"]

[[callers]]
machine_id   = "hm_beta"
capabilities = ["agent", "operator"]
workspaces   = ["ws_orbit", "ws_constellation"]
ssh_key_fingerprint = "SHA256:…"
agent_invoke = true

[[callers]]
machine_id   = "hm_cooperative"
capabilities = ["agent", "operator"]
workspaces   = ["ws_orbit"]
agent_invoke = true
agent_invoke_mode = "cooperative"

[[callers]]
machine_id   = "hm_gamma"
capabilities = ["agent"]
```

Row keys:

| Key | Required | Meaning |
|---|---|---|
| `machine_id` | yes | The calling machine's stable `hm_…` identity. Matched against the caller identity resolved below. |
| `capabilities` | yes | The ceiling this destination will serve that caller. Values are `agent` and `operator` only. |
| `label` | no | Operator-facing display name. Never an identity input. |
| `workspaces` | no | Narrows the grant to these logical `ws_*` IDs. Omitted means every workspace on this destination. |
| `ssh_key_fingerprint` | no | Binds the row to a key sshd authenticated, in the `SHA256:…` form `ssh-keygen -l` prints. See Tier 2. |
| `agent_invoke` | no | Explicitly grants remote trusted-host agent invocation on the operation's workspace scope. Requires `operator` and a workspace scope. |
| `agent_invoke_workspaces` | no | Narrows `agent_invoke` to logical `ws_*` IDs without narrowing ordinary capabilities. Omitted preserves the legacy `workspaces` scope. |
| `agent_invoke_mode` | no | Trust model for `agent_invoke`: omitted or `key-bound` preserves strict Tier-2 admission; `cooperative` accepts the existing same-OS-account SSH operator channel while retaining a self-asserted identity proof. Invalid without `agent_invoke = true`. |

File-level invariants:

1. A missing file is valid and means `default = "agent"` with no rows.
2. A duplicate `machine_id` makes the entire file invalid with `ambiguous_caller` at load, before any session is served — the same fail-closed shape `ambiguous_destination` has in the destinations file.
3. An unknown key, an unknown capability value, an empty `capabilities`, a `default` other than `agent` or `deny`, a malformed `machine_id`, or an `ssh_key_fingerprint` that is not a well-formed `SHA256:` digest invalidates the file at load. A malformed file is never served as if absent. The fingerprint format is checked here rather than at comparison time because a fingerprint in the wrong shape — an `MD5:` one, most plausibly — would otherwise never match and would present as a key mismatch on every session.
4. `runner` is not a grantable value. It is stamped in-process by a managed run and can never arrive over a transport.
5. Load happens once per server process, at startup, alongside identity resolution. A session's ceiling does not change under it mid-session.
6. `agent_invoke = true` additionally requires `operator` and an explicit workspace scope: `agent_invoke_workspaces`, or legacy `workspaces` when the new key is omitted. Both scopes reject empty or non-`ws_*` lists. Its default/key-bound mode also requires `ssh_key_fingerprint`. `agent_invoke_mode = "cooperative"` is the only mode that may omit the fingerprint; an unknown mode or invocation-only option without the operation grant invalidates the file.

## Remote origination is decided by the destination, not by argv

A session is **remote-originated** when the serving process observes `SSH_CONNECTION` in its own environment and standard input is not a terminal.

Both halves are load-bearing:

1. `SSH_CONNECTION` is set by sshd in the server process. A caller cannot forge it and, more importantly, cannot *omit* it. Keying on `--remote-caller-machine-id` instead would be bypassable by simply not passing the flag, which would present a remote session to the destination as a local one.
2. The non-terminal check separates the MCP transport from a person who SSH'd in and started a server by hand. The proxy argv is `ssh -T`, which never has a TTY; an interactive operator does. An operator whose interactive session is nonetheless piped is downgraded to the file's ceiling, which fails closed and is corrected by a row or by Tier 2.

A session that is not remote-originated is **local**, and its authority resolution is unchanged: the process owner is the caller, argv stays authoritative, and the accident-guard model holds unmodified. No local workflow changes.

Under Tier 2 the Tier 1 origination check is not consulted. The hidden `--accept-ssh` marker carries no value; the digest match uses `ORBIT_MCP_SSH_ACCEPTANCE`, which sshd installs with the generated per-key `environment=` option. The dedicated account's setgid Orbit login shell is therefore the first process to receive that environment, causing Linux to protect it in the kernel's exec path. Orbit then verifies the inherited non-dumpable state and real/effective group transition before reading the bearer. Its first-line `PR_SET_DUMPABLE=0` reinforces this state but does not and cannot close a pre-main window by itself. Public flags, copied fingerprints, and ordinary SSH-shaped environment variables cannot select a Tier 2 row.

## Requested, granted, effective

For a remote-originated session:

```
requested = argv authority         (--operator ⇒ {agent, operator}; otherwise {agent})
granted   = matched row's capabilities, or the file default
effective = requested ∩ granted
```

Invariants:

1. **Intersection only.** The callers file can never raise a session above what its argv asked for. The change is downgrade-only in every direction, so it opens no new privilege path and cannot be used to escalate a session that did not ask.
2. **`--remote-caller-machine-id` selects a row; it never grants.** It stays an unauthenticated label. An absent, malformed, or unmatched label falls to `default` — it does not fall to the argv.
3. **`default = "deny"` yields the empty set**, and every governed operation therefore refuses, along with every ungoverned tool that requires `agent`. Ambiguity fails closed, matching `CallerCapabilities::resolve`.
4. **A `workspaces` narrowing is evaluated per call**, against the resolved workspace of that call, not at session establishment. A session may hold `operator` for one workspace and `agent` for another on the same destination. Outside the listed workspaces the row falls back to the file `default`, not to a fixed `agent` — otherwise a narrowed row under `default = "deny"` would grant more elsewhere than the file's own floor. A call that resolves no workspace takes the unnarrowed grant; every governed operation is workspace-scoped, so such a call is a discovery call the narrowing has nothing to say about.
5. **Resolution is session-only.** The MCP chokepoint's `CapabilityResolution::SessionOnly` is unchanged: `ORBIT_OPERATOR` in the destination's environment stays inert on the MCP surface, and must not become a way to re-raise a session the callers file capped.

## Operation-specific remote agent invocation

Remote trusted-host invocation adds admission rules after ordinary capability
resolution. Every remote caller needs `operator`, `agent_invoke = true`, and an
invocation scope covering the resolved logical workspace. `agent_invoke_workspaces`
keeps that exception independent from ordinary `workspaces`; omitting it preserves
the original behavior of using `workspaces` for both. The file default never grants
the operation, the admission covers one invocation, the timeout remains
bounded, ordinary job input cannot supply the reserved admission, and a run
carrying an admission cannot be resumed.

For the existing trusted Mac caller on `dk-server-1`, whose row already has
`capabilities = ["agent", "operator"]` and no `workspaces` narrowing, the
minimal nonbreaking cooperative configuration is:

```toml
agent_invoke = true
agent_invoke_mode = "cooperative"
agent_invoke_workspaces = ["ws_orbit"]
```

These keys are added to the existing `hm_ba054a1a8fbfb914` row; production
installation and reconnect remain an operator action.

Omitting `agent_invoke_mode` preserves the shipped strict behavior: the row
must pin a fingerprint and the session's identity proof must be `key-bound`.
The destination owner may instead declare `agent_invoke_mode = "cooperative"`.
That mode trusts the existing non-interactive SSH operator channel under one OS
account. It is useful where the cooperating remote operator already has the
same shell authority as Orbit and Tier-2's dedicated account, root-managed
authorized keys, and protected launcher would not add isolation required by
the workflow.

Cooperative mode is deliberately candid about its boundary. The caller's
`machine_id` remains self-asserted and another peer able to use the same SSH
account can name the row. The mode is an accident-prevention policy among
cooperating same-account operators, not protection from a malicious peer who
already controls that account's files and processes. It must never stamp or be
reported as `key-bound`, and it does not relax authorization for other
operations or workspaces.

## Tier 2: key-bound caller identity

Tier 1 leaves `machine_id` self-asserted. To make the caller identity authenticated, the destination pins it to the SSH key that authenticated, using a forced command in `authorized_keys`:

```
environment="ORBIT_MCP_SSH_ACCEPTANCE=<destination-token>",command="/usr/local/libexec/orbit-mcp-ssh mcp serve --accept-ssh --caller hm_alpha --operator",no-pty,no-port-forwarding,no-agent-forwarding,no-X11-forwarding ssh-ed25519 AAAA… caller@daniels-mac-mini
```

Invariants:

1. Under a forced command the destination composes its own argv. The caller's command arrives only as `SSH_ORIGINAL_COMMAND` and **is ignored entirely** — not parsed, not merged, not used to derive a requested authority. The generated line names `--operator`, so the destination-owned request is `{agent, operator}`; the matched callers-file row remains the grant ceiling, and intersection with an agent-only or deny grant still cannot yield operator. Its *presence* is logged, so the trail shows that something was overridden; its content never enters a decision.
2. `--accept-ssh` is a valueless marker. `--caller` is honored only after the protected `ORBIT_MCP_SSH_ACCEPTANCE` value matches the SHA-256 digest stored for the same machine ID. `SshAcceptance::Environment` has nowhere to hold a caller identity, while `ForcedCommand` carries both the caller and capability; merely typing the public flags cannot construct a trusted identity.
3. The acceptance record also carries the fingerprint of the public key beside which the capability was emitted. A matched row with a different `ssh_key_fingerprint` refuses the session at establishment. `--caller-key-fingerprint` is not accepted: a copied fingerprint is caller text, not an observation. `SSH_USER_AUTH` remains an optional Tier 1 observation source and is not what upgrades an argv identity to `key-bound`.
4. A pin is enforced under either tier. The operator wrote the fingerprint to have it checked, and a Tier 1 destination that happens to expose auth info can check it. What Tier 2 adds is that the *identity itself* stops being the caller's to choose. Being unable to observe the key at all is not a mismatch and serves the session with a warning; observing that *no* key authenticated is a mismatch and refuses it, so a password or keyboard-interactive login cannot collect a pinned row's grant on a destination that set `ExposeAuthInfo yes`.
5. Tier 2 is opt-in. A destination running Tier 1 alone is a valid, documented configuration with a weaker guarantee, and the difference is legible in the audit trail rather than assumed — see `caller_identity` below.
6. An acceptance invocation that omits `--caller`, lacks the credential-changing exec boundary, inherits ordinary dumpable state, presents an unknown token, runs on an unsupported host, or names a caller other than the token record refuses before loading a grant and before `CallerIdentityProof::KeyBound` can be stamped.

Prepare the launcher before authorization (choose the installed Orbit path appropriate for the host):

```sh
sudo groupadd --system orbit-mcp-launch
sudo install --owner=root --group=orbit-mcp-launch --mode=2555 \
  "$(command -v orbit)" /usr/local/libexec/orbit-mcp-ssh
sudo usermod --shell /usr/local/libexec/orbit-mcp-ssh <dedicated-account>
```

`orbit mcp callers authorize --machine-id <hm_…> --key <path> --launcher /usr/local/libexec/orbit-mcp-ssh` rotates the caller's acceptance capability, stores its digest and key fingerprint under `~/.orbit/mcp-ssh-acceptance/`, validates the launcher, and emits the operator-requesting authorized-keys line. The launcher must be a root-owned, mode-2555 byte-for-byte copy of the running Orbit binary, setgid to a private group different from the login account's primary group, and configured as that account's login shell. This last condition is load-bearing: sshd always starts a forced command as `<login-shell> -c <command>`, so a normal shell would receive the bearer before Orbit. The protected Orbit shell recognizes only the exact generated command shape and feeds it to normal CLI parsing without a second exec. Orbit permanently drops the effective and saved launch group before opening task data. The private group must still have no members and grant no access to files, sockets, or services: its only job is to make Linux take the protected credential-changing exec path. `NoNewPrivs` and `fs.suid_dumpable=1` disable the boundary and fail closed.

The line must live in a root-owned `AuthorizedKeysFile` the account cannot read (for example `/etc/ssh/authorized_keys/%u`), and its per-key environment requires `PermitUserEnvironment yes` in a scoped `Match User` block. The request remains capped by the callers-file row. Orbit does not edit or verify SSH login policy. After every Orbit upgrade, replace the launcher with the new binary while preserving its owner, group, and mode; then re-run `authorize` and replace the old root-managed line. Re-authorizing changes the stored digest immediately, so the old line stops authenticating before its file is replaced.

The rendered line carries `no-pty,no-port-forwarding,no-agent-forwarding,no-X11-forwarding` and the validated absolute launcher path: a forced command runs without a login shell's `PATH`, and the MCP transport is one non-PTY stdio pipe that needs none of the capabilities those options close.

## Errors, audit, and diagnosis

1. A denial names the file, the resolved caller, and the requirement: `caller 'hm_alpha' is granted [agent] by ~/.orbit/mcp-callers.toml on the machine that executes this call; 'orbit.command.exec' requires operator`. A refused caller must be able to act on the message without reading Orbit's source, and must not be advised a remedy it cannot reach — neither `ORBIT_OPERATOR` nor `--operator` on the calling side raises this ceiling.
2. A new `CallerProvenance::RemoteGrant` distinguishes "the destination's callers file granted this" from a local `Session` stamp. Provenance is recorded, never discarded, so the trail can separate the two.
3. The audit envelope records the resolved caller identity, the granted set, and the effective set. Recording only the effective set would make a downgrade indistinguishable from a caller that never asked. On the `command = 'authorization'` row the effective set is `capabilities_json` and `subcommand` is the provenance (`remote-grant`); the caller, granted set, file, and `caller_identity` are recorded together in `arguments_json`.
4. `caller_identity` is `key-bound` or `self-asserted`, and it is what keeps the two identity tiers apart in the trail. For trusted-host invocation, `agent_invoke_mode` independently records `key-bound` or `cooperative`; the durable admission and `trusted_host.execution_admitted` event retain both fields, the destination workspace, the authorizing operator, and the per-invocation working directory/bound. Recording only the grant or only the mode would leave a reader to assume how the caller was identified.
5. `orbit mcp callers list` prints the loaded rows and the default. `orbit mcp callers check <machine_id>` prints what a session from that caller would resolve to, without serving one, and says whether the row is key-bound or selected by a name alone.
6. Caller authorization is decided at **session establishment**, and the per-call `workspaces` narrowing at the existing governance chokepoint. Neither enters the routing precedence ladder in [federated-workspace-mcp.md](./federated-workspace-mcp.md), which classifies calls that have already been admitted.

## Surfaces this does not change

1. **`tools/list` does no capability filtering.** Placement is not permission; that invariant is unchanged, and a capped session still sees the full advertised surface and is refused at the chokepoint.
2. **The mux's client side.** `--mode federated` requests `agent` and should continue to. The mux is a caller here, and its request is capped by each destination independently.
3. **`orbit mcp listen`.** A TCP socket authenticates nobody, so its hardcoded `Agent` authority stays correct and must not be made configurable by this file.
4. **Capability class.** `control_plane` / `execute` and `capability_refused` are untouched. The two axes compose: a call must clear both.
5. **v1 `--mode remote` byte-transparency.** The proxy is unchanged. The destination-side resolution happens in the server the proxy reaches, not in the proxy.

## Migration

A missing callers file must not silently preserve today's behavior, because today's behavior is the defect.

1. First release: a missing file means remote-originated sessions resolve to `agent` only, and the server emits one warning at startup naming the file to create. This is a downgrade that **will** cut existing operator-over-SSH flows on the first upgrade, and that is the intended direction.
2. `orbit mcp callers init` seeds a file from the `machine_id`s already present in the local registry and destinations file, granting `agent` and leaving `operator` for the operator to add deliberately. Adding `operator` must remain an explicit edit; the seeder must never write it.
3. `orbit doctor` reports both gaps [ORB-11053], as two rows because they are two different conditions. `mcp-callers` warns when this machine accepts SSH logins — evidenced by a non-empty `~/.ssh/authorized_keys` — and has declared nothing about who may call it; it fails when the file exists and does not load, because every remote session is then refused. `mcp-caller-keys` warns when a row grants `operator` with no `ssh_key_fingerprint`: the strongest thing the file can say, resting on a name. Both are warnings rather than errors, because Tier 2 is opt-in and a deliberate Tier 1 destination is not broken. The rows are machine-global and are composed in `orbit-cli`, since `orbit-cmd` does not know about MCP and must not learn.
4. There is no compatibility window in which the caller's argv is honored on a remote-originated session. A phased default would leave the escalation path open for the length of the phase while implying it was closed.

## Agent Signature

Drafted by claude, 2026-08-29, from a read of `crates/orbit-mcp/src/remote/identity.rs`, `crates/orbit-mcp/src/federated/`, `crates/orbit-cli/src/command/mcp/`, and `crates/orbit-common/src/governance/authorization.rs`. Tier 1 implemented by claude, 2026-08-29 [ORB-11052]; the decision entry is [An MCP session's authority is declared by the destination, not requested by the caller](../4_decisions.md#an-mcp-sessions-authority-is-declared-by-the-destination-not-requested-by-the-caller). Tier 2 implemented by claude, 2026-08-29 [ORB-11053] in `crates/orbit-mcp/src/remote/ssh_auth.rs`, with its pre-userspace launch boundary corrected by codex, 2026-09-04 [ORB-11184] and demonstrated in `crates/orbit-cli/tests/mcp_roundtrip.rs`; its decision entry is [A caller identity is only as strong as the key sshd checked for it](../4_decisions.md#a-caller-identity-is-only-as-strong-as-the-key-sshd-checked-for-it). The operation-specific cooperative SSH mode and its truthful audit provenance were implemented by codex, 2026-09-07 [ORB-11496].
