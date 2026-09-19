---
title: Distributed Drain — Vision
owner: claude
last_updated: 2026-09-18
last_validated: 2026-09-18
status: Draft
feature: distributed-drain
doc_role: vision
type: design
summary: What the pull-based drain deliberately does not do yet — crew auto-assignment, cloud stores, follower-side merge, federated run inspection — and the questions each one opens.
tags: [distributed-drain, multi-host, vision]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, resident-orchestrator, host-registry]
related_artifacts: [ORB-12488]
---

# Distributed Drain — Vision

V1 distributes execution while retaining one coordination authority. Drains are explicitly
invoked; the unused ship sweep is retired, while authorized landing follows durable handoff requests. Its required scope includes
idempotent admission, attempt ownership, manual recovery, routed task reads/writes, and durable
landing handoff. The questions below extend that contract; they are not prerequisites hidden as
future work.

## 1. Open Questions

1. **Liveness signals and automatic recovery.** V1 exposes claims for inspection and requires
   deliberate revocation. It does not infer death from reservation expiry or absent owner-local
   runs. A later heartbeat could improve diagnosis, but automatic reassignment would still need
   the current attempt fencing and external-side-effect reconciliation contracts.
2. **Crew assignment and execution eligibility.** V1 requires every participating host to satisfy
   the full workspace execution requirements. Future owner-evaluated eligibility could select the
   first task a host can execute while preserving canonical priority among eligible tasks. Crew
   auto-assignment and OS/toolchain eligibility are separate concerns; neither requires delegating
   priority order to followers.
3. **Follower-side merge.** V1 stops delivery at `pr_open` + `review`. Letting the follower run
   `pr_complete` needs the merge authority and `gh` credentials on every host, and reintroduces the
   PR-handoff recovery races that are currently owner-local.
4. **Cloud-offloaded owner store.** Federated-mcp's open question 4. If the coordination store
   leaves the owner host, pull becomes an HTTP call and the "always-on owner" decision dissolves.
   The pull contract is written to survive that: it names a store, not a machine.
5. **Federated run inspection.** `orbit run` on the owner cannot see follower runs. With
   `job_run_host` on the task, a read-only `execute`-class run lookup routed to that destination is
   the obvious shape; whether the dashboard should
   aggregate it is a separate question.
6. **Hosted sessions as followers.** A cloud session could pull if the owner store were reachable
   from it, which today it is not. Revisit once question 4 has an answer.

## 2. Prior Work

### Orbit's own drain

`workspace_auto_pipeline` already refills slots from the whole backlog as each child finishes and
treats a live wrapper run as the claim. The distributed drain keeps that throughput model, moves
the claim into authoritative store transactions and drops the epic branch of the loop. Archiving
the resident-orchestrator folder is part of the proposed retirement, not an already completed step.
See [resident-orchestrator 2_design.md §4](../resident-orchestrator/2_design.md#4-workspace-drain-workspace_auto_pipeline).

### Federated MCP and the callers file

The `control_plane` / `execute` split, host-qualified selectors, fail-closed routing, and
destination-side caller authorization are all specified and partly live. Pull uses that surface
and adds no transport. Core runtime reads, claim-scoped mutations, and
subprocess context still need explicit routing; MCP federation alone does not wire them together.

### Work-stealing schedulers

Pull-based distribution where idle workers take from a shared queue is the standard shape for
workers: capacity can remain local, and a worker that disappears stops taking new work. Its
existing claims still require settlement or deliberate recovery; pulling does not solve that
failure mode by itself. Push-based round-robin was considered and rejected
for exactly that reason ([4_decisions.md](./4_decisions.md#followers-pull-the-owner-never-places)).

### Task migration

Hosts stay disjoint by `task_prefix` and the minting host owns each task. The distributed drain
does not change that: followers never mint, so a pulled task keeps the owner's prefix.

## 3. What May Be Distinctive

The useful property is the split between centralized admission/landing and host-local execution.
There is no fleet registry, heartbeat service, or message bus. Durable request receipts and claim
identities are still required: ordinary task status and reservation TTL cannot distinguish a lost
response from a new pull, or an obsolete attempt from its replacement.

The first implementation should measure completed throughput and time spent waiting on locks,
CI, and landing before adding placement policy or automatic recovery.

## 4. References

**Orbit-internal**

- [federated-mcp specs/federated-workspace-mcp.md](../federated-mcp/specs/federated-workspace-mcp.md)
- [federated-mcp specs/caller-authorization.md](../federated-mcp/specs/caller-authorization.md)
- [host-registry 3_vision.md](../host-registry/3_vision.md) — "checkoutless operations" gate
- [resident-orchestrator 2_design.md](../resident-orchestrator/2_design.md)
- [runbooks/build-budget.md](../../runbooks/build-budget.md)

**External**

- Claude Code cloud sessions and environments: https://code.claude.com/docs/en/claude-code-on-the-web — evaluated 2026-09-18 and deferred (no completion signal, no private-network reach).

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
