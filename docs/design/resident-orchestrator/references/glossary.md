---
type: design
summary: "Glossary of resident-orchestrator terms: manager, mandate, authority, resident, fire, cycle, checkpoint, wake digest, cost event, evolution record"
last_validated: 2026-11-05
title: Resident Orchestrator — Glossary
owner: claude
status: Draft
feature: resident-orchestrator
tags: [resident-orchestrator, glossary]
related_features: [resident-orchestrator]
related_artifacts: [ORB-21229]
last_updated: 2026-11-02
---

# Resident Orchestrator — Glossary

Orbit-specific terms used by the resident-orchestrator design. Terms owned by other features
(notice, decision request, attention item, routine) keep their own definitions and are linked from
where they are used.

| Term | Meaning |
|---|---|
| Action key | `resident:<mandate>:<cycle>:<n>`, attached to every mutating action so a recovery cycle never repeats one ([2_design.md §5](../2_design.md#5-the-cycle)) |
| Authority | The rights (`dispatch`, `promote`, `merge`, `plugin_upgrade`) and covered workspaces a mandate grants; widened only by the human ([mandate-and-authority](../specs/mandate-and-authority.md)) |
| Cadence floor | Minimum interval between cycle starts for one mandate ([2_design.md §4.3](../2_design.md#43-cadence-floor)) |
| Checkpoint | The `resident.checkpoint` comment a cycle leaves; the resident's memory ([checkpoint-contract](../specs/checkpoint-contract.md)) |
| Cost event | One `costs[]` entry: a workaround, manual step, retry or wait, with its cost and wished-for capability ([evolution-ledger](../specs/evolution-ledger.md#cost-event)) |
| Cycle | One run of `resident_cycle_pipeline`: observe, one agent loop, finalize ([2_design.md §5](../2_design.md#5-the-cycle)) |
| Escalation | A question the resident must not decide, raised as a decision request ([2_design.md §8](../2_design.md#8-escalation)) |
| Evolution pass | The scheduled routine that turned evolution records above threshold into `proposed` tasks; removed in [ORB-22844] ([evolution-ledger](../specs/evolution-ledger.md#evolution-pass-removed)) |
| Evolution record | Per-signature record of a recurring capability gap, separate from friction ([evolution-ledger](../specs/evolution-ledger.md#evolution-record)) |
| Fire | The clock tick at which a mandate is due and a cycle starts ([2_design.md §4.2](../2_design.md#42-fires)) |
| Grant request | A `resident_authority` decision request raised when a non-operator asks to widen authority ([mandate-and-authority](../specs/mandate-and-authority.md#widening)) |
| Heartbeat | Maximum interval without a cycle, even when nothing changed ([2_design.md §4.2](../2_design.md#42-fires)) |
| Manager | The human's single conversational front door; holds no loop and no state ([2_design.md §1](../2_design.md#1-roles)) |
| Mandate | The assignment task a resident works for, tagged `resident`, with `resident/mandate.json` ([mandate-and-authority](../specs/mandate-and-authority.md#mandate-artifact)) |
| Narrowing | Any change that reduces what a resident may do; open to any writer of the mandate ([mandate-and-authority](../specs/mandate-and-authority.md#narrowing)) |
| Probe | A mandate run interactively on purpose, to keep a baseline ([2_design.md §11.5](../2_design.md#115-deliberate-interactive-probes)) |
| Recovery cycle | A cycle that follows one which left no checkpoint ([2_design.md §5](../2_design.md#5-the-cycle)) |
| Relay | A `resident.answer` comment carrying the human's words verbatim with provenance ([manager-channel](../specs/manager-channel.md#orbitresidentrelay)) |
| Resident | The loop driving one mandate, run as Orbit cycles with no process between them ([1_overview.md](../1_overview.md)) |
| Signature | Kebab-case key naming a capability gap; groups cost events into one evolution record ([evolution-ledger](../specs/evolution-ledger.md#cost-event)) |
| Takeover | Control action that stops firing so a human or interactive session runs the mandate until `release` ([manager-channel](../specs/manager-channel.md#orbitresidentcontrol)) |
| Wake digest | Hash over the facts a mandate watches; a change makes it due ([2_design.md §4.2](../2_design.md#42-fires)) |
