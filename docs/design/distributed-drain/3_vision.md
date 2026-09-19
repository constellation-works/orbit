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

V1 is deliberately the smallest thing that doubles throughput: one tool, one drain flag, one
pipeline input, one operator runbook. Everything below is what v1 leaves on the table, labelled as
speculation where it is speculation.

## 1. Open Questions

1. **Stale `in-progress` detection.** V1 relies on the reservation TTL and the unresolved-work
   scan to notice a task whose follower died. Is that fast enough in practice, or does the owner
   need a cheap liveness signal — for example the follower re-asserting its carried task ids on
   every pull call, so the owner can flag any `in-progress` task no host has asserted for N
   iterations? That is a heartbeat by another name; the question is whether the cost is worth it.
2. **Crew auto-assignment and crew-aware pulling.** V1 pulls strictly in queue order with no crew
   input. The planned next step is assigning a crew to each task by complexity when it enters the
   queue, so crew becomes a property of the queued task. Only then does a follower that can run
   some crews and not others have a well-defined pull: take the first entry whose assigned crew it
   can serve. Doing the filter before the assignment would let callers reorder the queue.
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
the claim into the store, moves the ordering into one owner queue, and drops the epic branch of
the loop. The resident-orchestrator folder that specified the epic path is archived with a pointer
here. See [resident-orchestrator 2_design.md §4](../resident-orchestrator/2_design.md#4-workspace-drain-workspace_auto_pipeline).

### Federated MCP and the callers file

The `control_plane` / `execute` split, host-qualified selectors, fail-closed routing, and
destination-side caller authorization are all specified and partly live. Pull is one more
`control_plane` tool routed through that surface; it adds no transport.

### Work-stealing schedulers

Pull-based distribution where idle workers take from a shared queue is the standard shape for
heterogeneous, unreliable workers: the queue never needs to know worker capacity or liveness, and
a worker that disappears simply stops taking. Push-based round-robin was considered and rejected
for exactly that reason ([4_decisions.md](./4_decisions.md#followers-pull-the-owner-never-places)).

### Task migration

Hosts stay disjoint by `task_prefix` and the minting host owns each task. The distributed drain
does not change that: followers never mint, so a pulled task keeps the owner's prefix.

## 3. What May Be Distinctive

Not much, and that is the point. The claim is the ordinary task status plus the ordinary lock
reservation, taken atomically by the same code that serves a local drain. There is no scheduler
process, no worker table, and no message bus; the coordination store already had every primitive.
What is somewhat unusual is that a host can join or leave the drain by editing one TOML row on the
owner and starting or stopping one local command, with no owner-side restart.

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
