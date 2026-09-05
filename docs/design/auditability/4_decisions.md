---
summary: "Auditability — Decisions"
type: design
title: "Auditability — Decisions"
owner: codex
last_updated: 2026-08-11
last_validated: 2026-08-29
status: Draft
feature: auditability
doc_role: decisions
tags: ["auditability"]
---

# Auditability — Decisions

This is the decision log for Auditability. Entries stay in historical order and use title-based links. New entries should use the template in [../CONVENTIONS.md](../CONVENTIONS.md) and cite the task that made the decision real.

---

## Dedicated auditability design ownership

**Recorded:** 2026-05-11 02:06:39.308694Z · [T20260426-0605]

### Context
Auditability is a primary Orbit feature, but its implementation and rationale were spread across README prose, Activity / Job docs, SQLite audit code, loop audit code, and redaction utilities.

### Decision
Create `docs/design/auditability/` as the canonical auditability design folder, owned by codex.

### Consequences
- Audit decisions now have one decision log and one glossary.
- Cost: auditability overlaps with Activity / Job docs, so cross-links must stay current instead of duplicating the full runtime design.

## Command audit rows stay compact and queryable

**Recorded:** 2026-05-11 02:06:39.310068Z · [T20260426-0605]

### Context
CLI commands need durable, filterable history across processes, but full provider payloads would make routine queries noisy and expensive.

### Decision
Keep command audit records as compact SQLite rows with command, target, role, status, timing, working directory, and optional argument/error fields; store transcript detail in JSONL and blobs.

### Consequences
- `orbit audit list/show/stats/export` can stay fast and table-shaped.
- Cost: complete incident reconstruction may require joining command rows with job state and file-backed traces.

## V2 run structure and loop transcript detail are separate audit layers

**Recorded:** 2026-05-11 02:06:39.311565Z · [T20260419-0002]

### Context
Activity/job execution needs run, step, retry, fan-out, loop, and activity structure. Provider loops need HTTP, tool-call, payload, and session detail.

### Decision
Use `V2AuditEnvelope` for activity/job structure and `LoopAuditEvent` for provider/tool detail, connected through run ids and parent event ids.

### Consequences
- Workflow replay can traverse a run tree without loading every provider payload.
- Cost: reviewers need tooling or documentation to move between related files.

## File-backed run traces are workspace-local state

**Recorded:** 2026-05-11 02:06:39.312800Z · [T20260426-0519]

### Context
V2 JSONL and blob traces are runtime artifacts, but their old first-level `.orbit/audit/` path blurred command audit, workspace state, and authored docs.

### Decision
Store activity/job envelopes, loop events, and blobs under `.orbit/state/audit/`; keep command audit rows in the configured SQLite database.

### Consequences
- Runtime traces live with other workspace-local run state.
- Cost: old `.orbit/audit/` artifacts may need manual fallback or migration for historical reconstruction.

## Redaction is a write-side durability boundary

**Recorded:** 2026-05-11 02:06:39.313914Z · [T20260426-0605]

### Context
Audit needs useful payloads for reproducibility, but raw provider keys or sensitive environment-derived values would make the trail unsafe by default.

### Decision
Redact sensitive environment values, HTTP authorization patterns, API-key fields, bearer tokens, and selected argv token shapes before durable blob or error-message persistence.

### Consequences
- Audit readers can treat normal stored blobs as already redacted.
- Cost: redaction changes payload hashes and may remove exact bytes useful for reproducing a provider interaction.

## Invocation metrics are audit-adjacent primary records

**Recorded:** 2026-05-11 02:06:39.315068Z · [T20260426-0526]

### Context
V2 job execution emits audit JSONL, but metrics and scoreboards read the invocation store. Scraping audit logs would couple reporting to transcript format and retention.

### Decision
Persist `InvocationTrace` records beside audit as first-class metric records keyed by job run, activity, task ids, agent, model, usage, and tool-call summaries.

### Consequences
- Dashboard metrics endpoints and scoreboards can avoid parsing audit JSONL.
- Cost: metrics can diverge from transcript detail if a provider path reports incomplete usage.

## Dashboard owns invocation metrics surfaces

**Recorded:** 2026-05-20 04:57:08.297992Z · [ORB-00190]

### Context
The metrics CLI surface is unused, and ORB-00191 moved the missing knowledge, activity, tool, task, and invocation views into dashboard HTTP endpoints. Keeping a second JSON-capable command would make future metrics work maintain two surfaces.

### Decision
The dashboard is the canonical user-facing and programmatic surface for invocation metrics. The metrics CLI command is retired, and future observability features should ship as dashboard endpoints and views.

### Consequences
- Programmatic consumers use the dashboard HTTP API (`/api/metrics/*`) instead of a dedicated CLI JSON scripting surface.
- Future invocation-metrics features build as dashboard endpoints first.
- No single code anchor; this convention is enforced through design docs and review.
- Cost: shell scripts cannot rely on a dedicated metrics command and must call the local dashboard API or shared runtime libraries.

## Run trace inspection stays separate from command audit

**Recorded:** 2026-05-11 02:06:39.316321Z · [T20260426-0705], [T20260426-0709]

### Context
Operators need first-class commands for activity/job envelope JSONL, but `orbit audit` is the compact SQLite command-audit surface.

### Decision
Expose v2 envelope inspection under `orbit run events` and `orbit run trace`, and keep envelope/blob parsing behind orbit-core runtime accessors.

### Consequences
- Command history and run-local workflow traces have dedicated commands.
- Cost: users must learn that `orbit audit` and `orbit run events/trace` answer related but different questions.

## Process tracing feed is global JSONL

**Recorded:** 2026-05-11 02:06:39.317474Z · [T20260426-2343]

### Context
CLI subprocess output emits structured tracing events after [T20260426-2313], but subscriber initialization happens before Orbit resolves a workspace root.

### Decision
Append process-level tracing events to `~/.orbit/state/logs/orbit.jsonl` through the default subscriber using the same `EnvFilter` as stderr and a retained non-blocking writer.

### Consequences
- Operators and dashboards can tail one machine-readable feed across workspaces.
- Cost: the v1 file is unrotated and concurrent processes can rarely interleave oversized JSONL records.

## Tracing redaction is enforced by field formatting

**Recorded:** 2026-05-11 02:06:39.318605Z · [T20260426-2349]

### Context
A durable JSONL feed made tracing output persistent, but call-site helpers only protected emitters that remembered to use them.

### Decision
Install redacting `FormatFields` implementations on stderr and JSONL tracing formatters so string fields, `Debug` values, and messages are scrubbed before output.

### Consequences
- New structured tracing emitters inherit default redaction before terminal or disk output.
- Cost: span attribute redaction, binary payload redaction, and user-configurable policies remain follow-up concerns.

## Canonical audit stores project high-signal events to tracing

**Recorded:** 2026-05-11 02:06:39.319833Z · [T20260427-0023]

### Context
Policy denials and friction submissions reached canonical stores or return paths, but operators tailing the live feed could miss them.

### Decision
Emit structured `tracing::warn!` projections beside canonical side effects for filesystem denials, proc-spawn denials, and friction task submissions.

### Consequences
- Dashboards can watch `orbit.policy.deny` and `orbit.friction.reported` without querying canonical stores.
- Cost: the tracing feed is lossy and filterable, so missing live events cannot prove the canonical store has no matching record.

## Unified log feed: producer completion + reader CLI

**Recorded:** 2026-05-11 02:06:39.321326Z · [T20260427-27]

### Context
The unified JSONL feed still lacked job-DAG lifecycle projections, library print hygiene, and a first-class reader for the v2-terminal-console mockup.

### Decision
Add one `emit_job_event` dual-write helper for job lifecycle tracing, migrate library `println!`/`eprintln!` calls to structured tracing with clippy denies in library crates, and add `orbit log tail` with path, target, level, since, follow, and JSON options.

### Consequences
- The terminal-console mockup can use real Orbit events, and library crates fail clippy if raw prints return.
- Cost: scheduler-event semantics remain aspirational, follow mode is v1, and the reader keeps the file in memory before applying `-n`.

## Friction scorekeeping derives from lifecycle history

**Recorded:** 2026-07-26 21:51:40.343066Z · [T20260510-13], [ORB-10458]

### Context

Friction reports once used a dedicated task type, but untriaged reports shared `status: proposed` with human-authored proposals, making scoreboard derivation ambiguous.

### Decision

Add `status: friction` as the creation status for self-reports, infer legacy friction routing at creation, and rebuild `friction_bounty.json` from task history.

### Consequences


- Friction inbox items were separated from human proposals while legacy task records remained readable during migration.
- Cost: legacy untriaged reports need migration, and already-triaged legacy histories depend on existing transition records.


## Unified log feed exposes shared backend surfaces for dashboard UI

**Recorded:** 2026-05-11 02:06:39.323654Z · [T20260427-44], [T20260427-46]

### Context
`orbit log tail` established terminal semantics, but the dashboard needed the same source/code/message vocabulary without copying formatter logic into browser JavaScript.

### Decision
Extract log formatter/filter/path logic into a shared `orbit-cli` module and expose dashboard `/api/log` snapshot plus `/api/log/stream` SSE endpoints that render escaped `message_html` server-side.

### Consequences
- CLI, dashboard backend, and dashboard UI share one log vocabulary and escaping boundary.
- Cost: stream rotation/truncation handling is best-effort, and the visual panel ships separately under UI ownership.

## Tool-call provenance was model-first

**Superseded by:** [Replace \[agent.<role>\] tables with named \[crews.*\] registry](../agent-families/4_decisions.md#replace-agentrole-tables-with-named-crews-registry)
**Recorded:** 2026-05 · [ORB-00080]

**Context.** Asking agents to pass both `agent` and `model` duplicated information and allowed exact models to be paired with the wrong family.

**Decision.** Originally deprecated `agent` as a normal tool-call input and used `model` for provenance. [Replace `[agent.<role>]` tables with named `[crews.*]` registry](../agent-families/4_decisions.md#replace-agentrole-tables-with-named-crews-registry) superseded the exact-model convention: `model` now carries the canonical agent family, with full model strings accepted only as compatibility input that normalizes to family.

**Consequences.**
- Seeded skills and instructions still use a single `model` provenance field, but examples teach family values (`codex`, `claude`, `gemini`, `grok`).
- Cost: compatibility normalization must remain for historical full-model inputs and external callers that have not migrated yet.

## Task attribution can be corrected explicitly

**Recorded:** 2026-05-11 02:06:39.326347Z · [T20260427-47]

### Context
Automatic task attribution is low-friction but can leave stale `planned_by` or `implemented_by` values when different actors start and finish work.

### Decision
Keep automatic stamping for plan writes and review/done transitions, but let task update callers explicitly set or clear `planned_by` and `implemented_by`.

### Consequences
- Agents can correct split or stale provenance without editing task files directly.
- Cost: attribution fields are editable metadata, so stronger authorship evidence still requires task history and audit rows.

## Tool-invocation audit is owned by the runtime, with MCP preflight bracketing

**Recorded:** 2026-05-11 02:06:39.327530Z · [T20260428-4]

### Context
CLI `AuditGuard` historically wrote tool-invocation audit rows, leaving MCP `tools/call` dispatch and MCP preflight failures outside the SQLite command-audit trail.

### Decision
Move tool-invocation audit to `OrbitRuntime::execute_tool_command_dispatch`, tag dispatches as CLI `"run"` or MCP `"run-mcp"`, bracket MCP preflight failures in `audited_mcp_call`, and use a per-thread signal so CLI guard rows are not duplicated.

### Consequences
- CLI and MCP tool calls, including unknown/unexposed MCP failures, now produce one audit row with shared identity resolution.
- Cost: the dedup signal is thread-local; future async or cross-thread guarded entry points must re-evaluate the boundary.

## Command-audit rows carry task / run / activity correlation IDs

**Recorded:** 2026-05-11 02:06:39.328720Z · [T20260428-7]

### Context
SQLite command-audit rows recorded tool invocations but had no direct link to the task, job run, activity, or step that caused them.

### Decision
Add nullable `task_id`, `job_run_id`, `activity_id`, and `step_index` columns, populate them at runtime tool dispatch from caller JSON first and engine env vars second, index task/run ids, and render the fields in dashboard detail rows.

### Consequences
- Operators can drill from a tool row to the originating task and run context without out-of-band correlation.
- Cost: historical rows remain NULL, and caller-asserted JSON values are weaker evidence than engine-supplied env context.

## Scoreboard tool-call totals project from command audit

**Recorded:** 2026-05-11 02:06:39.329894Z · [T20260428-11]

### Context
`summary.json` used token/invocation scoreboard tool-call totals, which can be empty for providers that do not emit invocation traces, while command audit records every tool-run attempt.

### Decision
Count `command: tool` rows with `subcommand: "run"` or `"run-mcp"` and `tool_name` present as scoreboard all/failed tool-run attempts; keep token totals sourced from invocation/token scoreboards.

### Consequences
- Failed and denied tool runs become visible in compact summaries even for trace-sparse providers.
- Cost: the legacy max overlay is conservative and may undercount the true union until both streams share an invocation id.

## Task-review feedback scores separately from PR review comments

**Recorded:** 2026-05-11 02:06:39.330996Z · [T20260428-17], [T20260430-4], [T20260430-5]

### Context
Local Orbit task review threads and GitHub PR review comments are different workflow artifacts, and reply volume should not be scored as distinct review findings.

### Decision
Keep `pr.review_comments` for synced PR/GitHub comments, score local review-thread creations separately as `task-review-threads` surfaced as `task_review.threads`, do not score replies, and accept only exact configured or built-in model identities.

### Consequences
- Local review feedback earns immediate task-review credit while synced PR feedback remains a separate PR metric.
- Cost: review productivity now has two counters, and aggregate views must label them clearly rather than adding them blindly.

## Command-audit execution ids are process-disambiguated

**Recorded:** 2026-05-11 02:06:39.332153Z · [T20260505-6]

### Context
Timestamp-only command-audit execution ids collided when concurrent `orbit tool run orbit.task.show` processes in one workspace generated ids at the same effective clock tick.

### Decision
Generate command-audit execution ids through one shared helper that combines a stable prefix, wall-clock nanoseconds, process id, and a per-process atomic sequence while keeping the SQLite unique index authoritative.

### Consequences
- Parallel CLI and runtime audit producers get deterministic collision resistance without weakening uniqueness constraints.
- Cost: execution ids are longer and less visually compact than the old `exec-<nanos>` shape.

## Loop audit JSONL files materialize on first loop event

**Recorded:** 2026-05-11 02:06:39.333473Z · [T20260506-2]

### Context
V2 runs always constructed both the v2 envelope sink and the loop-level sink. Runs that emitted only envelope events or CLI-backend blobs therefore left zero-byte `.orbit/state/audit/loop/{run_id}.jsonl` files beside populated `v2_loop` files, making the audit tree look noisy and misleading.

### Decision
Keep the loop sink available for HTTP agent-loop events and blob writes, but defer creating `loop/{run_id}.jsonl` until the first `LoopAuditEvent` is emitted. Blob writes continue to use `.orbit/state/audit/blobs/` without creating an empty loop event file.

### Consequences
- Runs with no loop-level provider/tool events no longer leave empty loop JSONL placeholders.
- Cost: consumers must treat a missing loop JSONL file as "no loop events were emitted", not as a missing run; the v2 envelope file remains the canonical run spine.

## Automated git commits carry implementer authorship

**Recorded:** 2026-07-26 21:51:41.039598Z · [ORB-10369], [ORB-10458]

### Context

Task records already store `implemented_by`, but automated `git_commit` actions previously delegated commit authorship to local git config, hiding the agent that actually produced the change.

### Decision

Pass a per-commit `--author` derived from `task.implemented_by` for single-implementer commits. Mixed-implementer batch commits use `orbit <orbit@orbit.local>` as the aggregate author and add one `Co-Authored-By` trailer per distinct implementer identity. [Workflow git commit identity is process-scoped](#workflow-git-commit-identity-is-process-scoped) extends this provenance to committer identity without reusing repo-local user config.

### Consequences


- Reviewers can see implementation provenance directly in git history without joining back through run audit events.
- Local git config is not written by workflow commit automation and is no longer the source of committer identity for those commits.
- Cost: multi-implementer batch commits require trailer-aware attribution queries; `git log --author` finds the aggregate commit author, not every co-author trailer.


## Workflow git commit identity is process-scoped

**Recorded:** 2026-07-26 21:51:41.628333Z · [ORB-10369], [ORB-10458]

### Context

Reusing local Git config for workflow committers made agent identities sticky in developer repositories. If `user.name` or `user.email` was set to an agent identity in repo-local config, later human commits inherited that attribution.

### Decision

Automated `git_commit` actions set author and committer identity only for the spawned `git commit` process. Single-implementer commits use that implementer's scoped identity for both author and committer. Mixed-implementer commits use `orbit <orbit@orbit.local>` as the aggregate author and committer while preserving distinct implementers as `Co-Authored-By` trailers. Workflows must not write agent or aggregate identities into repo-local Git config.

### Consequences


- Human `user.name` and `user.email` settings remain byte-for-byte stable across workflow commits.
- Worktrees with no local `user.*` config can still create workflow-owned commits with explicit provenance.
- The public `git.commit` tool remains user-directed and ambient-config based; workflow-owned commit automation uses this scoped path instead.


## Friction reports are append-only records, not lifecycle tasks

**Superseded by:** [Keep frictions as distinct workspace-scoped records backed by SQLite](#keep-frictions-as-distinct-workspace-scoped-records-backed-by-sqlite)
**Recorded:** 2026-05-11 02:06:39.338334Z · [T20260510-13]

### Context
Friction reports are operational signal, not planned work. Storing them as task records cluttered task lists and forced accept/reject triage decisions that were more about duplicate handling than report validity.

### Decision
Store friction reports under `.orbit/frictions/{yyyy}-{mm}/F{nnn}.md` with YAML frontmatter and markdown body. Expose only `orbit.friction.*` artifact operations; exclude `friction` from the task status taxonomy and reject it during task parsing; compute rates on demand from friction records plus task completion attribution.

### Consequences
- The backlog contains work items rather than self-report signal, and friction reports remain append-only.
- The migration window is closed; task CLI, MCP, dashboard, and workflow surfaces no longer expose a friction status.
- Cost: workspaces with unmigrated legacy friction tasks must migrate them before upgrading because task deserialization no longer accepts `status: friction`.

---

## Task References

- **[T20260419-0002]** — Add workspace provenance and v2 audit envelope events for activity/job execution.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under workspace state.
- **[T20260426-0526]** — Persist v2 invocation traces for metrics beside audit.
- **[T20260426-0605]** — Add this auditability design folder and record initial ADRs.
- **[T20260426-0705]** — Expose v2 run audit events through `orbit run events` and `orbit run trace`.
- **[T20260426-0709]** — Align run step selectors on activity `step.id` and move CLI invocation log reading behind orbit-core runtime accessors.
- **[T20260426-2313]** — Stream CLI subprocess stdout/stderr through structured tracing events.
- **[T20260426-2343]** — Add the global process tracing JSONL feed at `~/.orbit/state/logs/orbit.jsonl`.
- **[T20260426-2349]** — Apply tracing-layer redaction before stderr and global JSONL output.
- **[T20260427-0023]** — Project policy denials and friction task submissions into the global tracing feed.
- **[T20260427-27]** — Close out the unified-log story: job lifecycle dual-write, library print migration with workspace lint gate, and `orbit log tail` reader CLI.
- **[T20260427-43]** — Add `status: friction`, creation-time friction routing, migration, and history-derived friction bounty refresh.
- **[T20260427-44]** — Add shared log formatter extraction and dashboard backend `/api/log` snapshot/SSE endpoints.
- **[T20260427-46]** — Implement the Gemini-owned Tasks-tab `orbit.log` panel using the shared dashboard backend API.
- **[T20260427-47]** — Allow explicit task attribution correction for `planned_by` and `implemented_by` through task update paths.
- **[T20260427-52]** — Deprecate `agent` in normal tool-call JSON, infer agent family from `model`, and reject inconsistent legacy pairs.
- **[T20260428-4]** — Record audit events for MCP tool invocations by moving ownership into the runtime, adding the entry-point discriminator, and bracketing MCP preflight.
- **[T20260428-7]** — Correlate command-audit rows with originating run/task/activity by adding nullable correlation columns and surfacing them on the dashboard.
- **[T20260428-11]** — Derive compact scoreboard all/failed tool-call counts from command-audit tool-run rows.
- **[T20260428-17]** — Split local Orbit task-review scoring from PR review-comment scoring and surface both in compact scoreboards.
- **[T20260430-4]** — Count local task-review score by review-thread creations, not replies, and rename the task-review summary field to `threads`.
- **[T20260430-5]** — Tighten task and PR review-message scoring so only exact configured orchestrator/helper model identities score; typo-prefixed labels are ignored.
- **[T20260430-20]** — Shorten the auditability docs while preserving required guarantees.
- **[T20260505-6]** — Replace timestamp-only command-audit execution ids with process-disambiguated generated ids for parallel tool runs.
- **[T20260506-2]** — Lazily materialize loop audit JSONL files only when loop-level events are emitted.
- **[T20260508-22]** — Use `task.implemented_by` to set git commit authors for automated task commits.
- **[T20260509-12]** — Scope workflow git author and committer identity to the spawned commit process without writing repo-local Git config.
- **[T20260510-13]** — Move friction reports from task lifecycle state to append-only `.orbit/frictions/` records.
- **[ORB-10202]** — Remove legacy friction from the task status taxonomy.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Keep frictions as distinct workspace-scoped records backed by SQLite

**Recorded:** 2026-08-09 19:30:11.118493Z · [ORB-10680]
**Supersedes:** [Friction reports are append-only records, not lifecycle tasks](#friction-reports-are-append-only-records-not-lifecycle-tasks)
**Paths:** `crates/orbit-store/src/driver/file/friction_store/**`, `crates/orbit-store/src/driver/sqlite/**`, `crates/orbit-core/src/adapter/tool_host/**`, `crates/orbit-web/src/api/frictions.rs`, `docs/design/auditability/**`, `docs/design/mcp-bridge/**`

### Context
[Friction reports are append-only records, not lifecycle tasks](#friction-reports-are-append-only-records-not-lifecycle-tasks) correctly separated friction reports from planned task work, but coupled that semantic decision to Markdown files under `.orbit/frictions/`. File backing was reasonable while records were low-volume, Git-visible, and directly inspectable. Frictions are now hub-only coordination state, authors and operators mutate them through Orbit surfaces, and every filtered list or stats request parses and materializes the complete retained file corpus. The real alternatives are to retain per-record Markdown for direct inspection or preserve the artifact semantics while moving live persistence to indexed SQLite.

### Decision
Keep friction as a first-class operational artifact outside the task lifecycle, with the existing `orbit.friction.*`, CLI, HTTP, dashboard, Bridge, status, tag, resolution, and task-relation semantics. Persist live friction records in the global Orbit SQLite store under composite identity `(workspace_id, friction_id)`, push filtering, ordering, pagination, and aggregation into SQL, and allocate workspace-local monthly IDs transactionally. Small tag-taxonomy configuration may remain file-backed. Direct inspection and portability are provided through supported show/list/export surfaces rather than live Markdown records.

### Consequences
- Task backlogs remain free of self-report signal; this decision does not restore a `friction` task status.
- Fixed-size friction pages decode only their result rows, so scan memory no longer grows with retained friction history.
- Identical friction IDs may safely coexist in different workspaces and every read/write remains explicitly workspace-scoped.
- Legacy Markdown records remain migration evidence for one release but cease to be a live source after a workspace import commits.
- Cost: Raw per-record file inspection and Git diffs are no longer the persistence interface; operators depend on SQLite backup/integrity tooling and Orbit export surfaces for recovery and review.

## Task References

- **[T20260419-0002]** — Add workspace provenance and v2 audit envelope events for activity/job execution.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under workspace state.
- **[T20260426-0526]** — Persist v2 invocation traces for metrics beside audit.
- **[T20260426-0605]** — Add this auditability design folder and record initial ADRs.
- **[T20260426-0705]** — Expose v2 run audit events through `orbit run events` and `orbit run trace`.
- **[T20260426-0709]** — Align run step selectors on activity `step.id` and move CLI invocation log reading behind orbit-core runtime accessors.
- **[T20260426-2313]** — Stream CLI subprocess stdout/stderr through structured tracing events.
- **[T20260426-2343]** — Add the global process tracing JSONL feed at `~/.orbit/state/logs/orbit.jsonl`.
- **[T20260426-2349]** — Apply tracing-layer redaction before stderr and global JSONL output.
- **[T20260427-0023]** — Project policy denials and friction task submissions into the global tracing feed.
- **[T20260427-27]** — Close out the unified-log story: job lifecycle dual-write, library print migration with workspace lint gate, and `orbit log tail` reader CLI.
- **[T20260427-43]** — Add `status: friction`, creation-time friction routing, migration, and history-derived friction bounty refresh.
- **[T20260427-44]** — Add shared log formatter extraction and dashboard backend `/api/log` snapshot/SSE endpoints.
- **[T20260427-46]** — Implement the Gemini-owned Tasks-tab `orbit.log` panel using the shared dashboard backend API.
- **[T20260427-47]** — Allow explicit task attribution correction for `planned_by` and `implemented_by` through task update paths.
- **[T20260427-52]** — Deprecate `agent` in normal tool-call JSON, infer agent family from `model`, and reject inconsistent legacy pairs.
- **[T20260428-4]** — Record audit events for MCP tool invocations by moving ownership into the runtime, adding the entry-point discriminator, and bracketing MCP preflight.
- **[T20260428-7]** — Correlate command-audit rows with originating run/task/activity by adding nullable correlation columns and surfacing them on the dashboard.
- **[T20260428-11]** — Derive compact scoreboard all/failed tool-call counts from command-audit tool-run rows.
- **[T20260428-17]** — Split local Orbit task-review scoring from PR review-comment scoring and surface both in compact scoreboards.
- **[T20260430-4]** — Count local task-review score by review-thread creations, not replies, and rename the task-review summary field to `threads`.
- **[T20260430-5]** — Tighten task and PR review-message scoring so only exact configured orchestrator/helper model identities score; typo-prefixed labels are ignored.
- **[T20260430-20]** — Shorten the auditability docs while preserving required guarantees.
- **[T20260505-6]** — Replace timestamp-only command-audit execution ids with process-disambiguated generated ids for parallel tool runs.
- **[T20260506-2]** — Lazily materialize loop audit JSONL files only when loop-level events are emitted.
- **[T20260508-22]** — Use `task.implemented_by` to set git commit authors for automated task commits.
- **[T20260509-12]** — Scope workflow git author and committer identity to the spawned commit process without writing repo-local Git config.
- **[T20260510-13]** — Move friction reports from task lifecycle state to append-only `.orbit/frictions/` records.
- **[ORB-10202]** — Remove legacy friction from the task status taxonomy.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Ship PR transitions preserve task implementer attribution

**Recorded:** 2026-05-17 07:05:49.092815Z · [ORB-00106]

### Context
`orbit run ship` reached PR-open Review handoff and PR-merge Done handoff through system-owned automation even when the workflow had a resolved implementer identity. Prior attribution fixes in ORB-00067, ORB-00089, and ORB-00091 covered adjacent automation paths, but the ship PR loops still had two real alternatives: trust the ship actor/runtime context, or carry task/run provenance explicitly.

### Decision
Ship-path PR transitions carry attribution on each automation update. The Review handoff uses existing `task.implemented_by`, then the pipeline resolved implementer identity, then task-authored fallback fields (`planned_by`, `created_by`), leaving the genuine actor-less fallback as `system`. The Done handoff preserves existing `implemented_by`, otherwise uses `created_by`, then `system`. Regression tests exercise PR-open review stamping and distinct Done identities in one batch so a batch-level actor cannot homogenize them.

### Consequences
- Shipped task records, ship scoreboards, and follow-on git author derivation can preserve the implementer family that actually produced each task before and after PR review.
- Actor-less automation still records `system` instead of panicking or fabricating a family label.
- Cost: the ship pipeline must explicitly bridge task/run provenance into automation update payloads, so future edits to PR-open or PR-merge loops need to preserve the regression tests rather than assuming runtime actor context is enough.

## Derive invocation cost at query time from a versioned price table

**Recorded:** 2026-07-20 04:06:48.987384Z · [ORB-10338]
**Paths:** `crates/orbit-common/src/model/pricing.rs`, `crates/orbit-common/assets/model_prices.yaml`, `crates/orbit-store/src/driver/sqlite/invocation_store/**`

**Context.** The invocation store already retains exact per-invocation token splits, but had no notion of USD cost — cost existed only as a provider-reported total buried in the worker's unparsed per-run JSON, never joined to a model or token split. [ORB-10338] adds cost. Two real alternatives existed: (a) compute cost once at ingest time and store it as a frozen column, or (b) keep rows token-only and derive cost from a versioned price table looked up by exact model string and the invocation's timestamp on every read/aggregate.

**Decision.** Cost is derived at query time, not stored. `orbit_common::model::pricing` ships a versioned price table as an in-repo YAML asset (`crates/orbit-common/assets/model_prices.yaml`), keyed by exact model string plus an `effective_from`/`effective_until` date range, parsed once behind a `OnceLock` cache. `InvocationRecord` gains `derived_cost_usd` (computed at read time from the row's token splits, model, and timestamp against the price table) alongside a new `provider_cost_usd` column that persists the provider's own reported total verbatim for monthly manual reconciliation. Adding or correcting a price row is a YAML edit, not a Rust code change.

**Consequences.**
- Historical invocation rows re-price automatically when a price row is corrected or backfilled — no migration/backfill script needed to fix a wrong rate.
- `derived_cost_usd` is `None` whenever no price row covers a model/date, so unpriced or newly-launched models degrade to "unknown" rather than a silently wrong number.
- `provider_cost_usd` never changes once written, so it stays the ground truth Daniel reconciles against monthly even if `derived_cost_usd` for the same row changes later.
- Cost: because derived cost is recomputed on every read instead of frozen at ingest, editing a price row after the fact silently changes the reported cost of every past invocation under that model/date range — there is no record of what a row's derived cost "used to be", unlike the immutable `provider_cost_usd`.

## Workflow commit authors use the persisted crew model

**Superseded by:** [Workflow alone creates shipment commits while dirty failures remain recoverable](#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable)
**Recorded:** 2026-07 · [ORB-10519]

**Context.** Pipeline-created commits exposed only a generic or family author even though the job run already persisted the exact resolved crew model used as `AGENT_MODEL` for provider subprocess commit trailers. Deriving attribution again from `task.implemented_by` or crew aliases, or letting the author and trailer read different process state, would permit the ambient author to disagree with durable model telemetry.

**Decision.** Read the persisted job-run `crew_model` once and use that same opaque string both to construct the author name `orbit (<model>)` and to set the spawned Git process's `AGENT_MODEL` for `prepare-commit-msg`. Use `agent@orbit.invalid`; do not resolve aliases, validate model strings, or add a model registry. A missing model uses the generic `orbit <orbit@orbit.local>` author. Keep the committer as the process-scoped generic Orbit identity, and adopt existing commits without amendment.

**Consequences.**
- `git log --format=%an` distinguishes pipeline commits produced by different resolved models, while the model-bearing author and `Agent-Model` trailer cannot diverge.
- Existing `Agent-Run`, `Agent-Task`, and `Co-Authored-By` trailers remain additive and unchanged.
- ORB-10365 retains a host committer because its already-created commit was adopted forward-only, while ORB-10348 was created by pipeline automation with a scoped Orbit committer.
- Cost: a bare `[crews.*].model` value remains bare in the author because configured model strings stay opaque and Orbit ships no release-coupled alias table.

---

## Workflow alone creates shipment commits while dirty failures remain recoverable

**Recorded:** 2026-07 · [ORB-10519]

The full reasoning is preserved in [Workflow alone creates shipment commits while dirty failures remain recoverable](../activity-job/4_decisions.md#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable).

---

## Provider subprocess liveness is a separate audit event probed at read time

**Recorded:** 2026-07-27 02:57:13.483354Z · [ORB-10496]
**Paths:** `crates/orbit-engine/src/activity_job/cli_runner/**`, `crates/orbit-core/src/runtime/run_audit.rs`, `crates/orbit-common/src/process/identity.rs`, `crates/orbit-web/src/api/runs.rs`

### Context

A ship-pipeline (`workflow_ship`) implementation agent is a CLI subprocess spawned by the `agent_implement` activity inside the pipeline worker. Bridge `agent_run_list` only observes the separate Worker daemon behind `agent_invoke`, so these children have no run-store row and no exposed identity: `child.id()` was used only inside `cli_runner/supervisor.rs` for process-group cleanup. During run-rescue (F2026-07-083, ORB-10257) a healthy long-running Sol/Codex agent was therefore indistinguishable from a dead child — provable only by shell process-tree inspection — which risks an operator cancelling legitimate in-flight work.

Two shapes were available for making the child observable while it runs:

(a) A heartbeat: have the supervisor periodically write a liveness timestamp (audit row or run-record column) for as long as the child is alive, and let readers compare it against now.

(b) A single spawn-time record of the PID plus its process-start identity token, with liveness probed by the reader at query time.

Extending the existing `cli.invocation.started` event was not an option: it is emitted before spawn, by construction, so no PID exists yet.

### Decision

Emit one new `cli.invocation.process` v2 audit event (`provider`, `pid`, `pid_start_time`) immediately after spawn and before the supervision loop, ordered strictly between `cli.invocation.started` and `cli.invocation.finished`. The envelope writer persists synchronously, so the row is readable while the invocation is still running.

Liveness is computed at read time, not stored. `orbit_common::process::identity::probe_process_liveness` answers `alive` / `exited` / `unknown` from `kill(pid, 0)` plus the Linux zombie check, using the recorded `pid_start_time` token to reject a recycled PID. `OrbitRuntime::collect_run_provider_processes` pairs each process event with the `cli.invocation.finished` event that closes it within the same step and probes only the still-open ones. `GET /api/runs/:id` (bridge `workflow_run_status`) and `orbit run show` project the result.

### Consequences

- A long-running `agent_implement` step is distinguishable from a lost child without shell access to the host, which is the operator decision run-rescue actually has to make.
- Retries within one step pair up in order (newest still-open record wins), so a step that respawned its provider reports each attempt separately rather than collapsing onto the first spawn.
- PID reuse cannot fake a live agent: a live PID whose versioned start-identity token disagrees reads as `exited`.
- An unprobeable host degrades to `alive`/`unknown` rather than `exited` — a probe that cannot answer must never be read as proof of death, matching the existing job-run owner-reconciliation policy.
- Cost: liveness is only as fresh as the moment it is queried and only meaningful on the host that ran the child. A remote or later reader of the same audit trail gets `exited` for every historical open invocation, because the answer is derived from the local process table rather than persisted with the event. A heartbeat would have survived that, at the price of a write per interval per invocation and a staleness threshold to tune.
- Cost: `pid_start_time` costs one `ps` invocation per provider spawn. A sandbox that blocks `ps` yields `None`, which weakens the record to unguarded-PID liveness rather than failing the spawn.

## Provider completions correlate by invocation ancestry

**Recorded:** 2026-09-05 · [ORB-11284]
**Paths:** `crates/orbit-core/src/runtime/run_audit.rs`, `crates/orbit-core/src/runtime/tests/run_audit.rs`

### Context

The original read-side reconstruction paired a `cli.invocation.finished` event with the newest unfinished provider process in its enclosing step. That preserved sequential retry ordering, but a fan-out can run two invocations with the same `step_id`. In `jrun-20260905-1721`, a completion beneath one invocation instead marked the other invocation's still-live provider PID as finished.

### Decision

Use the direct `parent_event_id` as the provider invocation identity when pairing a process event with a completion. A completion with ancestry only closes an unfinished process with the same ancestry and step; it never falls back to another same-step process. For historical records with absent ancestry, close a process only when exactly one unfinished, ancestry-free process exists in the step. Keep multiple candidates open and report their read-time liveness rather than guessing.

### Consequences

- Parallel same-step completions may arrive in either order without changing another invocation's exit or liveness fields.
- Sequential retries still pair normally: after each completion there is one matching open process under the invocation identity.
- Older sparse trails remain useful when their sole candidate is unambiguous, while ambiguous evidence honestly preserves uncertainty instead of fabricating an exit.
- CLI and API run projections share this Core read boundary, so they cannot apply competing correlation policies.

## Friction records carry an author-settable title; derivation is a structural fallback

**Recorded:** 2026-08-02 23:41:57.916629Z · [ORB-10590], [ORB-10598]
**Paths:** `crates/orbit-common/src/governance/friction/**`, `crates/orbit-store/src/driver/file/friction_store/**`

### Context

A friction record's handle was not a field. `title` existed only on the wire: the read projection derived it from the body's first non-empty line, stripped leading `#` characters, and returned the whole line. No write surface accepted a title, so no author could set one, and nothing in the tool schemas or the authoring skill said the first line was load-bearing.

A survey of a mature corpus (41 records, two agent families) found the derivation tracked authoring style rather than content. Records written as headingless prose derived a descriptive opening sentence by accident of style. Records written as structured reports — a leading section heading followed by sibling headings — derived that section label as their title, identifying nothing. A record written as one long lead paragraph derived the entire 700-character paragraph. Both failure modes are the same missing field seen from opposite ends.

The cost is measurable rather than cosmetic. Two records six days apart documented the same underlying bug, each rediagnosed from scratch. Both carried the same generic section label as their handle, so a search for prior art before filing surfaced nothing recognisable. The corpus is meant to be small and high-signal; a record whose handle does not name its subject is invisible to the person deciding whether a problem is already known.

### Decision

**1. `title` is a stored, author-settable field.** `FrictionRecord` and its frontmatter gain `title: Option<String>`. `orbit.friction.add` accepts it; `orbit.friction.update` can set or clear it, so a record can be retitled without touching its append-only body. The dashboard's create and patch bodies accept it too, giving human triage the same power as the tool surface.

**2. Derivation runs at write time and stays as the read-side fallback.** An add that supplies no title derives one and persists it, so the file itself states the handle and the next reader can see and correct it. Records written before the field existed carry no `title` and derive one on read, so the existing corpus stays readable with no migration and no body rewrites.

**3. Derivation reads structure, not vocabulary.** Two rules, both language- and author-independent:

- A leading ATX heading is the record's own title only when no later heading at its level or shallower follows it. A heading with siblings labels the first *section* of a structured report, so the subject is the prose it introduces and derivation skips the label.
- A leading `**bold**` run that opens a prose line is an inline lead-in labelling the sentence beside it, so that sentence is the subject. A line that is nothing but the bold run is itself the subject.

The result is clamped to `FRICTION_TITLE_MAX_CHARS` (120) at a word boundary. An author-supplied title is validated against the same bound and collapsed to one line; past it the write is refused rather than silently truncated, because the author can fix what a truncation would guess at.

**4. There is no `summary` field, deliberately.** A survey of the code found none: no store field, no schema parameter, no projection. What consumers call a summary is either the record's `title` or a client-side truncation of `body`. The record keeps exactly one short handle plus the full report, so `title` and `summary` are unified by construction rather than by accident.

**Rejected: a list of generic section headings to skip.** It is the shape the symptom suggests and the wrong mechanism. It encodes one language and one house style, it needs an edit every time an author invents a new label, it cannot help the overlong-paragraph failure at all, and it treats a symptom of the missing field rather than the missing field. The corpus itself refutes it as necessary: heading count alone separated every well-titled record from every badly-titled one, with no word ever consulted.

**Rejected: rejecting an add whose derived title looks non-identifying.** A write gate would force every structured-report author to pass a title explicitly, which is the correct nudge but a hard break for existing callers (the bridge MCP server, the dashboard, machine-filed triage frictions), and "looks non-identifying" is the same brittle judgement the heading list makes. The skill text asks for the title; the structural rules make the fallback usable when it is not supplied.

### Consequences

- Every friction lands with a handle that names its subject: an author's own title, or the first prose statement of the body, never a bare section label and never an unreadable paragraph.
- The existing corpus self-heals on read for the section-label and overlong cases. A record whose body genuinely never states its subject still needs a human title; `update --title` is the supported way to give it one.
- Title validation lives in one place (`orbit_common::governance::friction::title`) and both tool-host implementations — the checkout-backed host and the checkoutless hub coordination executor — call it, so the two write paths cannot drift on what a legal title is.
- Cost: the MCP tool schema for `orbit.friction.add` and `.update` gains a parameter. The addition is additive and optional — every existing call keeps working unchanged — but it is still a tool-surface change, and the snapshot guard treats schema drift as release-visible.
- Cost: two structural rules are more code than a first-line read, and they can still be wrong. A body whose first section genuinely holds the subject in its second sentence derives a weaker title than a careful author would write. The stored field is the escape hatch, which is why it is the primary mechanism and derivation only the fallback.
- Cost: the deployed `orbit` binary and the Bridge MCP server must be rebuilt before `--title` is reachable from a live surface; until then existing records cannot be retitled through the tools.

## Friction records move to SQLite with a legacy-evidence path projection

**Recorded:** 2026-08-09 20:25:57.877485Z · [ORB-10680]
**Paths:** `crates/orbit-store/src/repository/friction/**`, `crates/orbit-store/src/driver/sqlite/migration/**`, `crates/orbit-store/src/driver/file/friction_store/**`

**Context.** Friction list, filtered-query, and stats operations discovered every Markdown record under a workspace's friction tree, parsed every YAML envelope and body, allocated the complete corpus as `Vec<StoredFrictionRecord>`, and only then filtered, sorted, paginated, or aggregated. Peak memory and parse work therefore grew with total retained friction history even when a caller asked for a 50-row page or a narrow status filter. The file-backed rationale no longer matched the runtime contract: frictions are hub-only coordination state, writes go through Orbit surfaces rather than human file edits, and the canonical hub already copies checkout-local records into a global per-workspace file tree. A public `path` field pointed at the backing Markdown file, so moving persistence could not silently leave it fictitious.

**Decision.** Friction records move into the host-global store under schema migration v12 `friction_records_sqlite`, keyed by the composite `(workspace_id, friction_id)` so IDs stay workspace-local and identical IDs in two workspaces coexist (L-0072). Every list predicate — workspace, status, model, tag, date range, free text — plus the ordering, the page, and every stats aggregate is pushed into SQL, so a bounded request decodes at most the rows it asked for and `stats` decodes none. Monthly ID allocation runs inside the same write transaction as the insert, backed by a unique `(workspace_id, month, seq)` index. Each workspace's legacy tree is imported exactly once, transactionally and idempotently: a malformed record, a friction ID claimed twice in one source tree, an ID that does not address the file holding it, or a discovered/handled count mismatch aborts the transaction, and an interrupted import commits nothing. After the marker commits, SQLite is the sole live read and write source. The public `path` field is retained and now reports the legacy evidence file an imported record came from, and `null` for any record written after cutover, rather than a fabricated location; the CLI renders it as `Legacy file`. The tag taxonomy stays a small YAML configuration file — moving record persistence does not move configuration.

**Consequences.**
- Bounded scan memory and indexed workspace-local reads for the CLI, MCP, HTTP, dashboard, Bridge-facing, and scoreboard friction paths; the scoreboard consumes a per-model SQL aggregate instead of the full record slice.
- Legacy trees stay untouched, read-only rollback evidence for one release, and `export_workspace_frictions` re-materializes the live corpus in the same Markdown layout for inspection.
- No retention, deletion, cold archival, body compression, or disk-reclamation policy is introduced; removing legacy trees needs an explicit later finalize path that also drops `legacy_path`.
- Cost: a consumer that read `path` as an always-present file location now sees `null` for post-cutover records and must treat it as an optional legacy pointer.
- Rejected alternative: keeping the file store and adding a SQLite index sidecar. That would have preserved two sources of truth for the same records and left the full parse cost on every cold read and index rebuild.

## Unauthenticated MCP callers get a second, explicitly-untrusted identity field

**Recorded:** 2026-08-16 · [ORB-10890]
**Paths:** `crates/orbit-types/src/telemetry/self_reported_actor.rs`, `crates/orbit-mcp/src/adapter/dispatch.rs`, `crates/orbit-store/src/driver/sqlite/audit_event_store/**`, [specs/self-reported-actor.md](specs/self-reported-actor.md)

**Context.** 92.5% of MCP tool calls (1373 of 1485 over a 30d production window) record `role = 'unverified'` and no agent or model at all. The cause is structural, not a defective check: `ORBIT_MANAGED_RUN_CONTEXT` + `ORBIT_RUN_ID` are injected only on the engine CLI-runner spawn path, while `orbit mcp serve` is started by the MCP client from that client's own config. Managed runs drive agents over the CLI and configure no MCP for their children, so MCP is effectively the interactive surface and an interactive session cannot satisfy the managed-run trust boundary by construction. Any per-agent tool-call fail rate therefore silently excluded ~92% of that agent's MCP traffic while presenting a complete-looking denominator.

**Decision.** Add a second field rather than relaxing the boundary. `role` — and the canonical actor projection derived from it ([Canonical audit actor identity](specs/actor-identity.md), ORB-10888) — stays the trusted, authenticated identity and keeps reading exactly the bytes it read before. A new nullable `self_reported_actor` column records whatever the client claims, sourced at MCP `initialize` from `_meta.orbit.actor` falling back to `clientInfo.name`, normalized through one gate that treats a blank, over-length, or control-character-bearing claim as anonymous rather than defaulting it. The two are never merged in storage, and `get_audit_tool_call_counts_by_attribution` classifies each tool-call row into disjoint `authenticated` / `self_reported` / `anonymous` buckets so a caller reads all three denominators — and their combination — off one result set.

**Consequences.**
- Interactive MCP traffic becomes attributable and countable without becoming trusted; a published per-agent number can now state which denominator it was computed over.
- Migration v17 is additive with no backfill, so existing `unverified` rows stay valid and read as anonymous — there was no claim collected at the time to recover, and deriving one from `role` would retroactively attribute traffic Orbit never authenticated.
- Cost: two rows can name the same agent under different attributions. That is the point, not a defect to dedupe, and every rendering surface must carry the marker — `orbit audit show` prints `(unverified)`, the JSON key is `self_reported_actor` rather than `actor`, and `attribution_split` rows carry both `attribution` and `verified`.
- Rejected alternative: widening `audit_role_label_for_entry_point` to read caller-supplied JSON. That would make every published per-agent number forgeable by anyone who can reach the MCP server.
- Rejected alternative: sourcing the claim per tool call. It would read model-authored JSON — the input the trust boundary already refuses for correlation — and would let one session present a different identity on every call.

## Tool-call provenance is model-first

**Recorded:** 2026-05-11 02:06:39.325095Z · [T20260427-52]

### Context
Asking agents to pass both `agent` and `model` duplicated information and allowed exact models to be paired with the wrong family.

### Decision
Deprecate `agent` as a normal tool-call input, prefer exact `model`, infer the agent family from known model names, and reject inconsistent legacy pairs.

### Consequences
- Seeded skills and instructions can use shorter model-only tool calls while task records still retain both fields internally.
- Cost: unknown or ambiguous models still need a compatible legacy `agent` value when family-specific dispatch matters.

## Task References

- **[T20260419-0002]** — Add workspace provenance and v2 audit envelope events for activity/job execution.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under workspace state.
- **[T20260426-0526]** — Persist v2 invocation traces for metrics beside audit.
- **[ORB-00190]** — Retire the metrics CLI and make dashboard endpoints canonical for invocation metrics.
- **[T20260426-0605]** — Add this auditability design folder and record initial ADRs.
- **[T20260426-0705]** — Expose v2 run audit events through `orbit run events` and `orbit run trace`.
- **[T20260426-0709]** — Align run step selectors on activity `step.id` and move CLI invocation log reading behind orbit-core runtime accessors.
- **[T20260426-2313]** — Stream CLI subprocess stdout/stderr through structured tracing events.
- **[T20260426-2343]** — Add the global process tracing JSONL feed at `~/.orbit/state/logs/orbit.jsonl`.
- **[T20260426-2349]** — Apply tracing-layer redaction before stderr and global JSONL output.
- **[T20260427-0023]** — Project policy denials and friction task submissions into the global tracing feed.
- **[T20260427-27]** — Close out the unified-log story: job lifecycle dual-write, library print migration with workspace lint gate, and `orbit log tail` reader CLI.
- **[T20260427-43]** — Add `status: friction`, creation-time friction routing, migration, and history-derived friction bounty refresh.
- **[T20260427-44]** — Add shared log formatter extraction and dashboard backend `/api/log` snapshot/SSE endpoints.
- **[T20260427-46]** — Implement the Gemini-owned Tasks-tab `orbit.log` panel using the shared dashboard backend API.
- **[T20260427-47]** — Allow explicit task attribution correction for `planned_by` and `implemented_by` through task update paths.
- **[T20260427-52]** — Deprecate `agent` in normal tool-call JSON, infer agent family from `model`, and reject inconsistent legacy pairs.
- **[T20260428-4]** — Record audit events for MCP tool invocations by moving ownership into the runtime, adding the entry-point discriminator, and bracketing MCP preflight.
- **[T20260428-7]** — Correlate command-audit rows with originating run/task/activity by adding nullable correlation columns and surfacing them on the dashboard.
- **[ORB-10228]** — Supersede [Command-audit rows carry task / run / activity correlation IDs](#command-audit-rows-carry-task-run-activity-correlation-ids) caller-JSON precedence for MCP; add trusted caller/process provenance, capability sets, and call/lease correlation.
- **[T20260428-11]** — Derive compact scoreboard all/failed tool-call counts from command-audit tool-run rows.
- **[T20260428-17]** — Split local Orbit task-review scoring from PR review-comment scoring and surface both in compact scoreboards.
- **[T20260430-4]** — Count local task-review score by review-thread creations, not replies, and rename the task-review summary field to `threads`.
- **[T20260430-5]** — Tighten task and PR review-message scoring so only exact configured orchestrator/helper model identities score; typo-prefixed labels are ignored.
- **[T20260430-20]** — Shorten the auditability docs while preserving required guarantees.
- **[T20260505-6]** — Replace timestamp-only command-audit execution ids with process-disambiguated generated ids for parallel tool runs.
- **[T20260506-2]** — Lazily materialize loop audit JSONL files only when loop-level events are emitted.
- **[T20260508-22]** — Use `task.implemented_by` to set git commit authors for automated task commits.
- **[T20260509-12]** — Scope workflow git author and committer identity to the spawned commit process without writing repo-local Git config.
- **[T20260510-13]** — Move friction reports from task lifecycle state to append-only `.orbit/frictions/` records.
- **[ORB-00067]** — Earlier automation attribution work that did not close the ship batch PR Done transition gap.
- **[ORB-00089]** — Earlier system-attribution gap that informed the ship-path fallback rule.
- **[ORB-00091]** — Prior fix for automation-driven status attribution that did not cover the ship merge loop.
- **[ORB-00080]** — Collapse Orbit agent identity to family and isolate exact model strings to invocation/configuration surfaces.
- **[ORB-00090]** — Align agent-facing docs and tool descriptions with the family-as-identity convention.
- **[ORB-00106]** — Preserve per-task implementer attribution when `orbit run ship` moves batch PR tasks from Review to Done.
- **[ORB-10202]** — Remove the retired friction task status and consolidate task mutation attribution and record-parameter construction.
- **[ORB-10338]** — Add the versioned model price table and query-time `derived_cost_usd`, plus a persisted `provider_cost_usd` column for reconciliation.
- **[ORB-10370]** — Fill provider model/cost trace fields from CLI result JSON and prefer reported model identity at invocation ingest.
- **[ORB-10579]** — Correct GPT-5.6 price periods, cache-write rates, gross-input accounting, and standard short-context estimate boundaries.
- **[ORB-10519]** — Keep the persisted crew-model author and process-scoped Orbit committer while removing hook-specific trailer input and provider-commit adoption ([Workflow alone creates shipment commits while dirty failures remain recoverable](#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable), superseding [Workflow commit authors use the persisted crew model](#workflow-commit-authors-use-the-persisted-crew-model) and [Preserve failed worktree state before cleanup and admit only proven task commits](../activity-job/4_decisions.md#preserve-failed-worktree-state-before-cleanup-and-admit-only-proven-task-commits)).
- **[ORB-10369]** — Introduce the persisted resolved crew model as the pipeline commit author with generic fallback and no alias resolver ([Workflow commit authors use the persisted crew model](#workflow-commit-authors-use-the-persisted-crew-model), superseded by [Workflow alone creates shipment commits while dirty failures remain recoverable](#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable)).
- **[ORB-10496]** — Record the spawned provider subprocess PID as its own audit event and expose read-time liveness through run status and `orbit run show`.

- **[ORB-10590]** — Make the friction record handle an author-settable field and derive it structurally when omitted ([Friction records carry an author-settable title; derivation is a structural fallback](#friction-records-carry-an-author-settable-title-derivation-is-a-structural-fallback)).
- **[ORB-10680]** — Moved hub friction records into the host-global SQLite store to bound scan memory ([Friction records move to SQLite with a legacy-evidence path projection](#friction-records-move-to-sqlite-with-a-legacy-evidence-path-projection)).
- **[ORB-10890]** — Record an explicitly-untrusted self-reported actor beside the trusted `role` so unauthenticated MCP traffic is attributable-but-labelled ([Unauthenticated MCP callers get a second, explicitly-untrusted identity field](#unauthenticated-mcp-callers-get-a-second-explicitly-untrusted-identity-field)).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
