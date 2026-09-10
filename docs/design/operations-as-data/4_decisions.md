---
title: Operations as Data — Decisions
owner: claude
last_updated: 2026-08-16
last_validated: 2026-09-10
status: Accepted
feature: operations-as-data
doc_role: decisions
type: design
summary: Decision log for the operations-as-data registry — the split spec/handler table, what stayed hand-written, and the touch-it-move-it ratchet.
tags: [operations-as-data, architecture, adr-0209]
paths: ["crates/orbit-common/src/governance/operation.rs", "crates/orbit-common/src/governance/authorization.rs", "crates/orbit-common/src/governance/friction/**", "crates/orbit-tools/src/builtin/orbit/tests/authorization.rs"]
related_features: [operations-as-data]
related_artifacts: [ORB-10358, ORB-10453, ORB-10478]
---

# Operations as Data — Decisions

> **Retired learning clauses:** [ORB-10736] / [Remove the native project-learning subsystem](../project-learnings/4_decisions.md#remove-the-native-project-learning-subsystem) removed the native
> project-learning CLI and tool operations. Learning-specific examples in
> earlier entries are retained as historical context only.

The parent bearing is **[North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry)** (north-star: operations as data behind an
operation registry), whose stored body now carries the friction pilot outcome and
the ratchet.

- **[Split spec/handler table joined by a typed verb enum](#split-spechandler-table-joined-by-a-typed-verb-enum) — Split spec/handler table joined by a typed verb enum** — Accepted.
- **[Renderers and HTTP routes stay hand-written](#renderers-and-http-routes-stay-hand-written) — Renderers and HTTP routes stay hand-written** — Accepted.
- **[Freeze the pre-migration surface as fixtures before migrating](#freeze-the-pre-migration-surface-as-fixtures-before-migrating) — Freeze the pre-migration surface as fixtures before migrating** — Accepted.
- **[Capability chokepoint for destructive operations outside MCP](#capability-chokepoint-for-destructive-operations-outside-mcp) — Capability chokepoint for destructive operations outside MCP** — Accepted.
- **[MCP advertisement is placement; the capability chokepoint is permission](#mcp-advertisement-is-placement-the-capability-chokepoint-is-permission) — MCP advertisement is placement; the capability chokepoint is permission** — Accepted.

## Split spec/handler table joined by a typed verb enum

**Recorded:** 2026-07-26 00:55:47.755882Z · [ORB-10358]
**Paths:** `crates/orbit-common/src/governance/operation.rs`, `crates/orbit-common/src/governance/friction/**`

### Context
 [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry) bearing 1 describes one operation table holding both the
serializable definition and its handler. Orbit's layering makes that
unreachable: every surface (`orbit-tools`, `orbit-cli`, `orbit-dashboard`) must
read the definition, so it has to live at or below `orbit-common`; handlers need
`&OrbitRuntime`, which lives in `orbit-core`, well above it. Co-locating them
would either drag the runtime into the leaf crate or lift the specs above the
surfaces that consume them — both new dependency edges that `ARCHITECTURE.md`
forbids.


### Decision
 Split the table across the two crates and join it with the noun's
typed verb enum: `&'static [OperationSpec<V>]` in `orbit-common`, an exhaustive
`match` on `V` in `orbit-core`. `V` is the only thing both halves share, and
because both the spec lookup and the handler dispatch are exhaustive matches, a
verb that is declared but not implemented fails to compile.


### Consequences


- Compile-time completeness across a crate boundary with no codegen, no trait
  object, and no runtime registration phase.
- Adding a verb breaks the build in exactly two known places, which is a usable
  to-do list rather than a silent gap.
- Future noun migrations should adopt this shape rather than re-attempting
  co-location; [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry)'s stored body records the correction.
- If [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry) bearing 2 (knowledge/execution split) moves knowledge handlers
  below the surfaces, the halves could merge and this ADR would be superseded.
- Cost: "the operation table" is now two files in two crates, so a reader
  looking for an operation's behavior must follow the verb enum to find the
  handler — the definition alone does not tell you what happens.

## Renderers and HTTP routes stay hand-written

**Recorded:** 2026-07-26 00:55:47.967899Z · [ORB-10358]
**Paths:** `crates/orbit-common/src/governance/operation.rs`, `crates/orbit-common/src/governance/friction/**`

### Context
 Once verbs are data, the obvious next step is to make the rest of
each surface data too: CLI output formatting and dashboard REST routes were both
candidates. Both would have grown the registry and both were rejected during the
friction pilot.


### Decision
 The spec declares *which* rendering a verb wants (`CliRender`) but
not how to render; the friction record and table printers stay in `orbit-cli` and
know friction field names. Dashboard route shapes, serde request bodies, and
HTTP-specific defaults stay hand-written in `orbit-dashboard`, which takes only
tool names and parameter names from the registry.


### Consequences


- The registry stays a description of *operations*, not of presentation, which
  keeps it readable and keeps its blast radius to contract.
- A REST path remains an interface design decision made per route, not a
  mechanical consequence of adding a verb.
- Adding a friction verb that should be reachable over HTTP is still a two-place
  change (registry + route).
- A noun whose output has a genuinely new shape needs a new `CliRender` variant
  plus a renderer — also two places.
- Cost: the dashboard is only partially derived, so it remains possible to add a
  verb and forget the route entirely; nothing fails, the verb is simply absent
  from the web UI, and no test catches it.

## Freeze the pre-migration surface as fixtures before migrating

**Recorded:** 2026-07-26 00:55:48.187121Z · [ORB-10358]
**Paths:** `crates/orbit-common/src/governance/operation.rs`, `crates/orbit-common/src/governance/friction/**`

### Context
 The pilot's hard requirement was that CLI argv/output and MCP tool
schemas stay wire-compatible. Derived clap help output is only byte-stable
because the adapter reproduces `#[derive(Args)]`'s conventions (arg id,
SCREAMING_SNAKE value name, declaration-order display) — a correspondence
nothing in the type system enforces. Verifying it after the fact, from the
migrated code, proves nothing.


### Decision
 Capture the pre-migration surface before writing any migration
code, and commit it as test fixtures. For friction: `orbit friction [<verb>]
--help` for all eight pages, captured from the binary built at the prior commit
and frozen under `crates/orbit-cli/src/command/tests/friction_help/`, asserted
via `include_str!`. The already-in-tree `mcp_tools_list.json` snapshot serves the
same role for MCP, where an empty `git diff` is the proof.


### Consequences


- "Wire compatible" became a checkable claim rather than a review assertion; the
  friction migration reproduces all eight help pages and the MCP snapshot
  byte-for-byte.
- The fixtures keep working after the migration as a regression guard on the
  derived surface, including across clap upgrades.
- Every future noun migration must do this first — the cookbook makes it Step 0.
- Cost: the fixtures encode incidental formatting (clap's global-arg placement,
  column alignment), so an intentional, approved CLI change now requires
  re-blessing files whose diff is mostly noise — and the fixture must be
  distinguished from a genuine regression by a human reading the PR.

## Capability chokepoint for destructive operations outside MCP

**Recorded:** 2026-07-26 21:49:30.348935Z · [ORB-10453]
**Paths:** `crates/orbit-common/src/governance/authorization.rs`, `crates/orbit-core/src/runtime/authorization.rs`, `crates/orbit-core/src/adapter/tool_execution.rs`, `crates/orbit-cli/src/main.rs`, `crates/orbit-cli/src/command/operation.rs`

### Context

Orbit had a capability model — `McpCapability::{Agent, Operator, Runner}` with `McpToolPolicy`, granted per server via `orbit mcp serve --capabilities` — and it governed exactly one surface. Three gaps followed.

The CLI dispatch path had no capability check at all. `register_inactive` hid destructive builtins from MCP listing, but hiding is advertisement, not enforcement: CLI subcommands reached those same tools through the admin `runtime.run_tool` bypass, and `orbit workspace teardown`, `orbit audit prune`, and `orbit learning prune` destroy things without touching a tool at all. And `task.approval.required_for_agent` / `task.approval.delegate_approval` were registered config keys threaded builder → context → runtime with no enforcement call site anywhere, with `OrbitError::TaskApprovalRequired` mapped in error formatting but never constructed.

The net effect was that no single place decided "may this caller perform this operation", so any per-command guard could be routed around by the next entry point.

This is an **accident guard, not a security boundary**. Agents on a development box run as the same OS user and can bypass Orbit entirely with git, the filesystem, or a direct write to the data root. Any design implying otherwise — in particular a credential or password — is explicitly rejected: it buys no protection and invites relaxing the surrounding rails on the strength of a boundary that does not exist. The goal is that unintended destruction fails loudly and leaves a record.

### Decision

1. **Extend the existing capability model; do not add a second one.** `McpCapability` is the vocabulary for every surface. MCP becomes one consumer of the model rather than its owner.

2. **Declare governed operations once, as data.** `orbit_common::governance::authorization::GOVERNED_OPERATIONS` is a const registry of `{ id, surface, allowed: &[McpCapability], rationale }`. Call sites name an operation, never a capability; the requirement is resolved from the registry. It lives in the leaf crate for the same reason the operations-as-data registry does ([North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry) bearing 1): every consumer surface must read it without a new dependency edge.

3. **One decision function, one chokepoint per surface.** `authorization::authorize` is the only place the rule is evaluated. It is reached from exactly two enforcement points, each of which its whole surface must traverse: `OrbitRuntime::run_tool_with_context_and_role` for every tool call (CLI `tool run`, the CLI admin bypass, MCP `tools/call`, the dashboard, the v2 deterministic dispatcher, agent loops), and the `Commands::operation` dispatch in `orbit-cli`'s `main` for CLI commands that destroy without a tool. Neither reimplements any part of the rule.

4. **The run's sanction travels with the run, not with the environment.** The distinction drawn is out-of-scope destruction versus destruction the run exists to perform. `run_deterministic` stamps `McpCapability::Runner` onto the tool context it dispatches with, so `release_locks` keeps working while the agent hosted inside the same run — which builds its own context and never reaches that code — does not inherit the grant. The pipeline's git path (worktree force-removal, `branch -D`, `git clean -fd`, `checkout -B`, `merge --ff-only`, PR merge) is deliberately absent from the registry: it is sanctioned destruction, and gating it would break every ship.

5. **Ambiguity fails closed, with one explicit escape hatch.** Caller capabilities resolve in strict precedence: session grants (a validated MCP session, or a run-stamped context) > `ORBIT_OPERATOR` override > agent envelope (`ORBIT_AGENT_NAME` / `ORBIT_AGENT_MODEL` / `ORBIT_TASK_ACTOR_KIND=agent` / `ORBIT_MANAGED_RUN_CONTEXT`) > interactive terminal > nothing. An unidentified caller holds no capability and every governed operation denies. An interactive TTY on both stdin and stderr is the one positive human signal available without a credential, which is what keeps `orbit workspace teardown` frictionless for a person while refusing it to a headless agent. `ORBIT_OPERATOR=1` is the documented escape hatch — trivial to set on purpose, because it is not defending against a determined caller, it is making a deliberate act look deliberate. Every use emits a `warn!` and a success-status `authorization` audit row.

6. **A denial is a distinct, recorded outcome.** `OrbitError::CapabilityDenied` is separate from `OrbitError::PolicyDenied` (`orbit-policy`'s path-scoping refusal): this one is about who is asking, not which path was touched. Both map to `AuditEventStatus::Denied` at every audit boundary, and the decision writes its own `command = 'authorization'` row so a denial is queryable with one predicate across every entry point, including surfaces that do not otherwise audit. The message names the required capability, why the operation is governed, what the caller was resolved as, and the escape hatch.

7. **Remove the approval config rather than wire it.** `task.approval.required_for_agent` / `delegate_approval` and `OrbitError::TaskApprovalRequired` are deleted. `required_for_agent` expressed "an agent may not unilaterally approve", which the capability model now states directly and enforces; `delegate_approval` had no consumer and no coherent meaning without a delegation surface. Keeping a second, unread knob for the same intent would reintroduce exactly the advertisement-versus-enforcement gap this ADR closes.

### Rejected alternatives

- **Per-subcommand guards.** Rejected: `orbit tool run` and every future entry point reopen the hole. This is the failure mode the task was filed against.
- **A credential, password, or keychain gate.** Rejected on the framing above — no protection, and it would misrepresent the boundary.
- **Making `ToolRegistry::execute` consult `ToolAvailability`.** Rejected: availability is an MCP advertisement concept, and enforcing it there would refuse operator callers the CLI legitimately serves, deepening rather than resolving the advertisement/enforcement conflation.
- **Converting the `register_inactive` builtins to `operator_only` policies.** Attractive — advertisement would then derive from the same declaration — but it changes what operator MCP sessions can see, and the capability chokepoint closes the actual bypass without that blast radius. Filed as ORB-10478, which also owns the residual gap noted in Consequences. Settled by [MCP advertisement is placement; the capability chokepoint is permission](#mcp-advertisement-is-placement-the-capability-chokepoint-is-permission): the two declarations stay separate, and a guardrail test pins their pairing.

### Consequences

- A single predicate (`command = 'authorization'`) answers "what was refused, to whom, and why" across CLI, MCP, dashboard, and engine. Denials are separable from failures in the audit trail for the first time.
- Adding a governed operation is one registry entry; the chokepoints need no edit. The capability required by an operation exists in exactly one enumerable place.
- Cost: a non-interactive caller with no session grant and no agent envelope — a shell script, a cron entry, a test binary — is now refused governed operations and must set `ORBIT_OPERATOR=1`. This is a real behavior change for automation that previously worked silently, and it landed as churn in three CLI integration tests. It is the intended trade: an unidentified caller performing destruction is precisely the accident being guarded against.
- Cost: the escape hatch is an environment variable an agent can set, so a *determined* agent is not stopped. That is by construction, not oversight — see the framing. The value delivered is the loud failure and the record, not prevention.
- Cost: the `governed_when` predicate for flag-gated commands (`--confirm`) lives in the `Commands::operation` arm, so a new destructive CLI command must say that it is one. The requirement itself still lives only in the registry, but the invocation-to-operation mapping is a second thing to remember. The compiler cannot catch a missing mapping.
- Residual gap, formerly tracked as **ORB-10478**, now closed: tools declared `McpExposure::OperatorOnly` (`orbit.friction.list` / `show` / `update`) were refused by the hub MCP *call* path (`admitted()`) but were absent from the registry, so `orbit tool run` still performed them for any caller. They were left ungoverned deliberately, because `OperatorOnly` encoded audience/placement rather than destructiveness and governing a read would have refused agents ordinary `orbit friction list`. See [MCP advertisement is placement; the capability chokepoint is permission](#mcp-advertisement-is-placement-the-capability-chokepoint-is-permission) for the resolution: `McpExposure` and `admitted()` no longer exist, the friction reads stay ungoverned on purpose, and the surviving pairing is pinned by a test.
- The registry is intentionally small and opt-in. It names the operations whose accidental invocation destroys something, not every mutating tool; broadening it is a judgment call to be made deliberately, per operation.

## MCP advertisement is placement; the capability chokepoint is permission

**Recorded:** 2026-08 · [ORB-10478] · **Implemented** in [ORB-10478]
**Paths:** `crates/orbit-common/src/governance/authorization.rs`, `crates/orbit-common/src/governance/operation.rs`, `crates/orbit-tools/src/builtin/orbit/tests/authorization.rs`

### Context

[Capability chokepoint for destructive operations outside MCP](#capability-chokepoint-for-destructive-operations-outside-mcp) closed the `orbit tool run` bypass but left one narrower question open, and ORB-10478 was filed to answer it: tools declared `McpExposure::OperatorOnly` were denied at the owner MCP *call* path by `admitted()`, yet were absent from `GOVERNED_OPERATIONS`, so the CLI performed them for any caller. Two surfaces encoded overlapping things, and one of them was acting as an authorization statement without being written as one.

Between the filing and this decision, most of that shape was deleted rather than reconciled. `McpExposure`, `allowed_capabilities()`, `admitted()`, and the `orbit-remote` crate that hosted them are all gone with the owner-route recomposition and the MCP v1 ownership cleanup. What is left is two axes and no overlap between their declarations:

- **Placement** — `OperationSpec::mcp_scope`, and `ToolRegistry::register_mcp` versus `register_inactive`. Decides which surfaces *list* a tool. `tools/list` performs no capability filtering, and `ToolRegistry::execute` never consults availability, so an unadvertised tool remains reachable through `orbit tool run`.
- **Permission** — `GOVERNED_OPERATIONS`, evaluated by `authorize` at `OrbitRuntime::authorize_tool_operation`, which every tool caller traverses.

The gap the earlier entry named has therefore closed by deletion, not by a fix: `orbit.friction.list` / `show` / `update` are ungoverned, no MCP path denies them, and an agent reads them from the CLI as before. What replaced it is the inverse pairing. `orbit.workflow.ship`, `orbit.workflow.run.{show,list,resume}` ([ORB-10534]) and `orbit.command.exec` ([ORB-10711]) are advertised to every MCP session and governed `operator` at the chokepoint, so an agent session is listed five tools it cannot perform.

### Decision

1. **Placement is not permission, and the two declarations stay separate.** MCP advertisement is an audience decision — what an agent reading `tools/list` is pointed at. It authorizes nothing. `GOVERNED_OPERATIONS` is the only authorization statement Orbit makes about a tool, and it is surface-independent by construction: the same answer for an MCP call, `orbit tool run`, the dashboard, and the deterministic dispatcher. This is stated once, in `orbit_common::authorization`'s module documentation, with a one-line pointer from the placement field so neither axis can be read as the whole story.

2. **A disagreeing pairing is legitimate, but never accidental.** Advertised-and-governed is how the operator MCP surface is expressed; unadvertised-and-ungoverned is how `orbit friction show` is kept off the agent surface without being taken away from anyone. Both are fine. Silent drift into either is not, so `crates/orbit-tools/src/builtin/orbit/tests/authorization.rs` declares the placement of every governed tool operation as a table and asserts it against the live registry in both directions. Advertising a governed tool, governing an advertised one, or dropping either half fails the test until the table is updated. `orbit-tools` is the lowest crate that can see both axes, so the check needs no new dependency edge.

3. **The agent-reachable friction reads stay ungoverned, and that is now a test.** Because the chokepoint is surface-independent, governing `orbit.friction.list` / `show` would refuse an agent the CLI read as well as the MCP one. A pin asserts they stay off the registry.

4. **An advertised governed tool must not list `agent` among its allowed capabilities.** Governing an operation the ordinary MCP caller already holds the capability for buys nothing and dilutes the entries that do refuse. A second assertion enforces it.

### Rejected alternatives

- **Give the exposure declaration a second axis — "which surface advertises" versus "which capability may perform" — and derive chokepoint enforcement from the second** (ORB-10478 option 1). Rejected: there is no longer a second advertisement axis to split. `McpExposure` is gone and `mcp_scope` is a plain placement decision, so this would mean *introducing* a policy framework to unify two declarations that already have one enforcement point between them — speculative v2 machinery for a drift that a ten-line table catches.
- **Narrow `OperatorOnly` to performable-only-by-operator, move the read-only friction triage tools to an agent exposure, and enforce `allowed_capabilities()` at the tool chokepoint** (option 2). Rejected: `OperatorOnly` no longer exists to narrow, and the chokepoint already enforces capabilities from a single declaration. The remaining content of the option — deriving enforcement from placement — would govern `orbit friction show` because it is unadvertised, which is precisely the CLI regression the earlier entry declined to ship.
- **Reconsidering the owner-path `admitted()` denial** (option 3's own follow-through). Not rejected but already satisfied: that denial was removed with the owner route, so MCP placement no longer acts as an authorization statement anywhere and there is nothing left to reconsider.
- **Leaving the pairing to a reader.** Rejected: it is exactly what the filing complained about — the relationship was true but only discoverable by holding two crates open, which is how it drifted in the first place.

### Consequences

- The question "may this caller perform this?" has one answer and one place to look, on every surface. "Will this caller be shown it?" is a different question with a different owner, and neither now implies the other by accident.
- An agent MCP session still sees five tools it cannot call. This is the accepted cost of a single advertised tool list plus per-call capability enforcement, and it is recorded rather than papered over. Filtering `tools/list` by session capability would remove it, but that is an MCP-server change with its own surface implications and no current demand; it is deliberately not bundled here.
- Cost: adding a governed tool now requires one more line — its placement — in the guardrail table. That is the intended friction: the line is where the advertise-or-not decision gets made explicitly.
- An MCP session's capabilities are decided once, when its server process starts, and the process environment is never consulted per call: `orbit mcp serve` grants `agent`, `orbit mcp serve --operator` grants `agent` and `operator`, and `ORBIT_OPERATOR` in the server's environment grants nothing on this surface ([ORB-10927]). The flag is the only operator path over MCP — the deliberate act has to be in the argv the operator wrote, because an environment variable is inherited by whatever server an agent happens to launch. `orbit mcp listen` is agent-only for the stronger version of the same reason: the socket authenticates no client, so a listener started with operator authority would serve it to every accepted connection.
- Because the MCP chokepoint discards the process envelope by design, its denial names the remedy that surface actually has (`orbit mcp serve --operator`, or the CLI) rather than the `ORBIT_OPERATOR` override that is inert there. That is `CapabilityResolution` in `orbit_common::governance::authorization`: one enum on the envelope, carried into the denial, changing the remedy sentence and nothing about the decision.

## Task References

- [ORB-10358] — piloted [North-star architecture bearing: operations as data behind an operation registry](../orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry) bearing 1 on the friction noun; produced the
  split table, the derived adapters, and the frozen-surface method.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
