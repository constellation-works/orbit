---
title: Distributed Drain — Vision
owner: claude
last_updated: 2026-10-31
last_validated: 2026-10-28
status: Draft
feature: distributed-drain
doc_role: vision
type: design
summary: What the pull-based drain deliberately does not do yet — crew auto-assignment, cloud stores, follower-side merge, federated run inspection — and the questions each one opens.
tags: [distributed-drain, multi-host, vision]
paths: ["crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml", "crates/orbit-mcp/**"]
related_features: [distributed-drain, federated-mcp, host-registry]
related_artifacts: [ORB-12488]
---

# Distributed Drain — Vision

The contract [ORB-12488] authored distributes execution while retaining one coordination authority. Explicit drains and ship
entry points share claim admission, while authorized landing follows durable handoff
requests. Its required scope includes idempotent admission, attempt ownership, manual recovery,
routed task reads/writes, and durable landing handoff. The questions below extend that contract;
they are not prerequisites hidden as future work.

Claimed PR leaves can run the owner's captured before-PR (`review.timing = "before_pr"`) gate
when the executor declares `review_gate`; the executor's own switch is diagnostic.

## 1. Open Questions

1. **Liveness signals and automatic recovery.** V1 exposes claims for inspection and requires
   deliberate revocation. It does not infer death from reservation expiry or absent owner-local
   runs. A later heartbeat could improve diagnosis, but automatic reassignment would still need
   the current attempt fencing and external-side-effect reconciliation contracts.
   The owner side of this question, failover when the owner itself is down, is proposed in
   [owner-failover](../owner-failover/1_overview.md). It keeps manual recovery: takeover is
   operator-triggered, and claims never cross an ownership epoch.
2. **Crew assignment and execution eligibility.** Per-executor eligibility is shipped:
   [ORB-13941] added crew filtering, [ORB-14005] added OS filtering, and [ORB-18316]
   added machine capability labels. [ORB-17304] made owner ordering scarcity-aware
   through the live executor census. Each host must still satisfy the sandbox and
   toolchain requirements of the task it receives. Automatic reassignment of a task's
   crew remains a separate question; the owner keeps admission and priority authority.
3. **Follower-side merge.** V1 stops delivery at `pr_open` + `review`. Letting the follower run
   `pr_complete` needs the merge authority and `gh` credentials on every host, and reintroduces the
   PR-handoff recovery races that are currently owner-local.
4. **Cloud-offloaded owner store.** Federated-mcp's open question 4. If the coordination store
   leaves the owner host, pull becomes an HTTP call and the "always-on owner" decision dissolves.
   The pull contract is written to survive that: it names a store, not a machine.
   Moving ownership between hosts during an outage, with the old owner rejoining, is proposed
   separately in [owner-failover](../owner-failover/2_design.md).
5. **Federated run inspection.** `orbit run` on the owner cannot see follower runs. With
   `job_run_machine` on the task, a read-only `execute`-class run lookup routed to that destination is
   the obvious shape; whether the dashboard should
   aggregate it is a separate question.
6. **Bounded request receipt retention.** V1 compacts settled receipts to permanent tombstones and
   stops each refill pass at its first idle response. A future retired-namespace/sequence protocol
   could bound storage while rejecting every old request; random IDs and age-based deletion cannot.
7. **Checkpoint resume under claims.** V1 refuses generic resume of interrupted claimed leaves;
   recovery creates a fenced new claim/run. A future logical execution identity or fenced run
   transfer can preserve checkpoints without permitting two run identities under one immutable binding.
8. **Hosted sessions as followers.** A cloud session could pull if the owner store were reachable
   from it, which today it is not. Revisit once question 4 has an answer.

## 2. Prior Work

### Orbit's own drain

Before distributed admission, `workspace_auto_pipeline` refilled slots using host-local
wrapper runs as claim evidence. [ORB-12528] moved admission into authoritative store
transactions, and [ORB-12617] unified occupancy across legacy wrappers and claimed leaves.
The distributed drain keeps slot refill and drops the epic branch of the loop.

### Federated MCP and remote authority

The `control_plane` / `execute` split, host-qualified selectors, and fail-closed routing are
specified and live. Destination-side caller authorization is **not**: it shipped and was removed in
[ORB-12564], because an SSH login to a destination is ownership of it. A destination now serves the
authority the session's argv asks for. This design originally required a `KeyBound` caller identity
in its §5; that requirement was withdrawn rather than rebuilt (see
[4_decisions.md](./4_decisions.md#ssh-login-is-the-admission-machine-labels-are-attribution)), and
session capability plus trusted claim fencing carry attempt ownership instead. Pull uses the
federated surface and adds no transport. Core runtime reads, claim-scoped mutations, and
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

Further placement policy or automatic recovery should be justified by measured completed
throughput and time spent waiting on locks, CI, and landing in the shipped implementation.

## 4. References

**Orbit-internal**

- [federated-mcp specs/federated-workspace-mcp.md](../federated-mcp/specs/federated-workspace-mcp.md)
- [federated-mcp 4_decisions.md — An SSH login to a destination is ownership of it](../federated-mcp/4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it) (supersedes the removed destination-side caller-authorization spec)
- [host-registry 3_vision.md](../host-registry/3_vision.md) — "checkoutless operations" gate
- [runbooks/build-budget.md](../../runbooks/build-budget.md)
- [owner-failover 1_overview.md](../owner-failover/1_overview.md) — proposed epoch-fenced owner handoff and takeover

**External**

- Claude Code cloud sessions and environments: https://code.claude.com/docs/en/claude-code-on-the-web — evaluated 2026-09-18 and deferred (no completion signal, no private-network reach).

## Task References

- [ORB-12488] — authored this design folder for the pull-based multi-host drain.
- [ORB-12528] — landed the transactional task/reservation admission boundary.
- [ORB-12564] — removed destination caller authorization and made SSH sessions honor argv authority.
- [ORB-12617] — unified legacy/claimed occupancy and completed the end-to-end fixture slice.
- [ORB-13941] — filtered pull admission by runnable executor crews and provider availability.
- [ORB-14005] — routed tasks by validated executor OS requirements.
- [ORB-17304] — implemented scarcity-aware admission against the live executor census.
- [ORB-18316] — added task placement by required machine capability labels.
- [ORB-20620] — proposed owner failover, linked from open questions 1 and 4.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
