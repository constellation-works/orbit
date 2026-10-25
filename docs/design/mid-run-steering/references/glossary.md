---
type: design
summary: "Glossary: Mid-Run Operator Steering"
last_validated: 2026-10-25
---

# Glossary: Mid-Run Operator Steering

Vocabulary specific to this feature. Pipeline, recovery and task-lifecycle terms are defined in [activity-job](../../activity-job/) and are deliberately not repeated here.

| Term | Meaning |
|------|---------|
| **Delivery** | The host building a prompt that contains a comment past the run's watermark, recording `steering_delivered` and advancing the watermark. A fact about the prompt, not about the agent. See [2_design.md §2](../2_design.md). |
| **Promotion gate** | The deterministic check before `commit` that refuses to proceed while steering is undelivered. See [2_design.md §3](../2_design.md). |
| **Safe point** | A step boundary where the host builds a fresh prompt — recovery, repair, final recovery, resume or re-dispatch. The only delivery points in version one. See [2_design.md §2](../2_design.md). |
| **Snapshot watermark** | The per-run high-water mark naming the newest task comment included in a host-built prompt for that run. See [2_design.md §1](../2_design.md). |
| **Steering block** | The bounded, oldest-first bundle of undelivered operator comments placed ahead of the frozen task context in a safe-point prompt. See [2_design.md §2](../2_design.md). |
| **Steering comment** | An operator-authored task comment written after the prompt snapshot a live run started from. See [1_overview.md §2](../1_overview.md). |
| **Steering round** | One budgeted re-dispatch of an agent step carrying a steering block, spent at the promotion gate. See [specs/steering-delivery.md](../specs/steering-delivery.md). |
