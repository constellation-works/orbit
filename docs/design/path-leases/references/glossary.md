---
type: design
summary: "Glossary: Path Leases"
last_validated: 2026-10-25
---

# Glossary: Path Leases

Terms that path leases introduce or give a specific meaning. Lock, reservation, footprint and
landing terms keep their meaning from the distributed-drain and conflict-budget designs and are
not repeated here.

| Term | Meaning |
|------|---------|
| **Advisory lease** | A lease that is recorded and announced but never blocks a task or another lease. The default mode. [2_design.md §3](../2_design.md#3-granting-queueing-and-the-drain) |
| **Bound branch** | The branch or pull request whose landing on the lease's base releases the lease. [2_design.md §4](../2_design.md#4-lifetime) |
| **Hard lease** | A lease that holds its paths exclusively, as a task context lock does, after path policy. [2_design.md §3](../2_design.md#3-granting-queueing-and-the-drain) |
| **Landing notice** | The `overlap_landed` notice: a landed commit on the lease's base changed paths the lease holds. [2_design.md §5](../2_design.md#5-notices) |
| **Lease listener** | The opt-in, lease-only HTTP listener that accepts lease tokens. [2_design.md §7](../2_design.md#7-authority-and-authentication) |
| **Lease queue** | Hard requests waiting on an overlapping hard holder, ordered by priority then age, with aging. [2_design.md §3](../2_design.md#3-granting-queueing-and-the-drain) |
| **Lease token** | A bearer credential for one principal, scoped to workspaces, verbs, a priority ceiling and an expiry, valid only on the lease listener. [2_design.md §7](../2_design.md#7-authority-and-authentication) |
| **Notice** | A durable, sequenced event on a lease, read by cursor poll or SSE. [2_design.md §5](../2_design.md#5-notices) |
| **Path lease** | A reservation of path selectors held by a principal rather than a task. [2_design.md §2](../2_design.md#2-the-lease-record) |
| **Principal** | The holder of a lease: a person or a named agent, with a principal class (`human` or `agent`). Attribution over SSH; the authenticated subject on the lease listener. [2_design.md §7](../2_design.md#7-authority-and-authentication) |
| **Priority ceiling** | The highest priority a principal may request; a guard over SSH, a boundary on the lease listener. [2_design.md §7](../2_design.md#7-authority-and-authentication) |
