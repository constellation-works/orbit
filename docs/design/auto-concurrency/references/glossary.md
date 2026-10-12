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
| **Admit thresholds** | The host CPU and memory levels, below the throttle's marks, that forecast load plus a candidate's profile must stay under. [2_design.md §5.1](../2_design.md#51-gap-fit) |
| **Backstop** | The existing resource throttle, which holds all admission under real pressure whatever the forecast says. [2_design.md §5.1](../2_design.md#51-gap-fit) |
| **Baseline** | Measured host load minus the measured load of running tasks; load that isn't from tasks, held flat over the forecast. [2_design.md §4](../2_design.md#4-the-load-forecast) |
| **Delivery** | All runs of one task admitted together (a local chain or one claimed run), sampled as one series from the admission minute. [2_design.md §2](../2_design.md#2-sampling-each-task) |
| **Gap fit** | Admitting a candidate because forecast load plus its profile stays under the admit thresholds for its whole horizon. [2_design.md §5.1](../2_design.md#51-gap-fit) |
| **Horizon** | A candidate's 90th-percentile duration from its profile, capped at 120 minutes; the span gap fit checks. [2_design.md §3](../2_design.md#3-profiles) |
| **Load forecast** | Expected host CPU and memory for each coming minute: running deliveries' profiles at their ages, reservations and the baseline. [2_design.md §4](../2_design.md#4-the-load-forecast) |
| **Reservation** | An admission decided but not yet running, counted in the forecast so two drains cannot fill one gap. [2_design.md §5.2](../2_design.md#52-serializing-drains-on-one-host) |
| **Resource profile** | Per minute since admission: the chance a delivery is still running, mean CPU and 90th-percentile memory, for one host, workspace and complexity. [2_design.md §3](../2_design.md#3-profiles) |
