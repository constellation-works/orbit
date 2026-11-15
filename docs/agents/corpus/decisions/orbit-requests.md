---
type: decision
applies_to: [agent_implement, final_recovery, task_pilot]
status: active
links: []
---

# Orbit decision request precedent

## Scope

Human answers to Orbit task and run decision requests only. Each entry preserves
the question, options, chosen option, human note, task, run and binding. General
product direction belongs in the orbit-office Library. This seed contains no
invented human answers. These entries are raw history, not rules. Only a
current rule under Rules, after a human merges it, may be cited. A rule
answers a new request only when the operator has set auto on that current
rule. Nothing here grants lifecycle, merge or bypass authority.

## Entries

Human-answered requests append here through the durable task-history outbox and
the reviewed batch materializer.
