---
summary: "User Interface — Design"
type: design
title: "User Interface — Design"
owner: gemini
last_updated: 2026-09-10
last_validated: 2026-09-10
status: Draft
feature: user-interface
doc_role: design
tags: ["user-interface"]
---

# User Interface — Design

This document describes the current Orbit UI implementation: the local dashboard assets, the Canon Refined visual rules they rely on, and the telemetry behaviors that must stay consistent with backend data.

## 1. Dense Layout

The dashboard favors wide, dense tables and panels over narrative screens. Tight spacing, small radii, and expandable sunken detail rows preserve hierarchy without hiding root lists. The scoreboard groups per-agent metrics into Delivery, Review, Knowledge, Operations, and Attribution Cleanup sections so task attribution, review work, tool reliability, and knowledge artifacts are scanned separately. Compact pair cells stay local to the sections where they add context: `tool fail/all` is failed over total tool calls, and every abbreviated metric or leader mark has a plain-language title or accessible name [T20260428-15] [ORB-00144] [ORB-10873] [Grouped Scoreboard Sections](./4_decisions.md#grouped-scoreboard-sections).

The selected window also includes a compact Notable completions list built only from durable task fields (id, title, priority, type, optional `impact:*` tag, completion timestamp, bounded `execution_summary` excerpt). Selection is priority then completion recency, labeled as a reading order rather than a quality score. Empty Review and other snapshot-backed sections distinguish no observed events from unavailable windowed coverage: PR comments come from a lifetime snapshot with no per-event timestamps, so a finite window reports that limitation instead of "no activity". Window selectors are `role="tablist"` buttons with `aria-selected`, keyboard movement, and a visible focus ring [ORB-10873].

## 2. Layered Palette

The UI uses layered dark surfaces instead of flat black: base canvas, elevated panels, sunken wells, and accent washes. Status color should stay muted and distinct; exact token values live in `./specs/theme.md` and the dashboard CSS.

## 3. Typography

`Inter` carries labels, headings, and prose. `JetBrains Mono` is reserved for IDs, metrics, timestamps, code, and log streams so numeric and diagnostic data stays aligned.

## 4. Live Status

Live processing is visible through pulsing dots, spinners, buffered-log counters, periodically refreshed tiles, and compact ticker-style values. The `orbit.log` panel is viewport-bounded; overflowing rows scroll inside the log stream so footer filters and follow-tail controls remain visible [T20260430-29]. Motion is functional: it points to active work without making the operator read raw logs first.

The log panel can be collapsed to its header or resized by dragging (or, with focus, arrow-key nudging) its top edge, and the chosen presentation is remembered per browser via `localStorage` rather than the URL, since it is a local viewing preference rather than shared state [ORB-10874]. The task list beside it keeps an independent, guaranteed-minimum body height regardless of the log panel's state, and below ~760px the two-column Tasks layout stacks into one column instead of squeezing both into slivers.

## 5. Dashboard Telemetry Consistency

Summary tiles and drill-down panels must agree. Audit > Policy is the detail view for the Denials 24h tile, so `/api/diagnostics/denials` combines v2 loop JSONL denial rows with SQLite `status = denied` audit events. SQLite filesystem boundary denials without an activity fsProfile use the stable `workspace-boundary` label [T20260428-13].

Workspace and time window are one dashboard scope after [ORB-10872]. `?workspace=` and `?window=` plus the diagnostics hash (`#diagnostics/scoreboard?window=7d`) are the source of truth. Scoreboard delivery/operations and the Managed Execution cost/token panel share that window: a 7d selection fetches one `/api/scoreboard?window=7d` payload, and a 24h body is refused rather than painted under a 7d selector. Audit Events honor the same window as `since` and open from an actor or metric click with removable chips for actor, workspace, window, outcome/status, and source metric. Reliability stays fleet-wide (ORB-10588) and labels that exception — `scope: "fleet"` plus a Fleet-wide badge — so the header workspace selector cannot imply it applies; if the dashboard window is unbounded `all`, Reliability keeps an independent labeled 7d cutoff because a rate without a range is not actionable. Half-open cutoffs (`since ≤ t < until`) from ORB-10609 are unchanged.

Run Detail > Steps now includes compact per-step agent log expanders for CLI-backed activity steps [T20260508-14]. The UI renders bounded stdout and stderr previews from `/api/runs/:id/logs`, distinguishes stderr blocks from stdout blocks, highlights structured `ERROR <target>:` lines, and keeps blob references behind the API so operators do not need to resolve content hashes manually.

Diagnostics has an Errors sub-tab after [T20260508-14]. It renders recent backend error rows independently of Metrics and Policy, combining Orbit process ERROR events with structured agent stderr rows. Rows with `job_run` provenance route back to the owning Run Detail step so error triage stays connected to workflow context.

Diagnostics has an Incidents sub-tab after [ORB-10871]. A raw failed audit row is evidence, not an incident: one refusal repeated in a burst is one problem, and one failed run that propagates its failure up through its enclosing steps is one root cause with a chain beneath it. `/api/audit/incidents` groups the window's failed and denied audit rows by `(job run, signature)`, where the signature is `class | actor | surface | normalized message` and volatile tokens (paths, ids, numbers, timestamps, hashes) are replaced with placeholders; same-run clusters of the same class within a 60s cascade window collapse onto their earliest cluster, which becomes the incident root, with the rest recorded as its propagation chain. After [ORB-10969], a later incident whose raw message cites another incident's `job_run_id` (the parent/child `pipeline_success_guard` pattern) also folds onto that cited root, so one failed leaf is one incident with its cascade count rather than independent roots. Grouping reads only durable audit columns, so no tool, agent, workspace, or task is special-cased.

Three counts are always rendered together with the selected window — raw failed events, grouped incidents, and affected runs. The panel header reads `N incidents / M failed events / R affected runs`, and the summary restates those three against the audited-event denominator. Failures stay classified as policy denials, expected negative paths (an `OrbitError`-derived caller-input refusal), or unexpected failures; classes are counted separately and never merged into one incident. Audit > Failures by tool after [ORB-10969] excludes rows with no tool identity from tool denominators and rates (they are not a synthetic tool named `unknown`) and labels them as `job-run lifecycle` beside the tool cards. The scoreboard keeps its raw `tool fail/all` pair and gains a `fail inc/events` pair beside it, so a burst can no longer inflate an agent's apparent failure rate. Nothing is dropped: an incident expands to its grouping signature, actor, surface, run/task/tool ids, first/last timestamps, and every underlying audit row, and links back into the raw Audit view — which still returns every event. Labels and denominators wrap rather than clip at the 720px and 520px breakpoints.

Diagnostics no longer has a Friction sub-tab after [ORB-00060]. The Friction name is reserved for append-only `.orbit/frictions/` artifacts, while audit-derived negative run signals stay visible in Recent Runs. Recent Runs joins `/api/job-runs` with `/api/diagnostics/friction` client-side by run id (`run_id`/`job_run`) and keeps the table sortable across `denials`, `tool fails`, and `duration`; the duration cell can carry the long-run flag when the diagnostics source supplies one. This preserves column continuity with the existing compact dashboard telemetry direction from [T20260428-15].

Knowledge is a top-level dashboard tab for friction triage. The retired native learning panels, routes, metrics, and mutations were removed by [ORB-10736].

Knowledge detail panels stay pinned while the artifact list scrolls after [ORB-10444]. The list grows well past a viewport, and scrolling it used to carry the pane the operator was reading out of view. The detail panel is sticky below the fixed chrome (header, tabs, health strip) and bounded to the remaining viewport height, so detail content taller than the screen scrolls inside the pane rather than being clipped. The single-column breakpoint unpins it, where the pane already stacks below the list.

## 6. Top-Level Navigation

The top-level nav carries five operator workflow surfaces — Tasks, Audit, Diagnostics, Operations, and Knowledge — plus the hash-only `run-detail` route [ORB-10444] [ORB-10875]. Operations owns routine and host-clock state; it is top-level because disabling unattended execution is an operational action rather than diagnostic telemetry. A deprecated review-threads tab was removed outright rather than hidden: nav entry, route, pane, refresh branch, and styles all went, so no dead asset ships and no route resolves to a missing pane. Scoreboard is diagnostics-shaped telemetry rather than a workflow surface, so it remains under Diagnostics as the `#diagnostics/scoreboard` sub-tab.

## 7. Task Write Actions

The Tasks tab is writable for the two actions that otherwise force a context switch to the CLI [ORB-10444] [One-Click Task Ship and Human-Attributed Dashboard Comments](./4_decisions.md#one-click-task-ship-and-human-attributed-dashboard-comments).

**Ship** appears on `backlog` tasks and is one click with no configuration UI: it posts only the task id to `POST /api/workflows/ship`. The crew is resolved by the pipeline from the task's own record and the mode from the selected workspace's registry binding, so that endpoint's omitted-`mode` default is the workspace ship mode (falling back to `pr` for a runtime with no binding). The resulting run id and state are surfaced as a notice, and a failed dispatch shows the server's error text instead of silently no-opping. Duplicate dispatch is refused: an explicit task selection whose id is already carried by a non-terminal run answers `409` with code `ship_run_in_flight` naming that run, and the UI holds a per-task guard across the double-click window. That refusal is the shared ship submission path's typed conflict, not an endpoint-local policy: the MCP tool, routine action, and interactive CLI all refuse the same duplicate ([ORB-10631], [Ship duplicate-dispatch guard lives in the shared submission path](../activity-job/4_decisions.md#ship-duplicate-dispatch-guard-lives-in-the-shared-submission-path)).

**Comments** post to `POST /api/tasks/:id/comments`, which writes through the task's existing review-thread structure rather than adding a field to the task record, so a comment survives a reload like any other task history. Authorship is forced to a human identity: an absent, agent-family, or model-constant author collapses to the `human` label, because the dashboard process may itself run inside a managed Orbit run where the runtime's ambient actor is a model.

Inline status/crew edits show a pending state on the control, refuse a duplicate submission while one is in flight, and report durable success or exact failure text next to the control (a live region, so the same feedback reaches assistive tech) rather than only logging to the console. The status control exposes every other existing status for every task, including reopening `done`, direct archival, and restoration from `archived`; the PATCH is an audited classification edit and never launches, cancels, merges, or completes work by itself. A move into `backlog` can make the task eligible for a later configured automation sweep. A successful edit offers a bounded-window undo that restores the prior value with one click, expiring automatically once the window (or a further edit) makes restoring it unsafe [ORB-10874].

In the aggregate ("All workspaces") view there is no ambient workspace to scope a mutation to, so status/crew edits are refused unless the task carries its own `workspace_id` (present on every `/api/tasks/all` row), in which case the mutation is sent explicitly qualified to that workspace rather than the currently-selected one [ORB-10874].

## 8. Task Count, Filters, and the Tasks/Log Layout

The Tasks count uses the `/api/tasks` or `/api/tasks/all` paging envelope (`{ items, total, limit, truncated, offset, next_cursor }`) to render the current page range and matching total rather than an ambiguous `N/50`. For example, a page with twenty items at offset zero of fifty-five matches reads `1–20 of 55`; the final fifteen-item page reads `41–55 of 55`. An empty page reads `0 of N`. If a client-side filter narrows the loaded page, the count prefixes that range with the number shown, such as `3 shown · page 1–20 of 55`. The count does not name a server page limit or describe a fetched-versus-filtered relationship. The active status chips are sent to the server as an explicit `status=` filter rather than fetched wholesale and narrowed client-side, so the envelope's `total` describes the filter actually in effect [ORB-10874].

The Tasks pager has Previous and Next buttons. Previous follows the saved cursor history and is enabled after moving beyond the first page; Next follows `next_cursor` when the server supplies one and is disabled on the last page. Both controls are disabled while a page is loading. The adjacent polite live region reports `Loading task page…` or `Page failed: …`, and changing the status or search filters resets pagination to a new first-page request.

The active status filter and search query are represented in the `#tasks` hash (mirroring the Audit tab's own hash-encoded filters) and restated as plain text next to the count, so the current view survives a reload or the browser's back/forward button and is legible without reading each chip's color. The selected workspace is likewise mirrored into the page's `?workspace=` query parameter on every change [ORB-10874].

## 8a. Task Artifact Previews

Artifact rows in task detail stay one compact line — path, media type, size — and fetch a payload only when clicked, so a task carrying a screenshot is no more expensive to scan than any other [ORB-11365].

A supported raster artifact (`image/png`, `image/jpeg`, `image/gif`, `image/webp`) renders as an actual image rather than a download link. The preview scales to the detail column (`max-width: 100%`, `height: auto`) so it never forces the panel to scroll sideways, and it is never upscaled past its own resolution. Below 760px it takes the full column width and is capped at `60vh` so a tall screenshot cannot push the rest of the task off screen. A checkerboard background makes a transparent PNG read as transparent instead of as whatever the panel happens to sit on. Each preview carries Open (new tab) and Download controls, and the artifact path is the image's alt text — it is the only description the artifact has.

When the browser cannot decode the bytes, the preview says so and still offers the file, rather than leaving a broken-image icon.

The set of types rendered inline mirrors the server's `nosniff` allowlist exactly. `image/svg+xml` and `text/html` are absent from both: they are viewable formats that are also script hosts, so they stay downloads. Everything else keeps its existing text render or download link.

## 9. Operations

Operations renders routine-definition state separately from the host sweep clock [ORB-10875]. Each routine row names its source workspace, cron schedule, catalog target, host pins, last scheduler evaluation/fire, linked run outcome, and next due slot. Definition `enabled` is the versioned switch; the clock card independently reports its native provider, configured/effective cadence, loaded/active health, and native last/next tick values when available.

Routine toggles and clock controls require one concrete workspace and the exact local host returned by the status response. All-workspace mode stays readable but has no active controls. Each mutation carries its displayed target plus expected prior state, so a delayed or duplicate click conflicts instead of overwriting a newer observation. The UI also holds an in-flight guard, renders pending state, and reports the server's exact success or failure. Starting/stopping the native service requires confirmation; cadence is a separate control and its feedback explicitly preserves the service-state distinction. The backend accepts typed actions only and resolves routine paths and native commands itself—browser input is never interpreted as shell text.

The two-column desktop layout stacks below 900px, and routine/clock metadata collapses to one column below 600px so schedules, state labels, and controls remain scannable at 480–720px widths.

## 10. Concerns & Honest Limitations

Accessibility still needs a real WCAG pass; responsive behavior remains optimized for wide desktop viewports; raw HTML, CSS variables, and dashboard JavaScript keep the runtime simple but leave duplication across project surfaces.

## Task References

- [ORB-11365] added the responsive task-artifact image preview and its open/download controls.
- [T20260427-29] introduced the Canon Refined UI direction.
- [T20260428-13] unified dashboard denial sources for the policy drill-down.
- [T20260428-15] compacted scoreboard ratio columns.
- [T20260430-24] shortened this design doc while preserving current behavior statements.
- [T20260430-29] bounded the live `orbit.log` panel to the viewport.
- [T20260508-14] added Run Detail agent-log previews and Diagnostics > Errors.
- [ORB-00060] collapsed Diagnostics > Friction into Recent Runs diagnostics columns.
- [ORB-10736] removed the native learning curation surface while preserving friction triage.
- [ORB-00144] grouped scoreboard metrics and added knowledge counters.
- [ORB-10444] retired a deprecated tab, folded Scoreboard under Diagnostics, pinned the Knowledge detail pane, and added task ship + comments.
- [ORB-10874] clarified the Tasks count and filter state, made the log panel collapsible/resizable, and added pending/undo feedback and an aggregate-mode mutation guard to inline task edits.
- [ORB-10875] added the Operations view and typed routine/clock controls.
- [ORB-10872] made workspace and time window one dashboard scope across Scoreboard, Audit, Reliability, and Managed Execution.
- [ORB-10871] made dashboard failure metrics incident-aware: grouped incidents and raw failed events are reported side by side with their denominators and window.
- [ORB-10969] restored that grouping for job-run lifecycle rows: Failures-by-tool no longer treats missing tool names as `unknown`, parent/child guard copies collapse onto the leaf, and the UI names raw events, incidents, and affected runs separately.
- [ORB-10873] added Scoreboard notable completions, honest coverage language, labeled abbreviations, and accessible window tabs.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
