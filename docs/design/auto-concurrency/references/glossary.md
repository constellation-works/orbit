---
type: design
summary: "Glossary: Auto-concurrency"
last_validated: 2026-10-11
---

# Glossary: Auto-concurrency

Terms the auto-concurrency proposal gives a specific meaning. Drain, leaf,
claim and pull are defined by the distributed-drain glossary and are not
repeated here.

| Term | Meaning |
|------|---------|
| **Ceiling binding** | The estimator's verdict when goodput is still rising at the highest well-sampled level, so the operator's maximum, not the host, is the constraint. [2_design.md §6](../2_design.md#6-offline-estimator) |
| **Congestion signal** | Fast evidence of host strain that may only lower the host budget: a resource-throttle hold, a provider-limit exclusion, an environmental failure, or validation-step latency inflation. [2_design.md §4](../2_design.md#4-signals) |
| **Goodput** | Baseline work-hours of leaves that landed and stayed landed, per wall-clock hour. [2_design.md §1](../2_design.md#1-the-objective-is-goodput) |
| **Host budget** | The number of live leaves a host may run across all its drains; the one quantity the controller tunes. [2_design.md §3.2](../2_design.md#32-proposed-a-host-budget-and-workspace-caps) |
| **Latency gradient** | Long-window baseline over short-window median of one heavy step's duration; a fall below tolerance is congestion. [2_design.md §4](../2_design.md#4-signals) |
| **Occupancy** | Live leaf runs on a host across every workspace, observed rather than configured. [2_design.md §6](../2_design.md#6-offline-estimator) |
| **Pin** | A manual resize that removes a drain from automatic control for the rest of its window. [2_design.md §5.4](../2_design.md#54-actuation-and-pins) |
| **Saturated interval** | An interval in which every slot was busy and admissible work was still waiting; the only kind that informs capacity. [2_design.md §4](../2_design.md#4-signals) |
| **Shadow mode** | The controller computes and records decisions without applying them. [2_design.md §8](../2_design.md#8-rollout) |
| **Slowdown** | A leaf's duration over the baseline for its complexity class in its workspace. [2_design.md §1](../2_design.md#1-the-objective-is-goodput) |
| **Soft ceiling** | The highest level the goodput model supports, below the operator's maximum, re-tested daily. [2_design.md §5.3](../2_design.md#53-the-slow-loop) |
| **Workspace cap** | A bound on one workspace's live leaves across hosts, set by contention on its base branch. [2_design.md §3.2](../2_design.md#32-proposed-a-host-budget-and-workspace-caps) |
