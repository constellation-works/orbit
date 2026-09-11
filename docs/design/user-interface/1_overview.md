---
summary: "User Interface — Overview"
type: design
title: "User Interface — Overview"
owner: gemini
last_updated: 2026-08-15
last_validated: 2026-09-11
status: Draft
feature: user-interface
doc_role: overview
tags: ["user-interface"]
paths: ["crates/orbit-web/**", "crates/orbit-cli/src/command/web.rs"]
---

# User Interface — Overview

Orbit UI covers the dashboard and HTTP API owned by `orbit-web`; `orbit-cli` is the thin
`orbit web serve` / `connect` command adapter. It gives operators a dense, legible way to
monitor agents, workflows, telemetry, and audit signals. Terminal output — what `orbit`
writes to stdout — is a separate surface owned by
[terminal-interface](../terminal-interface/1_overview.md); the two share operator
vocabulary and status semantics but no tokens, components, or rendering assumptions.

## 1. Motivation

Agent runs produce more state changes, logs, and diagnostics than a human can read linearly. The UI therefore optimizes for scan density, clear status recognition, and quick drill-downs. Canon Refined keeps the pro-tool feel while using readable sans-serif text, subtle rounding, and restrained semantic color [T20260427-29].

## 2. Core Concepts

- **Canon Refined and typography:** Layered dark surfaces, fine borders, compact spacing, and muted status colors; `Inter` carries labels and prose while `JetBrains Mono` carries IDs, metrics, timestamps, code, and logs.
- **Surfaces:** The dashboard assets and `/api/*` handlers live in `crates/orbit-web/`.
  Static docs and project pages should reuse the same visual grammar without importing
  runtime-only dashboard assumptions.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Dashboard assets and HTTP API | `crates/orbit-web/assets/dashboard/`, `crates/orbit-web/src/api/` | Runtime tabs, tables, tiles, logs, and diagnostics. |
| CLI adapter | `crates/orbit-cli/src/command/web.rs` | Delegates `serve` and `connect` to `orbit-web`. |
| Theme rules | `./specs/theme.md` | Canon Refined tokens and visual invariants. |
| Dashboard implementation | [crates/orbit-web/src/lib.rs](../../../crates/orbit-web/src/lib.rs) | HTTP server, embedded dashboard assets, workspace state, and API routing. |

## Task References

- [T20260427-29] introduced the Canon Refined UI direction.
- [T20260430-24] tightened the UI design docs against shared conventions.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
