---
title: Federated MCP — Decisions
owner: grok
last_updated: 2026-09-19
last_validated: 2026-09-19
status: Draft
feature: federated-mcp
doc_role: decisions
type: design
summary: Standing rules for the federated MCP mux: destinations are configured, selectors use machine_id, routing fails closed, and a client's `--operator` propagates into every destination it opens.
tags: [federated-mcp, mcp, host-registry, multi-host]
paths: ["crates/orbit-mcp/**", "crates/orbit-registry/**", "crates/orbit-core/**"]
related_features: [federated-mcp, host-registry, mcp-bridge, remote-access]
related_artifacts: [ORB-12564, ORB-12563, ORB-11184, ORB-11053, ORB-11052, ORB-11044, ORB-11023, ORB-11010, ORB-11009, ORB-11008]
---

# Federated MCP — Decisions

Record non-obvious decisions here by title. These are Door 2 standing rules. Code anchors: `crates/orbit-mcp/src/federated/` (`FederatedMcpHost`, destinations file, host-qualified selector, live probe, fail-closed routing). See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions).

## Federated MCP is a mux of operator-configured destinations

**Recorded:** 2026-08 · [ORB-11009] · [ORB-11010] (PR #1139)

### Context

A single MCP namespace can look like a fleet catalog. Host-registry already owns machine-local identity and checkout roles. Growing that catalog into routing, or auto-discovering owners, would make the gateway a second inventory and a placement service.

### Decision

Treat the federated surface as a mux in front of destinations the operator already configured. It is not a host-registry evolution, not a new fleet inventory, and not automatic owner discovery. Direct SSH stdio to one chosen host remains v1. Apply this whenever a future change is tempted to register, probe, or place hosts inside the gateway.

### Consequences

- Destination membership is an operator configuration problem, not a catalog schema problem.
- Cost: the mux cannot "just find" an owner or a healthy replica; a missing destination is a configuration gap, not a discovery miss.

## The accepting machine is an implicit local destination

**Recorded:** 2026-08 · [ORB-11044]

### Context

Every `mcp-destinations.toml` row required `ssh` and `machine_id`, so workspaces owned by the machine running `orbit mcp serve --mode federated` were absent unless the operator configured loopback SSH. A machine-id-only row for that host was rejected as a missing `ssh` field. Local federation then depended on Remote Login, SSH authentication, non-interactive PATH, and another process boundary.

### Decision

Always include the accepting machine as a local destination, keyed by its existing stable `machine_id` and listed from its workspace registry. Local selectors keep the host-qualified `hm_…/ws_*` shape and are delivered through the local MCP host in-process — never over SSH. `mcp-destinations.toml` remains the declaration surface for additional SSH remotes. A missing file or empty remote list is a valid local-only federated server. Local workspaces require no destination row; a machine-id-only row is still invalid and fails closed. If a valid configured row already names this machine, expose exactly one route for that identity (the local in-process destination) rather than duplicate selectors or open loopback SSH.

Rejected alternatives: treating a machine-id-only TOML row as local membership (the operator file would then describe both remotes and this host, and a typo would silently change routing); keeping loopback SSH as the local path (that is the problem being removed).

### Consequences

- A federated session on a machine with no destinations file still lists and routes that machine's workspaces.
- Cost: an operator who previously pointed an SSH row at this machine no longer gets a second, SSH-backed route for the same `machine_id`. Compatibility is "one local route", not "preserve loopback SSH".

## Host-qualified selectors are structured and caller-uninterpreted

**Recorded:** 2026-08 · [ORB-11009] · [ORB-11010] (PR #1139)

### Context

`host_id` is renameable display. Keying a route on it would invalidate every selector on `orbit host rename` and invite examples such as `orbit-linux/ws_orbit`. Treating the token as a formless blob hid that the encoding `hm_<id>/ws_*` is normative. The gateway's local catalog is the wrong resolution authority for another machine's workspace.

### Decision

Key the host-qualified selector on stable `machine_id` (`hm_…`). Encoding `hm_<id>/ws_*` is normative. Callers treat the token as **structured, caller-uninterpreted**: they must not parse it, must not construct it from `host_id`, and must not concatenate remembered `machine_id` and `id` values. The only caller-facing way to obtain a selector is to copy the `selector` field from federated `orbit_workspace_list`. Federated `tools/list` must say so and must not present cwd, a registered name, or a bare `ws_*` as valid. The gateway must not reinterpret it against its own local catalog. A token that is not uniquely host-qualified (a bare `ws_*`, including a v1 session default) is `unknown_selector` before forwarding. Federated `orbit.task.show` requires the host-qualified selector. Duplicate `machine_id` across destinations is config-load `ambiguous_destination`, not a per-call outcome. Apply this to every new selector encoding, including future transport wrappers.

### Consequences

- Renames do not rebind routes; callers can persist selectors across display-name changes.
- Cost: humans cannot mint a selector from a hostname they remember, and they cannot assemble one from listed `machine_id` + `id` either; they must copy the list `selector` field.

## Capability class is assigned by tool behavior, held by catalog role

**Recorded:** 2026-08 · [ORB-11009] · [ORB-11010] (PR #1139)

### Context

One namespace plus several checkouts of one repository looks like a synchronized task store unless capability and authority are named separately. Lumping "mutations" would send `orbit_task_add` to a replica, or silently fail over to the owner, and invent a second ownership model. Classifying only `orbit_task_add` would leave every other tool for an implementer to guess. Treating list advertisement as the source of truth is circular because advertisement can lag Core, and `owner_machine_id` is `Option`.

### Decision

Assign capability class by what the tool does: task issuance and coordination-store writes are `control_plane`; tools that touch runs, logs, or scheduler state are `execute`; discovery and list tools are unclassified and are not subject to `capability_refused`. Do not add a per-tool registry field for this. The destination's **local catalog role** determines which classes that destination holds; list advertisement is a hint that may lag. Destination Core refusal is the correctness boundary. Owner checkout holds `control_plane` and may also hold `execute` when it runs locally — that second class is not a refusal input. Replica checkout holds `execute`. A workspace with absent `owner_machine_id` cannot advertise `control_plane`. Destination-host Core refuses the other class with `capability_refused` and no implicit failover. Split remaining authority: the destination host owns runs, logs, and scheduler state; the declared control-plane owns task issuance and the coordination store. Apply this to every new federated tool, not only `orbit_task_add`.

### Consequences

- Owner/replica remain the only ownership vocabulary; execute-class work can stay on a replica without cloning the coordination store.
- Cost: a caller that picks the wrong selector gets a named refusal instead of a successful write on the "right" host; clients must route control-plane tools to a `control_plane` destination themselves, and they cannot trust a stale list advertisement over the destination refuse.

## Unreachable destinations stay in the list and routing fails closed

**Recorded:** 2026-08 · [ORB-11009] · [ORB-11010] (PR #1139)

### Context

Omitting a down host from `orbit_workspace_list` makes every later call a stale-route surprise. Calling the federated list "additive" hid that v1 puts `machine_id` on the envelope and filters to Active-and-locally-checked-out workspaces. Overloading one `health` field hides whether SSH failed or the repo root is gone. Falling back to another host with the same `ws_*` is a replica protocol by another name. Overlapping error classes without precedence would let an implementation pick whichever name was convenient.

### Decision

Federated `orbit_workspace_list` is a new session-unbound shape, not a compatible extension of v1: `machine_id` lives on each descriptor, not the envelope, and the v1 Active-and-locally-checked-out filter is not inherited. Configured workspaces on unreachable or inactive destinations are included. Host-reachability and checkout-health are separate fields. Routing decides on live delivery, not cached list health. Caller-facing precedence is `unknown_selector` → `ambiguous_destination` (config) → `unreachable_destination` → `stale_route` → `unhealthy_checkout` → `tool_not_on_this_host` → `capability_refused`. Unreachable wins over capability and stale because those are undecidable without the host. `tool_not_on_this_host` is distinct from `unknown_selector`. No local fallback, no default workspace, no cached host-local runtime. Probe cadence stays a vision open question. Apply this to every new federated discovery field and every new routing miss.

### Consequences

- Callers can distinguish "host down" from "checkout missing" from "tool not advertised here."
- Cost: the list is not a set of live, callable workspaces; clients must read reachability and capabilities before routing, and availability is bounded by the chosen destination.

## Routed delivery has its own budget, and a lost answer after dispatch is `outcome_unknown`

**Recorded:** 2026-08 · [ORB-11023]

### Context

Routing reused the discovery probe's single session-wide deadline, stamped once at session start. A routed call spends that budget on four remote round trips — SSH spawn plus `initialize`, discovery, `tools/list`, then `tools/call` — before the tool runs, so any routed tool slower than the residue could not complete over the mux at all. The mux advertises the whole canonical surface, including `orbit.command.exec` and `orbit.workflow.ship`. Worse, exhausting that deadline produced `unreachable_destination` for a destination that was healthy and actively executing, and killing the SSH child does not undo a write the destination already committed.

### Decision

Classification and delivery are budgeted separately. The probe budget bounds SSH setup, the handshake, discovery, and `tools/list`; the routed `tools/call` is stamped with its own, larger budget at the moment its request is written. A lost answer after that write — budget exceeded or session ended — is `OrbitError::OutcomeUnknown` (`outcome_unknown`), carrying the destination-facing request identity. A loss before the write, including a failed write, stays `unreachable_destination`. `outcome_unknown` is a post-dispatch outcome and does not join the fail-closed precedence ladder. Apply this to every new federated request: read-only classification phases are `unreachable`; anything that can commit on the destination is `outcome_unknown` once its bytes are on the wire.

### Consequences

- A caller can distinguish "the host never got it, retry" from "it may have run, reconcile before retrying," so a timed-out `orbit.task.add` is no longer an invitation to create a duplicate.
- Long-running routed tools become usable over the mux; a slow destination no longer converts into a false unreachable.
- Cost: both budgets are constants rather than per-destination configuration, so an operator still cannot tune one destination separately. Rejected for now because no second, differing use exists; a `Destination` timeout field can be added when one does.
- Cost: `outcome_unknown` gives the caller no verdict. That is the honest report — the mux cannot learn one after the transport is gone — but it does move reconciliation to the caller.

## Single control-plane per repository is operator configuration

**Recorded:** 2026-08 · [ORB-11010] (PR #1139)

### Context

Owner role is machine-local. Two hosts can each declare Owner. Independently inited checkouts have different `ws_*`, so the mux cannot observe the collision without the fleet discovery this design forbids. Listing "competing authorities" as a mux-enforced non-goal would make an implementer invent detection the architecture cannot perform.

### Decision

A single control-plane per repository is an operator configuration responsibility, not a mux invariant. The mux does not detect competing owners and does not raise an error when they exist. The would-be signal is matching `git_remote` across destinations with differing `owner_machine_id`. A violation surfaces as two independent control planes, not an error. Apply this whenever a future change is tempted to compare `git_remote` values inside the gateway or to refuse a second Owner.

### Consequences

- Operators who configure one owner per repository get one control plane; that is a deployment rule, not software.
- Cost: a misconfigured pair of destinations will accept conflicting task issuance with no mux warning.

## The mux is a federated-namespace exception to v1 no-relay

**Recorded:** 2026-08 · [ORB-11009]

### Context

mcp-bridge v1 forbids an Orbit process relaying a call onward and requires a byte-transparent SSH proxy. A mux necessarily inspects the selector and forwards. Treating that as a general relay product, or leaving vision §5 as "multi-host routing is a separate product," would either block the surface or un-forbid owner discovery, replication, and fleet placement.

### Decision

Admit the mux as an explicit exception to v1 byte-transparent / no-relay rules **for the federated namespace only**. Automatic owner discovery, replication, relays-as-product, and fleet placement stay out. v1 current-behavior docs continue to describe v1. Apply this whenever a later change wants the proxy to inspect, filter, or redirect traffic outside the federated namespace.

### Consequences

- Implementation can build the mux without rewriting mcp-bridge 2_design as if v1 already federated.
- Cost: two MCP entry shapes must be documented and tested — v1 direct SSH stays policy-free; only the federated namespace may route — and mixed use cannot leak mux policy into the byte-transparent proxy.

## An MCP session's authority is declared by the destination, not requested by the caller

**Superseded by:** [An SSH login to a destination is ownership of it](#an-ssh-login-to-a-destination-is-ownership-of-it)

**Recorded:** 2026-08 · [ORB-11052]

**Code anchors:** `crates/orbit-mcp/src/remote/callers.rs` (`SessionCapabilityPolicy`, `CallersFile`), `crates/orbit-mcp/src/remote/identity.rs::mcp_serve_session_policy`, `crates/orbit-common/src/governance/authorization.rs::CallerProvenance::RemoteGrant`

### Context

Two axes decide whether a federated call runs, and only one of them was answered by the machine that executes the work. Capability class (`control_plane` / `execute`) was already destination-derived in `federated/capability.rs`. Session authority (`agent` / `operator`) was not: `orbit mcp serve --operator` resolved it once at startup from argv, and on an SSH destination the *caller* writes the remote argv. Anyone with shell access wrote `ssh <host> "orbit mcp serve --operator"` and stamped their own session `Operator`, satisfying every entry in `GOVERNED_OPERATIONS` — `orbit.command.exec`, `orbit.workflow.ship`, `orbit.task.delete`, `orbit.workspace.claim.release`. `--remote-caller-machine-id` was an audit label nothing read as an authorization input.

### Decision

An MCP session's authority is **declared by the machine that executes the work**. A remote-originated session's argv becomes a *request*; the destination's machine-global `~/.orbit/mcp-callers.toml` is the *ceiling*; the session holds `requested ∩ granted`. Apply this to any future signal that would decide what a session may do: it is admissible only if the executing machine can observe it without trusting the caller to supply it.

Four rules make that operational, and each is the part a future change is most likely to erode:

1. **Origination is the destination's observation.** A session is remote-originated when the *serving process* sees `SSH_CONNECTION` and stdin is not a terminal. Keying on `--remote-caller-machine-id` instead would be bypassable by not passing the flag, which would present a remote session to the destination as a local one.
2. **Intersection only, never union.** The file can lower a session and can never raise one, so it opens no privilege path and cannot escalate a session that did not ask. A caller granted `[agent, operator]` that omitted `--operator` still resolves to `agent`.
3. **Ambiguity falls to the file default, never back to argv.** An absent, malformed, or unmatched caller label selects no row and takes `default`. Falling back to the request is the escalation being closed.
4. **A grant is recorded beside the effective set, not folded into it.** `CallerProvenance::RemoteGrant` separates "this destination granted it" from a local `Session` stamp, and the authorization audit row carries the resolved caller and granted set alongside the effective one.

Local (non-remote-originated) sessions are untouched: argv stays authoritative and today's accident-guard model holds byte for byte.

Rejected alternative: **keep argv authoritative and gate on the forwarded `machine_id` allowlist alone**, treating the label as the identity. That was on the table and is materially different — it needs no new file and no origination check — but it authorizes on a value the caller writes, so it moves the label from "audit" to "credential" while leaving the escalation exactly where it was. Also rejected: a **compatibility window** in which a missing callers file preserves the old behavior. A phased default would leave the escalation open for the length of the phase while implying it was closed; the migration is instead a deliberate downgrade that cuts operator-over-SSH on first upgrade.

Tier 2 — binding the caller identity to the SSH key via an `authorized_keys` forced command — shipped separately [ORB-11053] and is recorded below. On a destination that has not opted into it the identity here is still self-asserted, so *that* configuration remains an accident guard in keeping with the governance kernel's doctrine and must not be described in code or docs as a security boundary.

### Consequences

- The escalation is closed for the case it actually occurred in: a caller can still write any argv it likes, and the destination no longer honors it.
- The two axes compose without either learning about the other. Capability class stays in `federated/capability.rs` and `capability_refused`; session authority stays in the governance kernel and `capability_denied`. A call must clear both.
- The resolution stays in `orbit-mcp` rather than moving to `orbit-core`, because it composes the session envelope the way `--operator` always did; the *decision* remains the kernel's `authorize`. The crate keeps its rule that a protocol crate does not decide whether a call is allowed.
- Cost: **the first upgrade breaks working operator-over-SSH setups.** Every destination that serves remote sessions needs a callers file before an operator can dispatch a workflow over SSH again. That is the intended direction, but it is a real outage for anyone who has one, mitigated only by a startup warning and `orbit mcp callers init`.
- Cost: **`workspaces` narrowing is re-evaluated per call**, so a session's capabilities are no longer a single fact resolved at establishment. Every future call path that resolves a workspace on the destination has to stamp the narrowed set, and one that forgets silently serves the session's unnarrowed ceiling.
- Cost: **the caller identity is self-asserted unless the destination opts into Tier 2.** A caller that can reach such a destination can name a different row. The file is then strictly stronger than a caller-authored grant and strictly weaker than an authenticated one, and the gap stays legible in the audit trail rather than assumed away.

## A caller identity is only as strong as the key sshd checked for it

**Superseded by:** [An SSH login to a destination is ownership of it](#an-ssh-login-to-a-destination-is-ownership-of-it)

**Recorded:** 2026-08 · [ORB-11053], corrected by [ORB-11057], [ORB-11134], and [ORB-11184]

**Code anchors:** `crates/orbit-mcp/src/remote/ssh_auth.rs` (`SshAcceptance`, `issue_ssh_acceptance`, `verify_ssh_acceptance`), `crates/orbit-mcp/src/remote/callers.rs` (`RemoteCallerIdentity`, `enforce_key_binding`), `crates/orbit-cli/src/command/mcp/callers.rs::authorize`, `orbit_types::tool::CallerIdentityProof`

### Context

Tier 1 moved the authorization *statement* to the destination but still keyed it on `--remote-caller-machine-id`, a label the caller types. A caller that can reach the destination can therefore name a different row, which is why that tier is an accident guard rather than a boundary. ORB-11053 tried to close this by treating public forced-command flags as proof of their origin; ORB-11057 established that caller-controlled argv can reproduce those flags and a copied fingerprint. ORB-11134 moved the corrective bearer from argv to the forced-command environment, but ORB-11184 established that a first-line Rust `PR_SET_DUMPABLE=0` still leaves the interval from `execve` through dynamic loading and runtime startup readable to a same-UID scanner. The corrected boundary must exist before the capability-bearing process begins userspace execution.

### Decision

Delegate the identity to sshd, which already authenticates the key. Let the *destination* compose the argv that names the caller, while sshd supplies the bearer in a per-key environment:

```
environment="ORBIT_MCP_SSH_ACCEPTANCE=<destination-token>",command="/usr/local/libexec/orbit-mcp-ssh mcp serve --accept-ssh --caller hm_alpha",no-pty,… ssh-ed25519 AAAA… caller@box
```

Four rules make that operational:

1. **The destination composes the argv; the caller's command is ignored entirely.** `SSH_ORIGINAL_COMMAND` is never parsed, merged, or used to derive a requested authority. Only its presence is logged, so the trail shows an override happened without the content ever reaching a decision.
2. **An identity without a protected destination-issued capability is unrepresentable.** `--accept-ssh` is a valueless marker. The dedicated account uses a root-owned Orbit copy with mode 2555 and a private, privilege-free group different from its primary group as its login shell. This is load-bearing: sshd always starts a forced command as `<login-shell> -c <command>`, so making only the eventual command setgid would still expose the bearer in an ordinary shell. The login-shell credential transition makes Linux apply its secure-exec dumpability policy inside `execve`, before the first bearer-bearing dynamic loader or Rust startup. Orbit verifies the inherited state, permanently drops the launch group, recognizes the exact generated `-c` shape without a second exec, then sets `PR_SET_DUMPABLE=0` as defense in depth. A normal executable, `NoNewPrivs`, `fs.suid_dumpable=1`, missing bearer, wrong digest, or caller mismatch refuses before selecting a row. `--caller-key-fingerprint` remains refused.
3. **The capability is bound to the emitted key.** Its destination record stores the public-key fingerprint. A callers-file pin mismatch refuses at session establishment; copied fingerprint argv is not an observation. `SSH_USER_AUTH` remains optional Tier 1 evidence and does not turn caller-selected argv into Tier 2.
4. **Which tier answered is recorded, not assumed.** `CallerIdentityProof` (`key-bound` / `self-asserted`) rides in `RemoteCallerGrant` into the authorization audit row's `arguments_json`. Both tiers produce identical-looking grants once resolved, so a trail without this field would leave a reader guessing whether the caller had to hold a key.

Orbit renders the authorized-keys line and never installs it. The bearer-bearing entry must live in a root-owned `AuthorizedKeysFile` the login account cannot read, and its `environment=` option requires `PermitUserEnvironment yes` scoped to the account. `authorize` also requires an absolute `--launcher` path; verifies mode 2555, a group different from the account's real group, byte equality with the running Orbit binary, and that the system account database names it as the login shell; then emits that path. The installed launcher must be root-owned and its group must have no members, file privileges, or service privileges; the credential transition exists only to make the kernel protect process metadata. On every Orbit upgrade, replace the launcher with the new binary and re-run `authorize`. Re-authorizing rotates the capability digest immediately, so replacing the root-managed line is part of the same operation.

Rejected alternative: **call `prctl` earlier in `main` or a userspace constructor.** Every such call still runs after `execve` created readable process metadata, so it can reduce but cannot close the race. Also rejected: **require a host-wide Yama policy** such as `ptrace_scope=2`. It would protect this launch, but changes debugging policy for every process on the host rather than placing the rule at the Tier 2 executable boundary. Orbit still does not manage `authorized_keys` directly: a task tool must not own machine login or risk locking an operator out. Tier 2 remains opt-in because it requires sshd and launcher configuration the operator may not control.

### Consequences

- [3_vision.md §1](./3_vision.md) closes with evidence rather than by assertion: `crates/orbit-cli/tests/mcp_roundtrip.rs` runs a separate ordinary same-UID scanner continuously across repeated generated protected launches, retains a valid key-bound grant, and proves an invocation copied from observable metadata is refused.
- The boundary is real for the SSH transport only. `orbit mcp listen` authenticates nobody and keeps its hardcoded `agent`; no registry or session field was promoted into a credential to get here.
- `orbit doctor` gains two machine-global rows (`mcp-callers`, `mcp-caller-keys`), composed in `orbit-cli` because `orbit-cmd` does not know about MCP and must not learn. Both are warnings: an opt-in tier not taken up is not a broken machine.
- Cost: **`ssh_key_fingerprint` now enforces where it previously only parsed.** A destination that wrote the field speculatively under Tier 1 and cannot observe its callers' keys is unaffected, but one that *can* observe them will start refusing any caller whose row records a stale or wrong fingerprint. That is the intended direction and it is a behavior change on existing files.
- Cost: **the strongest guarantee depends on root-managed sshd and a refreshed protected launcher Orbit does not install.** The root-owned key file, `PermitUserEnvironment`, forced-command-only public-key authentication, root-owned launcher, private privilege-free setgid group, and `fs.suid_dumpable` other than 1 are deployment requirements. A stale or unhardened launcher fails closed, but an upgrade is not complete for Tier 2 until the protected copy and rotated root-managed line are replaced.
- Cost: **two similar-looking flags now exist.** `--remote-caller-machine-id` (Tier 1 audit label, hidden) and `--caller` (Tier 2 identity) both name a machine. They are not interchangeable, and a future change that merges them would silently reopen the escalation.

## An SSH login to a destination is ownership of it

**Recorded:** 2026-09 · [ORB-12564]

**Code anchors:** `crates/orbit-mcp/src/remote/proxy.rs::remote_serve_command`, `crates/orbit-mcp/src/remote/identity.rs::mcp_server_identity`, `crates/orbit-mcp/src/remote/legacy.rs`, `crates/orbit-common/src/governance/authorization.rs::agent_context_declared`

### Context

`orbit mcp serve --federated --operator` on the Mac held no operator authority on any destination. The remote argv carried no `--operator`, and the destination computed `requested ∩ granted` against a callers file or a forced command, so operator on a remote needed a per-destination setup: a row keyed on the caller's `hm_*` id, or a dedicated login UID, a setgid launcher, a forced command, and an acceptance digest. In practice it failed silently — the 0.20.0 QA sweep found an unparseable `mcp-callers.toml` on the Mac and every remote session downgraded with nothing to point at.

The ceiling was also stored in a file the caller could edit. Anyone who can run `ssh box "orbit mcp serve --operator"` can equally run `ssh box "ORBIT_OPERATOR=1 orbit tool run …"`, `rm`, `git`, or rewrite `mcp-callers.toml` to grant themselves a row. Tier 2 was the machinery that would make the ceiling real on a *multi-tenant* destination. Orbit has no such deployment: it is a single-user tool, and the accounts on both ends are the same person's.

### Decision

**An SSH login to a destination is ownership of it.** The destination honors the authority in the argv it was started with, for a remote-originated session exactly as for a local one. A federated or remote-proxy client started with `--operator` composes `orbit mcp serve --operator --remote-caller-machine-id <id>` for every destination it opens; started without it, the remote argv stays `agent`. The spec's claim that the accident-guard doctrine "does not carry across a machine boundary" had it backwards: SSH authenticating the caller is exactly why the far side needs no second authorization statement.

Three rules make that operational:

1. **Orbit governs agents, not people, and the guard is caller-side.** `remote_serve_command` refuses to propagate operator when the composing process declares itself an agent or a managed run (`agent_context_declared`), so a server an agent launched cannot hand operator authority onward to another machine. `child_env` still strips `ORBIT_OPERATOR` from agent children, and sandbox network policy is the backstop. One chokepoint covers both client paths — the v1 proxy and the federated mux — because two would eventually disagree.
2. **The label stays a label.** `--remote-caller-machine-id` is attribution: it marks the session's transport as SSH, names the calling machine in the destination's `authorization` audit rows and in a trusted-host admission, and contributes nothing to any decision. It is not renamed, merged with a credential, or promoted.
3. **The deny case is `authorized_keys`.** Removing a key is the only boundary this machine ever had. If per-workspace narrowing of remote operator is wanted later, it returns as a small opt-in on the *caller* side, not as a destination default.

What is kept is what actually describes something the destination can observe: capability class (`control_plane` / `execute`) and workspace scope describe the checkout, not the caller; the `authorization` audit rows; and `orbit mcp listen` staying agent-only, because a socket authenticates no client and that reasoning is untouched by any of this.

Rejected alternative: **fix the propagation and keep the file as an optional ceiling.** It would have satisfied the immediate bug while leaving every destination a setup step, a doctor row, and a silent-downgrade failure mode for a ceiling nothing can enforce. Also rejected: **a compatibility window** in which a present callers file still caps a session. A file that sometimes decides is worse than one that never does; migration is instead one `warn!` at startup and a `orbit doctor` row telling the operator to delete it.

This is the same rationale as [ORB-12563] on the dashboard: an authorization statement that the actor can rewrite is documentation, not a boundary, and pricing it as a boundary distorts everything built on top.

### Consequences

- `orbit mcp serve --federated --operator` yields operator-capable sessions on every reachable destination with no per-destination configuration. `orbit_agent_invoke`, `orbit.workflow.ship`, `orbit.task.delete`, `orbit.command.exec`, and `orbit.workspace.claim.release` work remotely.
- `crates/orbit-mcp/src/remote/callers.rs` and `ssh_auth.rs` are gone, with `orbit mcp callers`, `--accept-ssh`, `--caller`, `ORBIT_MCP_SSH_ACCEPTANCE`, `~/.orbit/mcp-ssh-acceptance/`, the setgid login-shell launcher, `CallerIdentityProof`, `RemoteAgentInvokeMode`, `RemoteCallerGrant`, and `CallerProvenance::RemoteGrant`. Roughly 2,500 lines of authorization machinery leave with them.
- Trusted-host admission is `operator`, full stop. The durable admission keeps `caller_machine_id` for attribution and no longer records an identity proof or a trust mode, because there is only one.
- Cost: **a destination is as exposed as its `authorized_keys` and no more.** That was already true — the file was editable by the same login — but it is now stated rather than obscured by a ceiling that implied otherwise.
- Cost: **an operator who wants a genuinely narrower remote surface has nothing to reach for.** The answer is a second SSH key and account, or not granting the login; Orbit deliberately does not reimplement multi-tenancy.

## Task References

- [ORB-11008] — recorded the prior federated MCP policy that these rules implement
- [ORB-11009] — recorded these standing rules as the contract home (PR #1139)
- [ORB-11010] — closed the PR #1139 review holes (selector wording, tool class, error precedence, competing authorities)
- [ORB-11044] — implicit local membership for federated serve
- [ORB-11052] — destination-side caller authorization, Tier 1 (the callers file)
- [ORB-11053] — key-bound caller identity, Tier 2 (the `authorized_keys` forced command)
- [ORB-11184] — kernel-protected Tier 2 exec boundary before userspace startup
- [ORB-12564] — argv-propagated remote operator; destination-side caller authorization removed
- [ORB-12563] — the same rationale applied to the dashboard

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
