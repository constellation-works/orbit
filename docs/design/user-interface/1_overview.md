---
summary: "User Interface — Overview"
type: design
title: "User Interface — Overview"
owner: gemini
last_updated: 2026-10-20
last_validated: 2026-10-24
status: Draft
stage: shipped
next: "—"
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

The Tasks dock holds the Auto-drain status and forecast, a simple **Start a
window** card, **Locks held** and **Log**. It gives way to the task pane when
a task opens. Below 1000px the list keeps the full width, and an explicit Drain,
Locks held or Log link shows the dock in its place until **Back to tasks**.
Locks held lists holders by task or locked paths by file, marking a path more
than one task holds, with a
filter by task ID. On a replica checkout, Start pulls directly from the
owner; the Auto-drain card can stop that pull's admissions.

The top-bar fleet indicator opens the Tasks dock in **All workspaces** mode.
There the Auto-drain card lists fleet drains, one tab per host when more
than one is registered (the chosen tab is remembered), with phase and
used/ceiling leaf slots for each workspace. Live drains lead; eligible work,
waits, holds and queued windows add a second line only when present.
Selecting a row switches host and workspace. The indicator remains visible
when an idle workspace is selected. Legacy `#drain`, `#drain/start` and
`#drain/log` links open the corresponding Tasks dock content.

`GET /api/fleet/drains` returns `{ items: [...] }` with one row per registered
workspace on the addressed host. Each row includes `workspace_id`, `workspace_name`,
`phase`, `slots_used`, `slots_ceiling`, `eligible_count`, `top_wait_reason`,
`holds`, and `queued_drains` (readiness's windows waiting for the live one).
`drain_phase`, `pull_active`, and `pull_admissions_stopped` preserve independent
drain/pull states for Settings Hosts, including a pull concurrent with a drain.
These fields are null on unavailable workspaces. Failed/inactive workspaces stay visible with `phase: "unavailable"`, an
`error`, and null counts. Counts use the read-only readiness snapshot (up to 500
tasks); `counts_truncated` marks a lower bound, displayed with `+`. The top wait
is the most frequent reason among those tasks, with alphabetical ties. Slots
include shared leaf occupancy, matching the workspace Drain card.

The browser combines this endpoint across the serving host's registered hosts
through `/api/on/<host>/fleet/drains`, with at most three requests in flight.
Completed hosts are shown as partial results while outstanding hosts are
marked `loading` (distinct from idle and unavailable); when a pending host
succeeds or fails, only its rows update and ready rows and the selected
workspace are preserved. Partial results are not cached as a fresh snapshot.
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
