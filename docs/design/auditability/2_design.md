---
summary: "Auditability — Design"
type: design
title: "Auditability — Design"
owner: codex
last_updated: 2026-08-30
last_validated: 2026-08-30
status: Draft
feature: auditability
doc_role: design
tags: ["auditability"]
---

# Auditability — Design

This document describes Orbit's shipped auditability implementation across command audit rows, activity/job envelopes, loop-level provider/tool traces, blob storage, redaction, identity attribution, metrics-adjacent invocation records, and known limitations. See [1_overview.md](./1_overview.md) for the feature purpose and [3_vision.md](./3_vision.md) for future questions.

---

## 1. Storage Roots and Audit Channels

Auditability is split across four channels:

1. **Command audit records.** SQLite rows in the configured audit database; queried through `orbit audit`.
2. **V2 activity/job and loop events.** SQLite rows in `v2_audit_events`, with loop rows created only when a run emits loop events; redacted content-addressed blobs remain under `.orbit/state/audit/blobs/`.
3. **Global tracing events.** Redacted JSONL under `~/.orbit/state/logs/orbit.jsonl`.
4. **Invocation metrics.** SQLite records keyed by job run, activity, task, agent, model, usage, and tool-call summaries.

The split is deliberate: command rows stay compact and queryable; envelopes preserve workflow structure; loop audit preserves provider/tool detail; tracing gives operators a live feed before workspace context exists; metrics answer cost and scoreboard questions without scraping transcripts. [T20260426-0519] moved file-backed run traces under `.orbit/state/audit/` while command audit rows remained in SQLite.

---

## 2. Command Audit Rows

`AuditEvent` lives in `crates/orbit-common/src/types/audit_event.rs`. Rows include execution id, timestamp, command/subcommand, optional tool and target metadata, role, status, exit code, duration, working directory, optional argument/error/stdout/stderr fields, host, pid, and session id. Nullable MCP additions include resolved workspace, caller/process machine and display-host fields, transport, origin-session id, per-call `trace_id`, and best-effort `caller_ip`. Caller machine and IP are audit metadata, not authenticated identity. Capability, `mcp_call_id`, and lease columns remain schema-compatible but current v1 MCP sessions leave them empty. Old rows read with null/empty additions and are not rewritten.

After [T20260505-6], command-audit producers use the shared `audit_execution_id` helper instead of timestamp-only ids. The id keeps a stable producer prefix and appends wall-clock nanoseconds, process id, and a per-process atomic sequence so same-workspace parallel `orbit tool run ...` calls do not collide on clocks with coarse effective resolution. The SQLite unique index on `execution_id` remains the enforcement boundary.

The CLI RAII guard in `crates/orbit-cli/src/audit_middleware.rs` defaults to failure, marks success or denial explicitly, and writes one row in `Drop`, so early returns still audit when stack unwinding reaches the guard. Direct `orbit audit ...` commands are outside the guard today to avoid recursive audit noise.

[ORB-10200] moved command and subcommand metadata selection out of the middleware into the exhaustive `Commands::operation` registry in `crates/orbit-cli/src/command/operation.rs`. The same operation declaration now owns dispatch, runtime bootstrap policy, audit metadata, JSON error preference, and hook error suppression, so a new top-level command cannot compile until all five concerns are declared; `audit_middleware.rs` owns only audit persistence.

For `orbit tool run`, [T20260427-52] first collapsed duplicate `agent` + `model` inputs. [ORB-00080] later made the family the durable identity: agent-facing `model` inputs should be `codex`, `claude`, `gemini`, or `grok`, while full model strings remain accepted for compatibility and normalize to the family before persistence. [ORB-10451] applies that same trust boundary to runtime bootstrap: CLI and `orbit-web` processes canonicalize `ORBIT_AGENT_NAME` / `ORBIT_AGENT_MODEL` to an agent family, and an absent or inconsistent envelope records `unknown` instead of asserting verified human presence. This changes attribution only; actor-based command gating remains separate work.

After [T20260428-4], tool-invocation audit is written at Core's dispatch boundary for
registered CLI/MCP tools. The current implementation is
`crates/orbit-core/src/command/tool/dispatch.rs`: a `ToolEntryPoint` becomes
`subcommand: "run"` or `"run-mcp"`, setup and handler failures inside the boundary are
audited, and `duration_ms` is at least `1`. The CLI RAII guard covers top-level command
execution and suppresses its duplicate after Core records a tool row; pre-runtime CLI
failures such as invalid JSON still produce the guard-side row. A successful tool result is
not returned if audit persistence fails, although the audit seam cannot roll back a
mutation already committed. When the tool itself fails, that domain error remains primary.

For MCP, initialize metadata controls only the external workspace address selector. The
server constructs the rest of `ToolSessionContext`: accepting-process identity,
`local`/`ssh-mcp` transport, an origin-session id, and one new `trace_id` per call. The SSH
proxy's caller machine label and the server-observed caller IP are opaque audit correlation.
Only a managed process environment contributes task/run/activity/step correlation; model
tool JSON does not. Outside that envelope the role is `unverified`. V1 performs no MCP
authorization and records no capability grants or leases.

Registered global calls, resolved workspace calls, and workspace setup failures enter a
Core audit seam. Every `tools/call`, including an unknown or unadvertised raw name, crosses
the global seam and records one denied row when dispatch rejects it. Legacy `host` remains
the executing-process hostname, while caller/process fields are additive.

---

## 3. Tool-Driven and Runtime Audit Records

Some runtime paths write targeted command-audit rows directly:

- `crates/orbit-core/src/command/tool/dispatch.rs` records runtime-backed, in-process, and global CLI/MCP tool invocations as `command: tool` with `subcommand: "run"` or `"run-mcp"`.
- `crates/orbit-cli/src/command/mcp/server.rs` composes `orbit-mcp` framing with `orbit-cmd` registered runtime selection. Global discovery, unknown/unadvertised names, and workspace setup failures use Core's global audit seam; resolved workspace calls use the runtime seam. The server does not make capability, placement, or authorization decisions in v1.
- `crates/orbit-core/src/runtime/orbit_tool_host/mod.rs` records task lock reservation checks, reservations, releases, and denials.
- `crates/orbit-core/src/runtime/v2_host/pipeline_actions.rs` records gate-starvation failures for task bundles.

These producers share the SQLite schema and must preserve the same status, target, actor, and redaction expectations as CLI rows. Prescriptive coverage expectations live in [specs/coverage-matrix.md](./specs/coverage-matrix.md).

[ORB-10888] added a canonical actor projection beside `role`: nullable `actor_kind`, `actor_id`, `actor_vendor`, `actor_family`, `actor_model`, and `actor_alias_version` columns, backfilled for existing rows by migration v16. `role` itself is untouched, so trust classification is unchanged; the projection exists so aggregates can group one agent recorded at family, shorthand, and full-model granularity as a single actor, and can tell synthetic (`admin`, `hook`) and unattributed (`unknown`, `unverified`) rows from real agents without string-matching the label. The alias map and its versioning rules live in [specs/actor-identity.md](./specs/actor-identity.md).

After [T20260427-0023], selected canonical stores also project live tracing events: filesystem policy denials still write FS audit events, proc-spawn allowlist denials still return `OrbitError::PolicyDenied`, and each path also emits a redacted `orbit.policy.deny` event. Friction reports are workspace-scoped records in host-global SQLite, not task lifecycle events or precomputed scoreboard updates; the old `.orbit/frictions/` tree is retained only as import/rollback evidence, and `orbit-web` owns the dashboard/API triage surface.

---

## 4. Activity/Job Envelope Events

`V2AuditEnvelope` lives in `crates/orbit-common/src/types/activity_job/audit_envelope.rs`. Each envelope carries `schemaVersion`, `event_type`, `event_id`, timestamp, `run_id`, `agent_identity`, optional `parent_event_id`, optional `workspace_path`, and a tagged `V2AuditEventKind`. Event families cover run, step, retry, skip, denial, join, fan-out/fan-in, loop, activity, filesystem, tool denial, CLI-backend delegation, and subprocess lifecycle. After [T20260508-8], `CliInvocationStarted` also records the resolved subprocess `cwd` when one is supplied by the Activity/Job workspace resolver. [ORB-11069] extends `ToolAllowlistHarnessDelegated` with the selected task ids, their requested `required_tools`, and the computed effective baseline-plus-task list; its compatibility `tools` field is the same effective list. The requested list is immutable creation-time task authority, so the audit event reflects the same persisted issuance-time list that dispatch consumes. This evidence is emitted before provider launch, while the CLI stdin envelope records requested and effective lists separately and `ORBIT_ACTIVITY_TOOLS` exports the effective list.

`V2AuditWriter` in `crates/orbit-engine/src/activity_job/audit_writer.rs` assigns event ids, maintains per-thread parent stacks, emits through `V2SqliteSink` in `crates/orbit-engine/src/activity_job/sqlite_sink.rs`, keeps a smoke-verification snapshot, and exposes the inner loop sink for provider/tool events. CLI-launched v2 runs stamp envelope `agent_identity` as `system`; concrete agent identity lives in activity configuration, CLI invocation events, and invocation metrics.

`V2SqliteSink` stores one serialized event payload per row in `v2_audit_events`. `crates/orbit-core/src/runtime/run_audit.rs` is the read-side accessor after [T20260426-0709], deriving activity DAG `step.id` values from `parent_event_id` ancestry and resolving CLI stdout/stderr blob references for `orbit run logs`. After [T20260508-14], the same accessor tolerates malformed stored event JSON and missing blobs for dashboard inspection, returning partial per-step CLI invocation records with run id, event id, timestamp, step index, exit status, timeout, duration, provider, blob refs, and bounded stdout/stderr material.

After [ORB-10496] (see [Provider subprocess liveness is a separate audit event probed at read time](./4_decisions.md#provider-subprocess-liveness-is-a-separate-audit-event-probed-at-read-time)), the CLI backend also emits `CliInvocationProcess` (`cli.invocation.process`) carrying the spawned provider child's `pid` and its `pid_start_time` identity token, immediately after spawn and before the supervision loop — ordered strictly between the started and finished events, and persisted synchronously so it is readable while the invocation is still running. `OrbitRuntime::collect_run_provider_processes` pairs a process and `cli.invocation.finished` event by their shared direct `parent_event_id` invocation identity as well as their enclosing step ([ORB-11284](./4_decisions.md#provider-completions-correlate-by-invocation-ancestry)). This preserves sequential retries while keeping same-step parallel invocations distinct even when completions arrive out of order. Historical events without ancestry pair only when exactly one ancestry-free process is open; ambiguity remains open and receives a liveness probe. The accessor resolves liveness for still-open records through `orbit_common::utility::process_identity::probe_process_liveness`, which reports `alive` / `exited` / `unknown` and treats a live PID with a disagreeing versioned identity token as `exited`. `GET /api/runs/:id` returns the projection as `provider_processes`, and `orbit run show` prints one `Agent:` line per still-open child. This is the only channel that observes ship-pipeline `agent_implement` agents: they are children of the pipeline worker, so the Worker-daemon run store behind bridge `agent_run_list` never records them.

---

## 5. Loop-Level Provider and Tool Events

`LoopAuditEvent` in `crates/orbit-agent/src/loop_engine/audit/mod.rs` covers session spawn/close, HTTP request/response, tool-call request/result, iteration boundary, and policy denial. `V2SqliteSink` persists loop events lazily into `v2_audit_events` and writes payload blobs to `{audit_root}/blobs/`; runtime callers pass `.orbit/state/audit` as `audit_root`. [T20260506-2] removed the old zero-byte loop JSONL placeholders for runs that only emit v2 envelope events or CLI-backend blobs.

Loop events reference hashes for request bodies, response bodies, tool inputs, and tool outputs instead of embedding the bodies inline. This keeps event rows queryable while preserving replay material in redacted blob storage.

---

## 6. Blob Storage and Redaction

`crates/orbit-common/src/utility/blob_store.rs` writes content-addressed blobs under `{root}/{hash_prefix}/{hash}`. The hash is computed after redaction, and existing blob paths are reused.

`crates/orbit-common/src/utility/redaction.rs` centralizes sensitive live environment value scrubbing plus regex-based HTTP/argv/SSH patterns for authorization headers, API keys, bearer tokens, JSON API-key fields, high-confidence provider token shapes, SSH public-key fingerprints and comments, and hosts in canonical OpenSSH diagnostic sentences. CLI audit errors, blob bytes, selected pipeline outputs/errors, artifact write tools, and the default tracing subscriber all redact before persistence or terminal/JSONL output. Artifact tool coverage, the current pattern-family inventory, and refuse-vs-mask rules live in [specs/artifact-redaction.md](./specs/artifact-redaction.md). The smoke example `crates/orbit-agent/examples/redaction_smoke.rs` verifies stored blob bytes omit the raw secret and contain a marker. [ORB-10591]

Dashboard log previews added by [T20260508-14] are derived views over the `v2_audit_events` SQLite store and `.orbit/state/audit/blobs`; they do not duplicate full transcripts into a separate transcript store. Preview responses are byte- and line-capped, apply defensive read-time redaction with the shared redactor, and preserve existing write-time redaction markers. The focused diagnostics error feed is also derived, combining global ERROR tracing rows with structured `ERROR <target>:` lines found in agent stderr blobs. No `.orbit/state/diagnostics/errors/` store exists in this design; retention remains bounded by the existing v2 audit, blob, and global log retention roots.

---

## 7. Identity and Attribution

Orbit currently carries identity through related fields rather than one universal key:

- Direct CLI commands and `orbit-web` runtime construction share the env-derived `agent family` / `unknown` actor rule. MCP is `unverified` outside a managed process envelope; when that envelope is present, its process environment supplies audit identity and correlation rather than caller JSON.
- `V2AuditEnvelope.agent_identity` records the workflow-envelope actor. CLI-launched v2 runs use `system`; concrete provider activity appears in event bodies and metrics.
- Task records carry `created_by`, `planned_by`, `implemented_by`, `agent`, and `model`.
- Invocation metrics record agent family and configured runtime model beside job run and activity ids.

Task attribution remains automatic by default: non-empty plan writes stamp `planned_by`, and transitions into `review` or `done` stamp `implemented_by`. After [T20260427-47], `orbit.task.update` and direct `orbit task update` can explicitly set or clear those fields; explicit values win within the same update.

For `orbit run ship`, the PR-open handoff preserves implementation provenance when tasks move from In Progress to Review, and the PR-merge handoff preserves it again when tasks move from Review to Done. The Review handoff resolves attribution from existing `task.implemented_by`, then the pipeline's resolved implementer identity, then task-authored fallback fields (`planned_by`, `created_by`) before leaving genuinely actor-less automation as `system`. The Done handoff preserves existing `implemented_by` and otherwise falls back to `created_by` before `system`. This keeps mixed-family batches from collapsing to one ship actor while retaining the legitimate system fallback. [ORB-00106]

After [ORB-10519] / [Workflow alone creates shipment commits while dirty failures remain recoverable](./4_decisions.md#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable), pipeline-created `git_commit` automation reads the persisted job-run `crew_model` once and uses that opaque value for the visible author name (`orbit[<model>]`) with the non-routable `agent@orbit.invalid` address. A missing model falls back to `orbit <orbit@orbit.local>` without aborting the commit. The committer remains the process-scoped generic Orbit identity; repository `git config user.name` and `user.email` are neither required nor changed. Orbit exports no hook-specific model input, requires no `prepare-commit-msg` injector or `Agent-*` trailers, and does not adopt commits created by providers. Durable task and run records remain the workflow-provenance authority. This preserves the compatible attribution boundaries in [Automated git commits carry implementer authorship](./4_decisions.md#automated-git-commits-carry-implementer-authorship) and [Workflow git commit identity is process-scoped](./4_decisions.md#workflow-git-commit-identity-is-process-scoped) while superseding [Workflow commit authors use the persisted crew model](./4_decisions.md#workflow-commit-authors-use-the-persisted-crew-model)'s hook and adoption clauses.

The requirement is not to collapse every field into one value. It is that a reviewer can follow task state, command rows, run envelopes, provider/tool traces, and metrics back to a concrete human or agent family. A unified identity glossary and query join story remain open.

---

## 8. Query, Export, and Metrics Surfaces

The audit CLI exposes command rows through `orbit audit list`, `show`, `stats`, `export --format json`, `export --format csv`, and `prune`. Additive fields and filters cover workspace, caller/process machine, transport, origin session, canonical run, and compatibility capability/call/lease columns. JSON, CSV, show output, and `orbit-web` projections preserve the nullable schema. Current MCP rows additionally expose a per-call `trace_id` and optional observed `caller_ip`; capability, `mcp_call_id`, and lease filters normally match nothing because v1 does not populate those fields.

V2 traces are exposed separately: `orbit run events` prints chronological envelopes, `orbit run trace` renders the parent tree, and `orbit run logs` extracts CLI stdout/stderr blobs. `orbit run history` and `orbit run show` expose job-run state rather than the full envelope stream. Metrics and scoreboard commands read invocation records; they summarize cost and usage, not transcript structure.

After [ORB-10337], `POST /api/metrics/invocations` accepts the existing `InvocationInsertParams` shape and writes directly to the invocation store, with no additional schema. Worker bridges use the worker run id as `job_run_id`, pass every task id from run coupling, and preserve the provider's exact model string and input/cache-read/cache-create/output token splits. Each post creates one invocation row, so multiple worker runs coupled to one task remain independently queryable and contribute separately to task and agent aggregates. Like every dashboard mutation, ingestion requires an `Origin` header for `http://localhost` or `http://127.0.0.1`.

After [ORB-10338] (see [Derive invocation cost at query time from a versioned price table](./4_decisions.md#derive-invocation-cost-at-query-time-from-a-versioned-price-table)), `InvocationInsertParams.trace` carries an optional `provider_cost_usd` — the provider's own reported total, persisted verbatim in a new `invocations.provider_cost_usd` column for Daniel's monthly manual reconciliation. `InvocationRecord` also exposes `derived_cost_usd`, computed at read time (not stored) by `orbit_common::types::pricing::derive_cost_usd` from the row's model, timestamp, and token splits against a versioned price table shipped as `crates/orbit-common/assets/model_prices.yaml` — rows keyed by exact model string plus an `effective_from`/`effective_until` range, loaded once behind a `OnceLock`. Adding or correcting a rate is a YAML edit; no Rust change and no backfill are needed, and existing rows re-price automatically the next time they are read. `derived_cost_usd` is `None` whenever no price row covers the model/date, and it never overwrites `provider_cost_usd`.

After [ORB-10579], each price row also declares whether its input count is exclusive or gross-with-cache. Existing rows default to exclusive accounting; GPT-5.6 rows use gross accounting, so derived pricing subtracts cached-read and both cache-write buckets from the gross input total before charging the full input rate. Checked subtraction makes inconsistent rows unknown instead of silently saturating. Codex JSONL and OpenAI-compatible response parsing retain provider-reported cached-read, standard cache-write, and output buckets; OpenAI reports no 1-hour write bucket, while a malformed stored nonzero count still has a nonzero fallback price. GPT-5.6 results are standard short-context API-equivalent derived estimates. Exact Fast/service-tier and long-context billing remains future work because Orbit does not yet retain those per-request dimensions.

After [ORB-10581] / [Attribute managed execution cost to an explicit task orchestrator](../task-artifacts/4_decisions.md#attribute-managed-execution-cost-to-an-explicit-task-orchestrator), `GET /api/metrics/orchestrators?since=&until=` reports managed-execution accounting from one unbounded, half-open invocation-fact read captured at an `as_of` timestamp. Each invocation's distinct linked task ids resolve against canonical tasks and enter exactly one conservative bucket with precedence `missing task > unattributed task > one named orchestrator > shared named orchestrators`; duplicate links and multiple tasks owned by the same orchestrator do not multiply cost. Buckets retain all five token splits and separate provider, derived, and same-population comparable cost sums, counts, and delta. Missing provider values and unpriced/invalid derived values remain explicit counts, so partial sums are never presented as reconciled totals. Direct Codex/Claude orchestration sessions that do not emit managed invocation rows remain outside this endpoint.

After [ORB-10582], the canonical dashboard scoreboard embeds that runtime projection as a top-level, independently versioned `orchestration` section (scoreboard schema v8, orchestration schema v1). It obtains the same selected bounded window from the runtime rather than reusing all-time token snapshots; its `until` is exclusive and is no later than the recorded `as_of`. This panel is deliberately outside executor-agent/model rankings: named orchestrator, shared ownership, unattributed ownership, and missing-task buckets describe canonical task ownership, not the identity of the execution agent. It labels provider-reported totals, derived estimates, same-population comparable totals and delta, and unavailable populations separately. Provider and derived partial sums are never displayed as reconciled unless the explicit comparable count says they share the same invocation population. The scope remains managed execution only; direct interactive Codex or Claude orchestration-session overhead is not included.

The workspace-local `model-price-audit` auto-task ([ORB-10583]) is the evidence
collection and reconciliation guard around this table. Once weekly, it
enumerates exact model strings from `InvocationRecord.model` telemetry and every
currently priced row, then records authoritative provider pricing, model, and
caching sources with URLs, retrieval timestamps, rates, units, tiers, and
effective boundaries. It is report-only: it never edits pricing. A verified
material discrepancy may create at most one deduplicated proposed remediation
task for human review; unavailable, contradictory, or ambiguous official
evidence creates no remediation task. Any recommendation must preserve
historical rows and use non-overlapping versioned periods, stating uncertainty
when the exact effective cutoff is not supported. Standard short-context rates
remain separate from Fast/service-tier and long-context dimensions, which are
not approximated into the base table. Direct Codex/Claude orchestration-session
cost is outside the audit scope.

After [ORB-10370], CLI response parsing also fills `InvocationTrace.provider_model` and `provider_cost_usd` directly from provider-owned result metadata. Claude exposes a `modelUsage` map and `total_cost_usd`; when Claude reports its internal helper model beside the requested model, Orbit selects the unique highest-cost map entry and preserves its key verbatim. Gemini exposes an exact model key under `stats.models` but no invocation USD total; Orbit accepts it only when the map has one entry. Codex JSONL does not currently report either value, so those fields remain `None`. Grok Build CLI JSON (`grok` 1.0.5+) carries the same `modelUsage` / `total_cost_usd` wrapper fields as Claude. The ledger's only key is the requested public menu id with a `-build` suffix (`grok models` lists `grok-4.6` and `grok-4.5`; the usage map key is `grok-4.6-build` / `grok-4.5-build`). Parser extraction keeps that ledger key verbatim. Wrappers that omit both fields still leave `provider_model` and `provider_cost_usd` unset. At SQLite ingest, a non-empty provider model wins over the configured request/alias and the configured model remains the fallback, except Grok Build's `{public-id}-build` ledger name is canonicalized to the requested public menu id so `invocations.model` matches the shipped price row (`grok-4.6`, not `grok-4.6-build`). A disagreement emits a retained `WARN` event under `orbit.core.invocation` with job run, activity, CLI, requested model, and provider model fields. The Grok `{id}` vs `{id}-build` pair is not disagreement; requested `grok-4.6` vs ledger `grok-4.5-build` still warns. [ORB-10970] This structured mismatch event was chosen instead of a second invocation column: it makes provider routing drift detectable under the default logging filter without migrating or backfilling rows, while `invocations.model` remains the exact model used for pricing and aggregation.

The local dashboard exposes two read-only API surfaces for these traces after [T20260508-14]: `GET /api/runs/:id/logs` returns bounded per-step CLI invocation previews, and `GET /api/diagnostics/errors` returns recent process ERROR rows plus structured agent-stderr error rows sorted newest first. Both endpoints use existing dashboard limit conventions and tolerate missing stored event rows, malformed event JSON, and missing blobs by returning empty or partial arrays.

After [T20260428-11], compact `summary.json` counts all audited tool-run attempts and failed attempts from command-audit rows where `command: tool`, `subcommand` is `"run"` or `"run-mcp"`, and `tool_name` is present. Token totals still come from invocation/token scoreboards, with legacy tool-call totals used only as a max overlay to avoid obvious double counting.

After [T20260428-17] and [T20260430-4], local task review and GitHub PR review are separate scoreboard inputs. Local review-thread creations record `task-review-threads` in `task_review.json`; successful GitHub sync records `pr-review-comments` in `pr.json`. `summary.json` schema version 2 exposes these as `task_review.threads` and `pr.review_comments`, and scoring accepts only exact configured model identities or built-in defaults, skipping `human`, `system`, and arbitrary bare labels.

After [T20260510-13] and [ORB-00062], friction reporting is outside the task lifecycle: `orbit.friction.add` writes markdown records under `.orbit/frictions/`; `orbit.friction.list/show/tags/update/resolve` expose scan and triage helpers; and `orbit.friction.stats` computes `open`, `triaged`, `resolved_this_month`, total resolved count, and model/tag rates on demand from that corpus plus task completion attribution. After [ORB-10202], `friction` is no longer a `TaskStatus` variant or accepted persisted task value. The dashboard `Knowledge > Frictions` subtab delegates to the same tool helpers through `/api/frictions*`, so human triage and CLI/MCP reads share one vocabulary and stats shape.

After [ORB-10680], friction records live in the host-global store rather than the Markdown tree, keyed by the composite `(workspace_id, friction_id)`: IDs stay workspace-local and monthly, and the same `F2026-05-001` in two workspaces is two unrelated records. `orbit.friction.list` pushes workspace, status, model, tag, date-range, and free-text predicates plus the ordering and the `limit`/`offset` page into SQL, so a bounded page decodes only the rows it asked for; `orbit.friction.stats` is a set of `GROUP BY` aggregates and decodes no bodies at all. Monthly ID allocation is transactional, backed by a unique `(workspace_id, month, seq)` index. Each workspace's legacy tree is imported once through schema migration v12 `friction_records_sqlite` — transactional, idempotent, and fail-closed on a malformed record, an ID claimed twice in one tree, an ID that does not address the file holding it, or a count mismatch — after which SQLite is the sole live source. Legacy trees stay as untouched read-only rollback evidence for one release, `orbit_store::friction_store::export_workspace_frictions` re-materializes the live corpus in the same layout for inspection, and the tag taxonomy stays the `tags.yaml` configuration file it always was. The public `path` field now reports the legacy evidence file of an imported record and `null` for anything written after cutover; [Friction records move to SQLite with a legacy-evidence path projection](./4_decisions.md#friction-records-move-to-sqlite-with-a-legacy-evidence-path-projection) records that disposition and the rejected index-sidecar alternative.

After [ORB-10590], a record carries an author-settable `title` — its handle in `friction list`, the dashboard, and the pre-filing search that decides whether a problem is already known. `orbit.friction.add` accepts it and `orbit.friction.update` can retitle a record without touching its append-only body. Callers that supply none get one derived from the body at write time, so the frontmatter always states the handle rather than leaving every reader to re-derive it; records written before the field existed carry no `title` and derive one on read instead, which is why no migration pass is owed. Derivation is structural (`orbit_common::friction::title`): a leading heading is the record's own title only when no sibling heading at its level or shallower follows it, a leading bold run is an inline lead-in whose sentence is the subject, and the result is clamped to `FRICTION_TITLE_MAX_CHARS`. There is no separate `summary` field — [Friction records carry an author-settable title; derivation is a structural fallback](./4_decisions.md#friction-records-carry-an-author-settable-title-derivation-is-a-structural-fallback) records why the record keeps exactly one short handle.

---

## 9. Global Process Tracing JSONL

`crates/orbit-common/src/utility/logging.rs` installs a default subscriber with one `EnvFilter`, stderr formatting, and an optional non-blocking JSONL file layer at `~/.orbit/state/logs/orbit.jsonl` after [T20260426-2343]. The retained `WorkerGuard` lets routine event emission avoid synchronous disk writes.

Each record contains timestamp, level, target, and structured fields. After [T20260426-2349], both stderr and JSONL use `RedactingFields`, which scrubs string values, `Debug`-formatted values, and unstructured messages while preserving numeric and boolean JSON types. This global feed is the live landing zone for subprocess output [T20260426-2313], policy-denial and friction projections [T20260427-0023], and other `tracing` events emitted before workspace runtime context exists. After [T20260508-8], CLI subprocess line events include `cwd` when Activity/Job resolved one, matching the audit-started event while omitting the field when the child inherits the parent cwd. It is operational telemetry, not the canonical workflow envelope.

---

## 10. Concerns & Honest Limitations

1. **Tamper evidence is promised more strongly than implemented.** SQLite rows and JSONL tracing files do not yet have hash chains, signatures, or external transparency logs.
2. **Audit is split across stores.** Command rows, v2/loop SQLite events, tracing JSONL, blobs, job-run state, and invocation metrics share ids but lack one joined operator command.
3. **`orbit audit` does not audit itself.** That avoids recursion but leaves audit reads, exports, and prunes outside the normal guard.
4. **Some command-audit fields are sparse.** `stdout_truncated`, `stderr_truncated`, and `session_id` often remain `None`.
5. **CLI backend tool enforcement is weaker than HTTP.** Activity/job audit records the CLI backend allowlist as harness-delegated rather than enforcing Orbit-level tool denial semantics inside the provider path.
6. **Redaction covers known secret shapes.** Environment-value and regex redaction reduce risk but cannot prove arbitrary user secrets are absent from every payload.
7. **The global tracing feed is process-shared.** It is size-rotated and pruned on startup, but has no cross-process line lock; readers should tolerate rare malformed lines if concurrent processes interleave large writes.
8. **Coverage is still expanding.** Some deterministic mutations write explicit audit rows; others rely on enclosing command/job context. The coverage matrix should become the review checklist for new mutation paths.

---

## Task References

- **[T20260419-0002]** — Add workspace provenance and v2 audit envelope events for activity/job execution.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under workspace state.
- **[T20260426-0526]** — Persist v2 invocation traces for metrics beside audit.
- **[T20260426-0605]** — Add this auditability design folder and document the current audit architecture.
- **[T20260426-0705]** — Expose v2 run audit events through `orbit run events` and `orbit run trace`.
- **[T20260426-0709]** — Align run step selectors on activity `step.id` and move CLI invocation log reading behind orbit-core runtime accessors.
- **[T20260426-0742]** — Remove duplicate job-level run inspection aliases and keep run inspection under `orbit run`.
- **[T20260426-2313]** — Stream CLI subprocess stdout/stderr through structured tracing events while retaining the existing audit/blob path.
- **[T20260426-2343]** — Add the global process tracing JSONL feed at `~/.orbit/state/logs/orbit.jsonl`.
- **[T20260426-2349]** — Apply tracing-layer redaction before stderr and global JSONL output.
- **[T20260427-0023]** — Project policy denials and friction task submissions into the global tracing feed.
- **[T20260427-43]** — Superseded friction lifecycle scoring with `status: friction` and history-derived counters.
- **[T20260427-47]** — Allow explicit task attribution correction for `planned_by` and `implemented_by` through task update paths.
- **[T20260428-4]** — Move tool-invocation audit ownership into the runtime, add the `ToolEntryPoint` discriminator, bracket MCP preflight + dispatch, and deduplicate CLI guard rows.
- **[T20260428-11]** — Derive `summary.json` all/failed tool-call counts from command-audit tool-run rows while keeping invocation/token scoreboard data as the token source.
- **[T20260428-17]** — Split local Orbit task-review scoring from PR review-comment scoring and surface both in compact scoreboards.
- **[T20260430-4]** — Change local task-review scoring to count review-thread creations rather than replies, rename the compact field to `task_review.threads`, and keep legacy metric reads mapped forward.
- **[T20260430-20]** — Shorten the auditability docs while preserving required guarantees.
- **[T20260505-6]** — Replace timestamp-only command-audit execution ids with collision-resistant generated ids for parallel tool runs.
- **[T20260506-2]** — Lazily materialize loop audit JSONL files only when loop-level events are emitted.
- **[T20260508-8]** — Record backend: cli subprocess cwd in v2 audit and live tracing.
- **[T20260508-14]** — Surface bounded per-step agent log previews and derived diagnostics error rows in the dashboard.
- **[T20260508-22]** — Use `task.implemented_by` to set git commit authors for automated task commits.
- **[T20260509-12]** — Scope workflow git author and committer identity to the spawned commit process without writing repo-local Git config.
- **[ORB-10519]** — Keep the persisted crew-model author and process-scoped Orbit committer while removing hook-specific trailer input and provider-commit adoption ([Workflow alone creates shipment commits while dirty failures remain recoverable](./4_decisions.md#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable)).
- **[ORB-10369]** — Introduce the persisted resolved crew model as the pipeline commit author with generic fallback and no alias resolver ([Workflow commit authors use the persisted crew model](./4_decisions.md#workflow-commit-authors-use-the-persisted-crew-model), superseded by [Workflow alone creates shipment commits while dirty failures remain recoverable](./4_decisions.md#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable)).
- **[T20260510-13]** — Move friction reports from task lifecycle state to append-only `.orbit/frictions/` records.
- **[ORB-00062]** — Surface first-class friction artifacts in the dashboard Knowledge tab and add triage endpoints.
- **[ORB-00090]** — Aligned agent-facing provenance wording with the family-as-identity convention.
- **[ORB-10337]** — Added dashboard HTTP ingestion for worker invocation records without changing the invocation schema.
- **[ORB-10338]** — Added the versioned model price table and query-time `derived_cost_usd`, plus a persisted `provider_cost_usd` column for reconciliation.
- **[ORB-10370]** — Parsed provider-reported CLI model/cost metadata, preferred the reported model at ingest, and retained structured mismatch evidence.
- **[ORB-10970]** — Mapped Grok Build `modelUsage` ledger ids (`grok-X.Y-build`) to the requested public menu id at ingest identity so stored `invocations.model` prices against the shipped `grok-X.Y` row without warning on the ledger suffix.
- **[ORB-10579]** — Corrected GPT-5.6 price periods and cache-write rates, added gross-input accounting, and retained OpenAI cache buckets for standard short-context estimates.
- **[ORB-10581]** — Added reconciliation-safe managed invocation accounting by canonical task orchestrator ([Attribute managed execution cost to an explicit task orchestrator](../task-artifacts/4_decisions.md#attribute-managed-execution-cost-to-an-explicit-task-orchestrator)).
- **[ORB-10582]** — Projected managed-execution orchestration accounting into the separately versioned dashboard scoreboard section without merging it into executor rankings.
- **[ORB-10591]** — Document the artifact-write redaction boundary, add structural SSH identifier coverage, and return field-level redaction details.
- **[ORB-00106]** — Preserve per-task implementer attribution when `orbit run ship` moves batch PR tasks from Review to Done.
- **[ORB-10200]** — Derive CLI audit metadata and the other cross-cutting command policies from one exhaustive command-operation registry.
- **[ORB-10225]** — Route in-process graph MCP calls through the safe-surface allowlist and shared runtime audit boundary.
- **[ORB-10228]** — Historical expansion of the MCP audit schema; current v1 uses the resolved workspace/process fields, origin session, transport, trace, and optional caller IP while leaving capability/call/lease fields empty.
- **[ORB-10262]** — Historical capability/placement preflight work, no longer part of the v1 MCP execution path.
- **[ORB-10319]** — Historical MCP boundary move; current ownership is `orbit-mcp` framing, CLI server composition, and Core audit dispatch.
- **[ORB-10325]** — Remove graph from MCP and registered tool dispatch while preserving the direct `orbit graph` CLI.
- **[ORB-10357]** — Remove the direct `orbit graph` CLI too; the graph has no audited surface left.
- **[ORB-10451]** — Attribute CLI and dashboard runtimes from the canonical agent env envelope, recording unenveloped callers as unknown.

- **[ORB-10590]** — Gave friction records an author-settable `title` and replaced first-line derivation with a structural, write-time fallback ([Friction records carry an author-settable title; derivation is a structural fallback](./4_decisions.md#friction-records-carry-an-author-settable-title-derivation-is-a-structural-fallback)).
- **[ORB-10680]** — Moved friction records into the host-global SQLite store under `(workspace_id, friction_id)` and pushed filters, paging, and stats into SQL ([Friction records move to SQLite with a legacy-evidence path projection](./4_decisions.md#friction-records-move-to-sqlite-with-a-legacy-evidence-path-projection)).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
