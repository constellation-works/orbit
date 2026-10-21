---
summary: "User Interface — Overview"
type: design
title: "User Interface — Overview"
owner: gemini
last_updated: 2026-10-13
last_validated: 2026-10-19
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

- **Canon Refined and typography:** Layered dark surfaces, fine borders, compact spacing, and muted status colors; self-hosted `Geist` carries labels and prose while `Geist Mono` carries IDs, metrics, timestamps, code, and logs, the same pair the project website uses.
- **Surfaces:** The dashboard assets and `/api/*` handlers live in `crates/orbit-web/`.
  Static docs and project pages should reuse the same visual grammar without importing
  runtime-only dashboard assumptions.
- **Context contention:** Task-detail selectors are amber for backlog tasks and red
  for in-progress tasks when they overlap another in-progress task in the selected
  owning workspace. Each highlighted entry names linked competing tasks and their
  matching selectors. Review tasks do not cause highlights. The locks API computes
  these matches using the shared declaration expansion and selector overlap index;
  this display does not change admission or locking. Unavailable data (including
  aggregate views without a selected workspace) is shown as unknown. Lock refreshes
  update open details without discarding unsaved editors.

WORK › Drain (`#drain`) shows the auto-drain state, forecast, window controls,
admission passes and waits, fleet drains and locked files. Its Log view is
`#drain/log`. The same dock, with the Auto-drain status card, Start a window,
Locks held and the Log view, sits beside the task list on Tasks and gives way
to the task pane when a task opens.
The Drain page shows the fleet drains, including in **All workspaces** mode,
grouped under one heading per host: one line per workspace with its drain
phase and used/ceiling leaf slots, live drains first. Eligible tasks, the most
common wait reason, admission/provider holds and queued windows (naming any
Claude crew a queued window would run without a credential) add a second line
only when present. Selecting a row switches host and workspace using the dashboard scope
controls. The top-bar fleet indicator remains visible when an idle
workspace is selected; opening it selects the fleet overview.

`GET /api/fleet/drains` returns `{ items: [...] }` with one row per registered
workspace on the addressed host. Each row includes `workspace_id`, `workspace_name`,
`phase`, `slots_used`, `slots_ceiling`, `eligible_count`, `top_wait_reason`,
`holds`, and `queued_drains` (readiness's windows waiting for the live one). Failed/inactive workspaces stay visible with `phase: "unavailable"`, an
`error`, and null counts. Counts use the read-only readiness snapshot (up to 500
tasks); `counts_truncated` marks a lower bound, displayed with `+`. The top wait
is the most frequent reason among those tasks, with alphabetical ties. Slots
include shared leaf occupancy, matching the workspace Drain card.

The browser combines this endpoint across the serving host's registered hosts
through `/api/on/<host>/fleet/drains`, with at most three requests in flight.
A snapshot is kept for 60 seconds, longer than the dashboard's 30-second panel
poll, and that poll paints the indicator from the snapshot instead of asking
again. The snapshot is warmed once hosts are known and refreshed on its own
60-second timer, which does not run inside the panel poll and pauses while the
tab is hidden. The fleet card refetches only when its snapshot is older than
that cadence. Unreachable hosts remain error rows and the indicator marks
partial coverage. No recursive federation or workspace writes are performed.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Dashboard assets and HTTP API | `crates/orbit-web/assets/dashboard/`, `crates/orbit-web/src/api/` | Runtime tabs, tables, tiles, logs, and diagnostics. |
| Dashboard layout | `crates/orbit-web/assets/dashboard/` | `index.html` and the `app.js` entry module at the root with the npm manifest; `js/` holds the feature modules, `css/` the stylesheets, `fonts/` the self-hosted fonts, and `vendor/` the pinned third-party libraries. Routes mirror the tree under `/static/`. |
| Dashboard styles | `crates/orbit-web/assets/dashboard/css/` | One file per screen plus shared base, shell, and components layers; `DASHBOARD_CSS` in `lib.rs` joins them in cascade order and serves the result as `/static/dashboard.css`. |
| CLI adapter | `crates/orbit-cli/src/command/web.rs` | Delegates `serve` and `connect` to `orbit-web`. |
| Theme rules | `./specs/theme.md` | Canon Refined tokens and visual invariants. |
| Dashboard implementation | [crates/orbit-web/src/serve.rs](../../../crates/orbit-web/src/serve.rs), [assets.rs](../../../crates/orbit-web/src/assets.rs), [state/](../../../crates/orbit-web/src/state/), [api/](../../../crates/orbit-web/src/api/) | HTTP server, embedded dashboard assets, workspace state, and API routing. |

## Task References

- [T20260427-29] introduced the Canon Refined UI direction.
- [T20260430-24] tightened the UI design docs against shared conventions.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
