---
summary: "User Interface — Decisions"
type: design
title: "User Interface — Decisions"
owner: gemini
last_updated: 2026-09-12
last_validated: 2026-09-12
status: Draft
feature: user-interface
doc_role: decisions
tags: ["user-interface"]
---

# User Interface — Decisions

[Remove the planning duel and retain compatibility-only residue](../activity-job/4_decisions.md#remove-the-planning-duel-and-retain-compatibility-only-residue) records the proposal to remove the retired planning competition and its scoreboard projections. [ORB-10627] removes those UI surfaces; the proposal remains here as historical reasoning.

## Canon Refined Aesthetic

**Recorded:** 2026-07-26 21:51:43.494533Z · [T20260427-29], [ORB-10458]

### Context

The dashboard and project website need one visual identity. The prior Trading Terminal direction was dense but too rigid for hierarchical data, review threads, and mixed telemetry.

### Decision

Adopt Canon Refined: layered dark surfaces, `Inter` plus `JetBrains Mono`, soft semantic colors, compact spacing, and subtle radii.

### Consequences


- The UI keeps a serious pro-tool signal while allowing standard web affordances when they improve operator clarity.
- Cost: The design system must be maintained so Canon Refined does not drift into generic dark SaaS styling.


## Unified Denial Sources for Policy Dashboard

**Recorded:** 2026-05-11 02:06:39.445775Z · [T20260428-13]

### Context
The Denials 24h tile counted SQLite audit rows and v2 loop denials, but the Policy tab originally scanned only v2 loop JSONL files. Direct CLI denials could increment the tile while the detail table appeared empty.

### Decision
Aggregate v2 denial envelopes and SQLite `status = denied` audit events in the policy-denials endpoint. SQLite filesystem denials without an activity fsProfile use `workspace-boundary`.

### Consequences
- Audit > Policy is a faithful drill-down for Denials 24h, including direct `orbit tool run` policy denials.
- Cost: The endpoint carries a translation layer because SQLite audit rows lack typed denial fields like `profile` and `path`.

## Compact Scoreboard Ratio Columns

**Recorded:** 2026-05-11 02:06:39.447535Z · [T20260428-15]

### Context
The scoreboard had separate columns for output tokens, tool calls, duel wins/losses, and friction triage. After failed tool calls became first-class, the split counters made reliability harder to scan.

### Decision
Render companion metrics as compact pairs: `tokens` is `total/output`, `tool fail/all` is failed over all tool calls, and `duel w/all` is wins over participated duels. Keep only friction reports in the primary table.

### Consequences
- The table presents reliability and participation context in fewer columns, while `0/N` tool failures stays meaningful.
- Cost: Friction accepted/rejected counts and raw duel losses require summary JSON or a future detail view.

## Bounded Live Log Tail

**Recorded:** 2026-05-11 02:06:39.449202Z · [T20260430-29]

This layout was superseded by the Tasks side dock: Status and Log now share the
right column, and `#log-panel` fills the dock when Log is selected.

### Context
The Tasks view keeps `orbit.log` visible beside the task list, but the log panel could grow taller than short viewports and push footer controls below the screen.

### Decision
Keep the Tasks view in a two-column layout and size `#log-panel` to the available viewport. The log row stream owns overflow scrolling, while filters, buffered-count, and follow-tail controls remain inside the bounded panel.

### Consequences
- Operators get one clear scroll target for raw log rows while live-tail controls stay visible during short-screen monitoring.
- Cost: The Tasks view trades narrow-screen stacking for denser columns so the live log remains in the first viewport.

## Task References

- [T20260427-29] introduced the Canon Refined UI direction.
- [T20260428-13] unified policy-denial sources for the dashboard.
- [T20260428-15] compacted scoreboard ratio columns.
- [T20260430-24] tightened this decision log without changing decisions.
- [T20260430-29] bounded the live `orbit.log` tail panel.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Grouped Scoreboard Sections

**Recorded:** 2026-05-18 02:58:43.428001Z · [ORB-00144]

### Context
The scoreboard started as one compact per-agent table. Adding knowledge-artifact counters and planning-duel matrix data made the flat table mix delivery attribution, review work, operations, knowledge stewardship, and duel outcomes in one scan path. Alternatives were to keep widening the table, add column groups inside the same table, or split the view into focused sections.

### Decision
Render the dashboard scoreboard as focused sections: Delivery, Review, Knowledge, Operations, Planning Duels, a family-vs-family Duel Matrix, and Attribution Cleanup for non-canonical rows. Keep compact pair cells where they still help local interpretation, but do not treat the whole scoreboard as one primary flat leaderboard.

### Consequences
- Operators can inspect one contribution dimension at a time without conflating task creation, planning, implementation, review, tool usage, and knowledge artifacts.
- Non-canonical attribution rows stay visible but no longer compete with canonical agent families in primary sections.
- No single Rust code anchor; this is enforced by dashboard rendering and design review, and workspace-local ADR comments should not be embedded in shipped dashboard assets.
- Cost: Cross-section comparison now requires scanning multiple tables instead of one row, and future metrics must choose an explicit section before being added.

## Task References

- [T20260427-29] introduced the Canon Refined UI direction.
- [T20260428-13] unified policy-denial sources for the dashboard.
- [T20260428-15] compacted scoreboard ratio columns.
- [T20260430-24] tightened this decision log without changing decisions.
- [T20260430-29] bounded the live `orbit.log` tail panel.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Extract Dashboard + JSON API to orbit-dashboard Crate

**Recorded:** 2026-05-18 06:55:55.249402Z · [ORB-00146]

The extracted crate is now named `orbit-web`; it still owns the HTTP API and
embedded dashboard assets while `orbit-cli` remains the thin entry point.

### Context
The Orbit web dashboard lived inside orbit-cli even though its HTML, JavaScript, read-only axum API handlers, and embedded assets formed a distinct internal surface. The only local coupling was the CLI Execute trait; keeping the dashboard in orbit-cli forced unrelated CLI edits to rebuild the heavier web tree and mixed dashboard tests into the CLI target.

### Decision
Extract the dashboard assets, ServeArgs, JSON API handlers, router construction, browser opener, and serve(runtime, args) entrypoint into the internal orbit-dashboard crate. Keep orbit-cli as a thin delegator that wires the clap subcommand to orbit_dashboard::serve.

### Consequences
- orbit-cli no longer carries the direct axum dashboard dependency for command-only edits.
- Dashboard assets live beside the Rust server that embeds them, and dashboard tests compile under a dedicated crate.
- No single Rust code anchor; this is a crate-boundary decision enforced through architecture review.
- Cost: one more workspace crate and temporary duplication of a few projection helpers until a later shared projection layer exists.

## Unified Leaderboard Matrix Scoreboard

**Recorded:** 2026-05-18 06:56:13.678774Z · [ORB-00154]

### Context
[ORB-00154] found that the dashboard Scoreboard fragmented the four canonical agents across six stacked tables, repeated headers, sparse zero glyphs, and bare integers. Real alternatives included keeping grouped tables, switching to an agent-major wide table, using per-agent cards, or reducing the view to a pure heatmap.

### Decision
Render canonical scoreboard metrics as one metric-major Unified Leaderboard Matrix: metric rows grouped by Delivery, Review, Knowledge, Operations, and Planning Duels, with codex, claude, gemini, and grok as fixed columns. Non-zero metric cells carry inline bars scaled within the metric row, tied leaders get an explicit leader badge, zero values render as an em dash instead of a visible zero glyph, the Duel Matrix remains compact below, and Attribution Cleanup renders only when non-canonical agents have non-zero signal.

### Consequences
- Operators can identify the leading agent per metric by bar length and leader badge without comparing digit strings across repeated tables.
- The canonical agent set remains the primary row population while non-canonical attribution stays conditional.
- Rejected alternative: agent-major flat wide table; at roughly twenty metric columns it would require horizontal scrolling at the canonical dashboard viewport.
- Rejected alternative: per-agent card grid; it preserves the need to scan separate blocks to answer which agent leads a specific metric.
- Rejected alternative: pure heatmap matrix; color alone hides precise values needed for operator judgment.
- No single Rust code anchor; this UI convention is enforced in dashboard rendering and design review, and workspace-local ADR comments are not embedded in shipped dashboard assets.
- Cost: the matrix is denser and needs careful row-height discipline when new metrics are added.

## Global, Multi-Workspace Dashboard

**Recorded:** 2026-07-26 21:51:44.038593Z · [ORB-00030], [ORB-10458]

The single-workspace serving branch described below has since been retired.
`orbit web serve` now always enumerates the selected registry; `--global` is an
accepted compatibility no-op, and `--workspace` selects the initial workspace.

### Context

`orbit web serve` was coupled to a single workspace: the CLI eagerly initialized one `OrbitRuntime` (failing outside a workspace) and handed it to the dashboard as `Arc<OrbitRuntime>` axum state, so 46 of 48 handlers took `State(runtime): State<Arc<OrbitRuntime>>`. Operators wanted one dashboard over every workspace on the machine, launchable from any directory. `~/.orbit/workspaces.json` already enumerates workspaces and the SQLite stores already scope by workspace, so the missing piece was serving many runtimes from one process without rewriting every handler.

### Decision

Introduce `DashboardState` — a workspace-keyed, lazily-built runtime map — as the axum state, and a `Ws` extractor that selects the request's runtime from a `?workspace=<id>` query parameter (falling back to a configured default). Handlers change only their signature line (`State(runtime): State<Arc<OrbitRuntime>>` → `Ws(runtime): Ws`); bodies are untouched. `DashboardState::single` preserves the pre-existing single-workspace behavior (and every handler test). `orbit web serve` dispatches through a new `serve_from_env` before the CLI's eager runtime init, so it works from anywhere: inside a workspace without `--global` it stays single-mode; with `--global` or outside any workspace it enumerates the registry, skipping stale-path entries rather than failing. Two aggregate endpoints (`GET /api/workspaces`, `GET /api/tasks/all`) plus a header workspace selector and an "All workspaces" task view expose the machine-wide surface.

Rejected alternative: workspace-prefixed route paths (`/api/:workspace/tasks`). Rejected because it would rewrite all 48 route registrations and every frontend fetch path, versus a single query-param choke point in `common.js` and one-line handler signature swaps.

### Consequences


- One loopback dashboard can cover every workspace; per-workspace views drill down via the selector while Tasks offers a cross-workspace aggregate.
- The loopback-only bind guard (ORB-00360) is unchanged — global mode broadens data exposure only on the same machine, still with no network binding and no auth added.
- Runtimes are built on first access and cached, so an unopenable or stale workspace degrades to being skipped instead of failing startup.
- Cost: handlers that need a concrete workspace now depend on the `Ws` extractor's selection rules; the aggregate task endpoint reopens each workspace's store per request (no cross-workspace caching of task lists yet).


## Top-Level Dashboard Nav Is the Operator's Five Tabs

**Recorded:** 2026-07-26 19:14:18.916582Z · [ORB-10444]

### Context
Top-level nav is the dashboard's scarcest surface, and two of its six entries were not earning a slot: a deprecated review-threads tab with no backing view, and Scoreboard, a diagnostics-shaped read-only telemetry view sitting beside the operator workflow tabs.

### Decision
The top-level nav is exactly Tasks, Audit, Diagnostics, Operations, Knowledge (plus the hash-only run-detail route). The deprecated tab is removed outright rather than hidden. Scoreboard becomes a Diagnostics subtab routed as #diagnostics/scoreboard, with its markup moved verbatim so the /api/scoreboard contract is untouched.

### Consequences
- The nav reads as the operator workflow; telemetry lives one level down.
- Existing #scoreboard bookmarks fall back to Tasks.
- Cost: the diagnostics pane owns two main elements and a visibility toggle keyed on the active subtab.

## One-Click Task Ship and Human-Attributed Dashboard Comments

**Recorded:** 2026-07-26 19:14:19.126579Z · [ORB-10444]

### Context
The dashboard Tasks tab was read-only for the operator two most common actions: dispatching a backlog task and leaving a note on one. Both are writes against live state, so the question was how much configuration to expose and whose identity to record.

### Decision
Ship is one click with no configuration UI: the dashboard posts only the task id. The crew comes from the task record and the mode from the workspace registry binding, so the ship endpoint omitted-mode default changes from a hard-coded pr to that binding ship mode. Duplicate dispatch is refused server-side with 409 ship_run_in_flight when the task already has a non-terminal run. Comments post to POST /api/tasks/:id/comments, writing into the task existing review-thread structure and forcing a human author.

### Consequences
- Triage, dispatch and annotation complete inside the dashboard.
- A dashboard comment is always attributable to a person, even when the server runs inside a managed Orbit run.
- Cost: the ship endpoint scans a bounded window of recent runs before submitting, and a stuck non-terminal run must be cancelled before its task can be re-shipped.

## Pipeline reliability from durable run state, with roles discovered from the job catalog

**Recorded:** 2026-08 · [ORB-10588]

**Context.** The dashboard surfaced no measure of pipeline reliability. How often job runs fail, and how often the recovery path fires, were answerable only by opening SQLite by hand. A read-only analysis over a 30-day window found the recovery activity to be the second most common activity in the store — roughly one recovery invocation per 3.6 implementation attempts — a large continuous cost nobody had chosen to accept because nobody could see it. Three constraints shaped the design. The worker run store and orbit's `invocations` table disagree by an order of magnitude on tokens per run and some runs carry no cost figure at all, so any rate built on token or cost fields would display a confidently wrong number. The dashboard seeds every workspace, so no caller-specific workspace name, id prefix, crew name, or hardcoded activity-id list may appear in it. And a rate without a denominator and a stated time range is not actionable.

**Decision.** Compute both rates entirely from persisted `job_runs` and `invocations` rows via count-only store queries that reference no token or cost column, and discover activity roles from the job catalog at query time.

`RunOutcome` partitions the observed `job_runs.state` values. `success` is succeeded. `failed`, `timeout`, and `interrupted` are failures — all three are runs the pipeline intended to finish and did not. `cancelled` (a deliberate operator action) and `skipped` (never ran) are terminal but sit outside the rate. `pending`, `running`, and `retrying` are in flight. Anything unparseable lands in an explicit `unknown` bucket rather than being folded into an outcome. The failure rate divides by settled runs (`succeeded + failed`) only, and the payload carries the total and each excluded bucket so the UI cannot imply that ok + failed is the population.

`JobV2::activity_roles()` walks the declared job structure at any nesting depth and returns the step-activity and recovery-activity sets. Recovery activities are exactly those a job names via `recovery_activity` at job or step level — a property of the workspace's own job definitions, never of Orbit. An activity a catalog uses in both roles is reported as ambiguous and excluded from the numerator, because the store records only an id and cannot say which role a given invocation played.

Every rate is a `Rate { numerator, denominator, denominator_label, value, low_sample }`. `value` is `None` when the denominator is zero, so 0% can never stand in for no data. `low_sample` is set below a threshold of 20 and the frontend withholds the percentage for such cells, showing raw counts and an explicit marker. The window (`label`, `since`, `until`, `bucket`) travels with the payload, and the endpoint refuses an unbounded `all` window outright.

Rejected alternative: extend the existing token-metrics path (`list_activity_invocation_metrics`, the scoreboard cost aggregation). That path selects and aggregates token and cost columns, which are known to disagree across stores. Building reliability on it would put an untrustworthy input in the query path even where the specific figures went unused, and would make the no-token-input property unverifiable by inspection. A dedicated count-only read is a few dozen lines and is checkable.

Rejected alternative: reuse `list_job_runs_for_workspace` rather than adding a projected read. It hydrates full `JobRun` records and issues a per-run step query, so a 30-day window fans out to thousands of extra reads for three fields.

Rejected alternative: enumerate the recovery activity ids in source. The dashboard seeds every workspace; a hardcoded list would be wrong elsewhere and stale here. Discovery costs one already-existing catalog call.

Rejected alternative: count `cancelled` as a failure, or round small-`n` rates instead of withholding them. The first makes deliberate operator intervention register as pipeline breakage; the second turns a 1-of-3 bucket into a 33% spike the evidence does not support.

**Consequences.**
- Both rates are visible over an explicit window, broken down by workspace, by job, and over time, so a spike is attributable rather than merely alarming.
- The recovery rate is reported two ways with distinct denominators — per step-activity invocation, and per job run with any recorded invocation — both labelled in the payload and rendered in the UI.
- Distinct-run coverage needs its own single-pass store query: distinct-run counts do not compose, so summing per-activity `COUNT(DISTINCT job_run_id)` would overcount runs touched by more than one recovery activity.
- `invocations` has no `workspace_id`, so it is scoped by joining each row back to its owning `job_runs` row. An invocation whose run is absent is excluded rather than attributed arbitrarily.
- The identifier in `invocations.activity_id` is **not** uniformly the catalog activity name: the job executor records a dispatched step under its **step id**, a recovery dispatch under the **recovery activity name**, and the planning-duel runner under the **activity name**. The step role set therefore holds both the step id and the target's catalog name. This is a latent trap for any future consumer of `activity_id`; an end-to-end test pins it.
- Rates window and bucket on `created_at`, not `finished_at`, so a long-running run is attributed to when it started. Windows are half-open, so adjacent windows tile without double-counting.
- The per-run fact read is capped at 200,000 rows and reports `truncated` when the cap binds; the UI warns rather than presenting a partial window as complete.
- This adds the instrument; it does not assert the readings are stable. The standing measurement hold on efficiency baselines drawn from the current window is unaffected.

## All-or-Nothing Rejection of Unsupported Task Body Fields

**Recorded:** 2026-08 · [ORB-10648]

The summary below and [ORB-10648] are the complete surviving record; no separate narrative body survives.

Summary: `POST /api/tasks` and `PATCH /api/tasks/:id` no longer discard keys they
do not declare. Unknown keys are captured with `#[serde(flatten)]` and refused
with a 400 that names them, `priority` becomes a supported update field end to
end, `model` becomes declared provenance, and `agent` is a trap field pointing at
`model` — extending the ORB-00042 `workspace` trap shape to the whole body.

## Task References

- [T20260427-29] introduced the Canon Refined UI direction.
- [T20260428-13] unified policy-denial sources for the dashboard.
- [T20260428-15] compacted scoreboard ratio columns.
- [T20260430-24] tightened this decision log without changing decisions.
- [T20260430-29] bounded the live `orbit.log` tail panel.
- [ORB-00144] grouped scoreboard metrics and added knowledge counters plus duel matrix data.
- [ORB-00146] extracted the dashboard and JSON API into the new `orbit-dashboard` internal crate (this document).
- [ORB-00154] unified the Scoreboard tab into a metric-major leaderboard matrix.
- [ORB-00030] made the dashboard global/multi-workspace (workspace-keyed state, `Ws` extractor, serve-from-anywhere, aggregate endpoints).
- [ORB-10444] retired the deprecated tab, folded Scoreboard under Diagnostics, pinned the Knowledge detail pane, and added task ship + comments.
- [ORB-10588] added the Reliability subtab: job-run failure rate and recovery invocation rate from durable run state.
- [ORB-10648] made the task create/update bodies reject unsupported fields instead of discarding them silently.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
