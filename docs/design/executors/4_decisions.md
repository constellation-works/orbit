---
title: Executors — Decisions
owner: claude
last_updated: 2026-08-11
last_validated: 2026-08-29
status: Draft
feature: executors
doc_role: decisions
type: design
summary: Decision log for executor registration, the local-shell executor, and the (now retired) External Executor Protocol.
tags: [executors]
paths: ["crates/orbit-types/src/workflow/executor_def.rs"]
related_features: [executors]
related_artifacts: [ORB-00384, ORB-00400, ORB-10395, ORB-11294]
---

# Executors — Decisions

Layout note: as of [ORB-00400], this folder is intentionally decisions+specs-only.
[External Executor Protocol for dynamic out-of-process executor registration (retired)](#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired) and [specs/external-executor-protocol.md](./specs/external-executor-protocol.md)
are the load-bearing docs for the shipped External Executor Protocol; placeholder
`1_overview.md`, `2_design.md`, and `3_vision.md` docs would imply a broader
executor feature narrative that this work has not established. Add numbered docs
only when a future executor-architecture task owns that narrative, and retire
this exception in the same PR.

---

## Restore local-shell as a deterministic v2 action rather than a v1 executor

**Recorded:** 2026-09-05 · [ORB-11294]
**Paths:** `crates/orbit-engine/src/executor/automation/shell.rs`, `crates/orbit-core/assets/executors/local-shell.yaml`, [specs/local-shell.md](./specs/local-shell.md)

**Context.** [ORB-10395] deleted the v1 executor stack, including the
`cli_command` implementation, but `assets/executors/local-shell.yaml` kept
shipping with `executor_type: cli_command`. The asset advertised a capability
no code could dispatch: a job that referenced it failed, and the definition was
inert.

**Decision.** Restore local shell execution as a **deterministic action**
(`local_shell`) on the v2 dispatch path, not by resurrecting the v1 registry.
The action reuses the `orbit-exec` spawn, sandbox-selection, and supervision
machinery the CLI agent runner already uses; it does not carry a second copy of
process-group setup or timeout handling. `ExecutorType::CliCommand` is renamed
to `ExecutorType::LocalShell` with `cli_command` retained as a serde alias, so
the executor definition remains the place operators declare sandbox, default
program, environment additions, and timeout for shell steps.

Two boundaries are kept deliberately separate. Agent provider resolution
(`resolve_cli_executor`) rejects a shell definition, and shell executor
resolution (`resolve_local_shell_executor`) rejects an agent definition. A shell
step therefore cannot acquire agent authority — no prompt, no model, no tool
allowlist, no Orbit registry/workspace identity variables.

The program and its arguments are read from the activity's static `config`
block and never from step input. Job step `with:` blocks are template-rendered
and can carry agent-produced text; sourcing argv from them would make every
shell step an injection surface.

**Alternatives rejected.**

- *Reinstate the v1 `ActivityExecutor` registry for this one type.* Restores a
  parallel dispatch path that v2 deliberately collapsed, for a feature that maps
  cleanly onto an existing deterministic action.
- *Implement it as an `agent_loop` provider.* Would put deterministic shell
  execution behind crew/model/provider resolution and grant it the agent
  envelope, which is exactly the authority a shell step must not have.
- *Leave the asset as a YAML-only claim.* Rejected outright: the task exists
  because a shipped asset described behavior no runtime implemented.

**Consequences.**

- `local_shell` joins the engine deterministic action catalog, so job validation
  can prove a catalog asset naming it is dispatchable before a run admits work.
- Engine deterministic actions now receive the activity's `config` block and its
  `fsProfile` alongside step input. `config` was previously reachable only by
  core actions, and the profile only by the CLI runner.
- The shipped `local-shell` definition declares no sandbox, so shell steps run
  bare under the process supervisor and are confined by `cwd` containment plus
  whatever sandbox already contains the Orbit process. Each step reports the
  backend it actually ran under. Operators who want kernel confinement add a
  `sandbox:` to the definition and an `fsProfile` to the activity.
- `orbit-exec` gains `supervise_child`, the public seam for supervising a child
  built through a sandbox wrapper. Without it the sandboxed path would have had
  to reimplement draining, deadlines, and process-group cleanup.

---

## External Executor Protocol for dynamic out-of-process executor registration (retired)

**Recorded:** 2026-06-14 00:40:41.791069Z · [ORB-00384], [ORB-10395]
**Paths:** `crates/orbit-types/src/workflow/executor_def.rs`, `docs/design/executors/**`

> **RETIRED 2026-07-26 — [ORB-10395].** The External Executor Protocol is not a supported Orbit surface. Retiring the v1 executor stack removed everything this decision stood up: `ExternalExecutor`, the shared `direct_agent` subprocess transport, `ActivityExecutorRegistry`, the `ActivityExecutor` trait, the v1 `ExecutionContext`, the `external.example.yaml` template, and the conformance fixture. v2 dispatch (`orbit-engine::activity_job`) is the only execution path and consults no executor registry, so an `executor_type: external` def is now inert — it deserializes and stores, but nothing spawns it. `ExecutorType::External` survives in the wire enum only so pre-existing defs keep parsing; dropping the variant is a separate release decision, tracked alongside the same call for `ExecutorType::AgentCli`. The Consequences below are history, not live obligations — in particular the wire protocol is **no longer** a backward-compatibility obligation. Any future out-of-process extension point must be decided afresh against the v2 dispatch path.

**Context.** Orbit's `ExecutorType` is a sealed enum and `load_from_defs` is a closed `match`, so a homegrown executor can only be added by forking orbit-engine — an `internal`-tier crate with no downstream guarantees. Yet `DirectAgentExecutor` already implements an out-of-process transport (spawn `command`, write a prompt envelope to stdin, map a stdout result envelope to an outcome): the capability exists but is undocumented and coupled to the agent-family `direct_agent` path.

**Decision.** Promote that transport into a documented, versioned **External Executor Protocol v1** and expose it through a new `ExecutorType::External` (wire value `external`). A homegrown executor is registered by dropping a YAML executor def that points at a binary/script speaking the protocol — no recompile, no linking, language-agnostic. In-process Rust extension (an `ExecutorFactory` registry plus a runtime injection seam) is explicitly deferred to a separate Tier 2 decision.

**Consequences.**
- Most homegrown executors become config-only: a YAML def plus a conforming binary, with zero changes to Orbit.
- The stdin/stdout envelope becomes a stability commitment — once v1 ships, the request/result shape is a contract that must be versioned, not changed in place.
- `external` reuses the existing `FsProfile`→sandbox path, so dynamic registration does not widen the sandbox-bypass surface relative to `direct_agent`.
- Executors needing a non-subprocess transport (in-process SDK, gRPC, internal queue) are NOT served by Tier 1 and must wait for Tier 2.
- Cost: a documented wire protocol is a long-lived backward-compatibility obligation — every future executor capability must be expressible as an additive, versioned envelope field, and a conformance harness must be maintained so adopters do not silently depend on undocumented behavior.

## Retire the External Executor Protocol v1

**Recorded:** 2026-07-25 22:59:08.214884Z · [ORB-10395]

### Context

The v1 runtime-host phase-out (knowledgebase/polaris/design/orbit-cleanup/phaseoutv1.md, Stage 3) deletes the v1 executor stack once planning duel is ported to v2 (ORB-10393). `ExternalExecutor` implements the External Executor Protocol v1 — a documented public extension point (docs/design/executors/specs/external-executor-protocol.md, [External Executor Protocol for dynamic out-of-process executor registration (retired)](#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired), assets/executors/external.example.yaml) — and shares the `direct_agent` subprocess transport slated for deletion. The phase-out design flagged an open question: if external executors remain a supported surface, the transport must be rehomed rather than deleted.

### Decision

Daniel decided on 2026-07-25: the External Executor Protocol is not a supported surface and is retired. `ExternalExecutor` and the shared `direct_agent` transport are deleted with the rest of the v1 executor stack in Stage 3 (ORB-10395); nothing is rehomed. The protocol spec doc and example asset are marked retired/removed in the same change, and [External Executor Protocol for dynamic out-of-process executor registration (retired)](#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired) is superseded by this record.

### Consequences

- Stage 3 becomes a pure deletion with no transport-rehoming work; the v1 executor stack (~1,000+ LOC) drops out in one gated task.
- Any out-of-tree executor built against the protocol stops working; there are no known consumers, and the removal is noted in release notes.
- Cost: re-introducing an external-executor extension point later means designing a new protocol against the v2 pipeline from scratch rather than reviving this one.

## Task References

- **[ORB-00384]** — External Executor Protocol v1: define the contract, add `ExecutorType::External`, register a generic external-process executor, document the spec, ship a conformance test.
- **[ORB-00400]** — recorded the `executors` folder as a decisions+specs-only layout exception while refreshing design-doc ownership conventions.
- **[ORB-10395]** — retired the External Executor Protocol with the rest of the v1 executor stack: deleted `ExternalExecutor`, the shared subprocess transport, the executor registry, the example def template, and the conformance fixture.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
