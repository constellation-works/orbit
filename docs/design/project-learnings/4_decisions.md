---
summary: "Project Learnings — Decisions"
type: design
title: "Project Learnings — Decisions"
owner: claude
last_updated: 2026-08-13
last_validated: 2026-09-07
status: Superseded
feature: project-learnings
doc_role: decisions
tags: ["project-learnings"]
---

# Project Learnings — Retired Decisions

> Retired by [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem). The entries before [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem) are historical records only; they do not describe an available Orbit resource, tool, route, index, delivery mechanism, dashboard surface, or authoring workflow.

Format for each entry: **Recorded date and task provenance**, then *Context → Decision → Consequences*. Every decision names at least one cost.

---

## Push-based discovery via context injection, not pull-only via search

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [T20260510-11], [ORB-10736]

### Context
Three classes of discovery were on the table:

| Approach | Profile |
|----------|---------|
| **Pull-only via search tool** | An `orbit.learning.search` MCP tool. Agents query when they think to. Lowest implementation cost; depends entirely on agent discipline. |
| **Push at session start** | All learnings (or an agent-curated subset) load into agent context at session start, like `CLAUDE.md` does. No discipline required, but unscoped and noisy at scale. |
| **Push at the moment of action** | Scoped injection triggered by the file path or task an agent is about to touch. Higher implementation cost; matches discoverability cost to relevance value. |

The repeated failure mode the system exists to prevent is *agents not knowing they should look*. Pull-only inherits that failure mode wholesale: the agent that needed the learning most — the one that forgot the rule — is the one who won't think to query. Session-start push avoids the discipline problem but punishes every session with content that may not apply.

### Decision
Phase 1 ships push-at-the-moment-of-action across three layers: engine pre-prompt injection (universal, task-scoped), MCP tool-response sidecar (cross-agent, file-path-scoped), and Claude Code `PreToolUse` hook (Claude Code only, edit-scoped). A pull surface (`orbit.learning.search`, `orbit-learnings` skill) ships alongside as a complement, not a substitute.

### Consequences
- Agents get relevant learnings without having to query — the discoverability failure mode is closed.
- Authoring effort produces compounding value: every learning is delivered the next time anyone touches the relevant area, automatically.
- The three-layer architecture means coverage degrades gracefully: agents without hook support still get layers 1 and 2.
- Cost: every Orbit-spawned task and every relevant MCP tool call pays a small latency hit for the scope-match query, plus a few dozen tokens of context per injected learning. At expected scale (low hundreds of learnings, sub-millisecond match) the latency is negligible; the context cost is bounded by the per-call cap of 5 and the per-session cap of 20. The cost is real and paid uniformly — even on tasks where no learning applies, the engine still queries to find that out.

---

## Native Orbit primitive (`learning` resource) over a flat markdown directory

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [T20260510-11], [ORB-10736]

### Context
Storage choice. Three plausible shapes:

1. **Flat markdown directory.** `docs/learnings/*.md` plus an index file. Easy to author with any text editor. Cheap to grep. Hard to query programmatically (no structured fields), hard to scope (path globs in markdown frontmatter are non-standard), no native lifecycle (supersession, staleness).
2. **Native primitive in `orbit-store`.** YAML on disk + SQLite index, mirroring tasks. Structured fields (`scope`, `evidence`, `status`), atomic mutations via `orbit.learning.*` tools, indexable for sub-10ms lookups. Implementation cost is real but reuses the existing layered store pattern.
3. **Hybrid: markdown bodies + YAML metadata.** Markdown for content, YAML frontmatter for structure. Familiar to many tools. Splits concerns awkwardly when programmatic mutations write to one half and humans edit the other.

The injection layers (the retired `2_design.md §4`) are the forcing function. Layer 1 has to query "which learnings match this task's context_files" before agent spawn; layer 2 has to do the same per MCP call. Both are hot paths. Grepping markdown frontmatter on every spawn or every tool call is the wrong shape — it makes every layer pay a full filesystem walk for what should be an indexed lookup.

A flat-markdown approach can be retrofitted with an index, but at that point it's a native primitive with extra steps and a less convenient on-disk format.

### Decision
Phase 1 implements `learning` as a first-class Orbit resource: YAML records under `.orbit/learnings/<id>.yaml`, SQLite index under `learnings_index`, MCP/CLI surface mirroring `orbit.task.*`. Tasks were the model because they're the closest existing primitive in shape and lifecycle.

### Consequences
- Hot-path queries are indexed, sub-10ms, and don't pay filesystem-walk cost.
- Lifecycle (`status`, `supersedes`, `superseded_by`) is structurally enforceable.
- The CLI/MCP surface is symmetric with tasks, which lowers the cognitive cost for agents and humans who already know the task model.
- Cost: real implementation work — a new `orbit-store/file/learning_store/` module, a new SQLite table, six MCP tools, six CLI subcommands. This is non-trivial vs. "create a folder and grep it." The bet is that hot-path query performance and lifecycle enforcement justify the build cost over the lifetime of the system.

---

## Workspace-scoped, checked into git (not workspace-private state)

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [T20260510-11], [ORB-10736]

### Context
Where do learning records live on disk?

- **Workspace state** (`.orbit/state/learnings/`, gitignored). Same locality as job runs, command audit, etc. Workspace-private; doesn't survive collaborator handoff.
- **Workspace-scoped, checked in** (`.orbit/learnings/<id>.yaml`, in git). Same locality as tasks. Travels with the repo across machines and collaborators.
- **Global** (`~/.orbit/learnings/`). Like the global skills location. Cross-workspace; requires conflict semantics if multiple workspaces author overlapping records.

Per the Scoping Rules table in [CLAUDE.md](../../../CLAUDE.md), tasks are `WorkspaceOnly` and live in `.orbit/tasks/` checked in. Job runs are also `WorkspaceOnly` but under `.orbit/state/`, gitignored, because they're execution artifacts. Learnings sit closer to tasks in shape — durable project artifacts authored over time — so the task locality is the right precedent.

The cross-workspace case (the retired `3_vision.md §1.4`) is real but secondary: most learnings are repo-specific, and the cross-cutting ones are best handled by tag-driven promotion later, not by making the default storage location global.

### Decision
Phase 1 stores learnings at `.orbit/learnings/<id>.yaml`, scoped `WorkspaceOnly` per the Scoping Rules table, checked into git. The SQLite index lives under `.orbit/state/` and is rebuildable from the YAML; it does not need to be checked in.

### Consequences
- Learnings travel with the repo. New collaborator clones, gets all the project knowledge from day zero.
- A learning authored on one machine and a task fix on another arrive in the same PR and review together, which keeps the knowledge in lockstep with the code that produced it.
- The git semantics for tasks (review, merge, conflict resolution) apply uniformly; no new mental model needed.
- Cost: every learning is a commit. PR diffs include learning records, which is fine for substantive learnings but adds review noise for housekeeping edits (typo fixes, scope-glob tweaks). Merge conflicts on the SQLite index are avoided by gitignoring it, but conflicts on the YAML are possible when two PRs add learnings simultaneously — handled by ID allocation (date + sequence), but worth noting.

---

## Phase-1 scope = path globs + tags, ranked by recency; semantic and symbol-aware deferred

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [T20260510-11], [ORB-10736]

### Context
A learning's scope (when does it match?) and ranking (which match wins?) have multiple plausible designs:

| Scope axis | Profile |
|------------|---------|
| **Path globs** | Match against file paths the agent is about to touch. Stable shape, simple matcher (reuses `orbit-policy`'s glob engine). Brittle to file renames. |
| **Tags** | Free-form labels. Survive renames. Require the author to anticipate the categorization. |
| **Symbol IDs** | Match against knowledge-graph symbols. Survive renames cleanly. Couples to graph rebuilds. |
| **Semantic similarity** | Match by embedding distance to current edit context. Catches relevance the other axes miss. Depends on semantic-search infrastructure. |

| Ranking | Profile |
|---------|---------|
| **Recency (`updated_at` desc)** | Trivial. Wrong when an old, important learning loses to a recent, marginal one. |
| **Manual `priority`** | Author-supplied. Honest signal when used; degenerates to "everything is high priority" without curation discipline. |
| **Semantic similarity** | Best signal. Requires embeddings. Cost = embed every learning + run cosine on every query. |

Phase 1's binding constraint is: ship before semantic-search reaches Accepted ([T20260510-3]). That rules out semantic similarity for both scope and ranking. Symbol-aware scope is *technically* available — the knowledge graph already exists — but coupling the learning store to graph rebuilds adds dependency surface and mainly pays off when fused with semantic ranking. Doing one without the other yields a clunky middle state.

### Decision
Phase 1 supports two scope axes, evaluated as logical OR: path globs (matched via the `orbit-policy` glob engine) and tags (matched as exact strings). Ranking is `updated_at` desc with optional `priority` tagging as a tie-breaker. The schema reserves `scope.symbols` and `scope.semantic_seed` fields for phase 2 forward compatibility, but neither is read in phase 1.

Phase 2 (the retired `3_vision.md §1.1` and `§1.2`) layers symbol-aware scope and semantic ranking once semantic-search ships.

### Consequences
- Phase 1 is implementable in parallel with semantic-search work, not gated on it.
- Path globs cover the common case (most learnings are file-area-scoped) and tags cover the cross-cutting case.
- The schema is forward-compatible; phase 2 is additive, not a migration.
- Cost: recency-only ranking has known failure modes (the retired `3_vision.md §1.2`) — old-but-important learnings get out-ranked by recent-but-marginal ones. Path globs are brittle to renames; the documented mitigation is "run `orbit learning prune --stale-only` after refactors that move files," which is operational discipline, not automation. Both costs are accepted as the price of shipping phase 1 ahead of semantic-search.

---

## Three-layer push pipeline (engine pre-prompt + MCP sidecar + Claude Code hook), not single-layer

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [T20260510-11], [ORB-10736]

### Context
The push-injection layer (the retired `2_design.md §4`) has multiple natural placements, each with different coverage:

- **Engine pre-prompt only.** Inject when `orbit-engine` spawns an agent for a task. Universal across agents. Coarse: fires once at task start, before the agent has read its way to the relevant code, so narrow learnings (file-path-scoped) may not surface for the file the agent edits ten tool calls in.
- **MCP-sidecar only.** Attach `learnings` to MCP tool responses that reference paths. Cross-agent. Misses Claude Code's built-in `Edit | Write | Read`, which agents use far more than they call MCP file tools.
- **Claude Code `PreToolUse` only.** Per-edit precision. Vendor-locked: doesn't apply to Codex, Gemini, Anthropic-API, Ollama, or any other agent runtime.
- **All three layered.** Each layer adds precision on top of the layers below. Coverage degrades gracefully: agents without hook support still get layers 1 and 2; tools without path arguments still get layer 1.

The vendor-locked single-layer options are non-starters because the project supports multiple agent providers (see `crates/orbit-agent/providers/`). Engine-pre-prompt-only misses the long-task case where an agent works for an hour through a wide context. MCP-sidecar-only misses the most-frequent agent action (built-in editor tools).

### Decision
Phase 1 ships all three layers active simultaneously. Each layer consults a per-session deduplication set so the same learning doesn't inject multiple times across layers. Per-call cap of 5 learnings; per-session cap of 20.

### Consequences
- Coverage is robust: even if one layer misfires or a vendor lacks hook support, the others provide a baseline.
- Agents see relevant learnings at multiple natural moments — task start, MCP tool call, individual edit — without being drowned in repeats (dedup set).
- The architecture admits a future "layer 4" (Orbit-side proxy for agents without hooks) without restructuring, but doesn't require it (the retired `3_vision.md §1.5`).
- Cost: three injection sites means three places to maintain. A schema change to learning records (new field surfaced at injection time) requires touching `orbit-engine`, `orbit-mcp`, and the Claude Code hook script. The dedup set is agent-local; if context is compressed mid-session, the set may reset and the same learning may inject twice. Both costs are accepted as the price of robust coverage; collapsing to a single layer would mean choosing one failure mode (vendor lock-in, coarse scope, or missing built-in tools) and living with it.

---

## Task References

- [T20260510-11] — Design + build project-learnings system as native Orbit primitive. The task that produced this folder.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Rank matched learnings by task-anchored decay-weighted upvotes

**Superseded by:** [Remove vote and comment surfaces from the learning subsystem](#remove-vote-and-comment-surfaces-from-the-learning-subsystem)
**Recorded:** 2026-08-01 19:18:13.490059Z · [ORB-00095], [ORB-10479]

**Context.** Recency and manual priority do not capture whether a learning is still load-bearing. An older learning that agents keep relying on should outrank a newer marginal note, but `updated_at` only moves when the learning body changes. The natural re-validation moment is duplicate-check: an agent reads a candidate learning, decides it already covers the concern, and does not author a competing record.

Alternatives considered:

| Approach | Profile |
|----------|---------|
| **Keep recency + priority only** | No new state. Continues conflating "was once written" with "is still useful." |
| **Global vote count** | Simple. Lets ancient high-volume learnings outrank recently useful ones forever. |
| **Task-anchored decayed votes** | Captures repeated usefulness across work contexts while letting old signal fade. Requires a sidecar file and idempotency policy. |
| **SQLite vote mirror first** | Fast summaries. Adds schema/cache complexity before measured need. |

**Decision.** Each learning may have `.orbit/learnings/<id>/votes.jsonl`, created lazily on first vote. Each row records `learning_id`, `voter_model`, `voted_at`, and `task_id`. V1 rejects votes without `task_id`; idempotency key is `(learning_id, voter_model, task_id)`. Search ranking filters by scope first, then sorts by decay-weighted vote score, `priority`, `updated_at`, and `id`. Default half-life is 180 days; `ORBIT_LEARNING_VOTE_HALF_LIFE_DAYS=0` disables decay for raw-count behavior.

Votes are derived from per-learning JSONL on read. `orbit learning sync` validates vote files but does not rewrite them or mirror them into SQLite.

**Consequences.**
- Load-bearing learnings accrue a ranking signal without mutating the YAML body or bumping `updated_at`.
- Duplicate-check becomes constructive: "this already exists" reinforces the existing record instead of producing a duplicate.
- Per-learning files keep write contention local; same-learning upvotes serialize with a per-learning lock and append atomically.
- Cost: vote spam is possible if agents upvote reflexively. Task anchoring, idempotency, and decay reduce but do not eliminate that risk.
- Cost: search now opens one small votes file per matched learning. This is acceptable for the expected 1-20 row matched sets; a SQLite summary mirror is deferred until measurement shows a need.

## Remove vote and comment surfaces from the learning subsystem

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [ORB-10046], [ORB-10736]
**Supersedes:** [Rank matched learnings by task-anchored decay-weighted upvotes](#rank-matched-learnings-by-task-anchored-decay-weighted-upvotes)
**Paths:** `crates/orbit-tools/src/builtin/orbit/learning/**`, `crates/orbit-store/src/file/learning_store/**`, `crates/orbit-cli/src/command/learning/**`, `crates/orbit-core/src/runtime/orbit_tool_host/learning_tools.rs`, `crates/orbit-common/src/types/learning.rs`

### Context

The learning subsystem shipped two auxiliary surfaces beside the core add/update/supersede/evidence primitives:

- **Votes** (`orbit.learning.upvote`, `votes.jsonl` sidecar, `learning_vote_summary`, decay-weighted vote score in search ranking): a task-anchored upvote used as a secondary rank key.
- **Comments** (`orbit.learning.comment.{add,list,delete}`, `comments.jsonl` sidecar, `LearningReminder.comments`, a free-text redaction policy in `artifact_redaction.rs`): free-text footnotes anchored to a learning, injected under the learning summary in reminder blocks.

Across orbit's own `.orbit/learnings/` corpus (50 records), exactly one record had any comments (L-0005, one comment) and zero had any votes. The team had already half-retired both in ORB-00289/00348: `orbit.learning.upvote`, `orbit.learning.comment.list`, and `orbit.learning.comment.delete` were flipped to `register_inactive` (operator-only, off the agent-facing MCP surface). This ADR records the decision to finish the direction and remove both surfaces entirely.

The alternative that was on the table was **keep both, narrower**: keep `comment.add` as a lightweight annotation channel (accepted, but with clearer wording about when to prefer `update`/`supersede`), and keep `upvote` as a signal channel for "this learning is still useful" recency without ranking impact. That alternative was rejected below.

### Decision

Remove the vote and comment surfaces from the learning subsystem entirely. Concretely:

- Delete the tool definitions `orbit.learning.{upvote,comment.add,comment.list,comment.delete}` and their `OrbitBuiltinAction` variants; drop them from the tool registry, MCP host `LEARNING_TOOL_NAMES`, and both `INACTIVE_TOOL_NAMES` canaries.
- Delete the CLI subcommands `orbit learning upvote` and `orbit learning comment`.
- Delete the store surface: `LearningStoreBackend::{upvote_learning,learning_vote_summary,add_learning_comment,list_learning_comments,delete_learning_comment}` and their file-backend impls; `votes.jsonl`/`comments.jsonl` layout paths; `LEARNING_COMMENTS_FILE_NAME`; `next_learning_comment_id`, `validate_learning_comment_id`, and comment JSONL record helpers.
- Drop the scoreboard `learning_votes_received` column (from both the Rust struct and the dashboard `scoreboard.js` renderer/aggregator).
- Drop `LearningComment`, `LearningCommentEvent`, `LearningCommentTombstone`, `LearningVoteRow`, `LearningVoteSummary`, `DEFAULT_LEARNING_COMMENT_RENDER_CAP`, `read_comment_render_cap_env`, `decayed_vote_score`, and `NotFoundKind::LearningComment` from `orbit-common`.
- Drop the `comments: Vec<LearningComment>` field from `LearningReminder`; the reminder block now renders `- [id] summary` per record with no nested footnotes.
- Drop the `orbit.learning.comment.add` policy entry from `artifact_redaction.rs` and its `ArtifactTarget { artifact_type: "learning_comment", ... }` mapping.
- Migrate the single existing comment (L-0005/C20260519-1, about `include_str!` entries in `crates/orbit-core/src/command/skill.rs`) into the L-0005 learning body via `orbit.learning.update` before the surface is deleted, so the datum is preserved.

### Consequences

- **Corrections and provenance are funneled through the primary surfaces.** Curators correct current wording with `orbit.learning.update`, mark material changes with `orbit.learning.supersede`, and cite provenance with the `evidence` array (`{kind, ref}`). That was already the documented pattern; comments were a weak middle-ground primitive that muddied it.
- **Search ranking is now `priority` desc → `updated_at` desc → `id` asc.** The decay-weighted vote score dropped out of the primary sort key. Since no learning had votes, this changes no observed rankings today; if a reason to re-rank by recency of validation returns, `updated_at` (bumped on every `update`) is the natural signal — no new dedicated store is needed.
- **Attack surface shrinks.** Free-text comments required the `LearningCommentAdd` entry in the artifact-redaction policy (`body` scrubbed for env-injected credentials and home-dir paths). That policy entry, its `learning_comment` artifact-target mapping, and the comment-only branch of the audit-emit path are gone.
- **Store layout is simpler.** Each learning is now `<L-id>/learning.yaml` and nothing else in the common case. `sync`/reindex no longer needs to validate comment JSONL files. Rollback of a partial create no longer needs to remove sidecar files.
- **`LearningReminder` is smaller and cheaper to hydrate.** No per-reminder comment scan; the reminder block loses its footnote lines. Consumers of the reminder JSON (v2 host, MCP sidecar, CLI hook renderers) all lose the optional `comments` field; a client that hand-constructed a reminder with `comments: []` no longer compiles.
- **A follow-up avenue exists if voting-style feedback is ever needed.** `friction.add` covers "this learning is wrong / has bit me" as an incident channel that already routes to the human triage surface; `supersede` (with the new record's `evidence` carrying the incident's task ID) covers the material-change path. Nothing about this removal precludes reintroducing a scoped feedback primitive later, but it should be driven by real usage, not carried speculatively.
- **Cost: this is a breaking change to the tool and store surfaces.** External callers of `orbit.learning.upvote`/`comment.*`, of the store trait methods, or of the removed types on `orbit_common`/`orbit_core` public API must be updated in the same release; the length canary in `EXPECTED_INACTIVE_TOOL_NAMES` moves from 26 to 22. The change is documented in CHANGELOG.md's Breaking Changes section under ORB-10046.

### Rejected alternative: keep `comment.add` as an annotation channel; keep `upvote` for recency

Rejected because (a) usage is zero-to-one after months of availability, so we would be defending a feature with no evidence of demand; (b) `update` on an existing learning already carries the author's `model` and bumps `updated_at`, subsuming any recency signal a vote would carry; (c) `evidence` on `add`/`update` carries provenance in structured form that comments cannot; (d) keeping comments preserves the free-text redaction burden that drove ~50 LOC in `artifact_redaction.rs` plus its tests; (e) the store cost — two extra JSONL sidecars per learning, ID allocators for `C<YYYYMMDD>-N` comment IDs, tombstone events, and reindex validators — is not justified by one comment across the corpus. If a lightweight annotation channel is genuinely useful later, it can be reintroduced with actual usage data behind it.

## Workspace-scope the learning envelope index in the shared host-global database

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [ORB-10113], [ORB-10736]
**Paths:** `crates/orbit-store/src/sqlite/learning_index.rs`

**Context.** The learning envelope index lives in the host-global `~/.orbit/orbit.db`, but `learnings_index` had no workspace discriminator and was keyed only by learning ID. Every workspace runtime pairs that shared table with its own workspace-local `.orbit/learnings/` YAML root, and `sync_learnings` truncated the entire table before reinserting one workspace's records. During the multi-workspace ship sweep on `dk1`, runtimes searched index rows written by other workspaces, emitting repeated `learning not found: L-0002` warnings during reminder hydration. Worse: several legacy workspaces hold different records under the same canonical ID (e.g. `L-0002`), so a foreign summary could be silently injected under a local ID.

**Decision.** Re-key `learnings_index` by the composite `(workspace_id, id)`, where `workspace_id` is the stable registered Orbit workspace ID (the same id used for `job_runs` and `v2_audit_events`, read from `.orbit/config.yaml`), not a path-derived label that changes across worktrees. Every index operation — search, upsert, delete, truncate/rebuild, cache materialization, and reminder lookup — is filtered by the runtime's own `workspace_id`. A v2 schema migration drops and recreates the table with the composite key; because YAML is the source of truth and legacy rows cannot be attributed to a workspace, the migration discards every envelope row and lets each runtime rebuild its own via `sync`. The migration touches only SQLite and never reads or modifies `learning.yaml`.

**Consequences.** Syncing workspace A can no longer read, truncate, or overwrite workspace B's rows; concurrent multi-workspace sweeps are safe. The reminder defensive local-body check still skips genuine same-workspace index ghosts. Existing envelope rows are discarded on upgrade and rebuilt from YAML on the next sync per workspace — the documented rollback also just reruns sync. Supersedes the "no shared store across workspaces" framing in design §5.3: the store is shared, but rows are workspace-partitioned.

## Teaser learning injection + show-as-usage-signal (replaces rejected ack design)

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [ORB-10316], [ORB-10736]
**Paths:** `crates/orbit-cmd/src/learning_hook.rs`, `crates/orbit-core/src/command/learning.rs`, `crates/orbit-store/src/sqlite/audit_event_store/mod.rs`, `crates/orbit-common/src/types/learning.rs`

### Context

The 2026-07-18 relevancy audit (friction F2026-07-092) found the learning PreToolUse hook fired 2,374 times over two weeks with 13 injections (0.55%) and **zero usage signal**: nothing recorded whether an injected learning shaped the receiving agent's work, so nothing could drive deprecation of stale learnings. [Remove vote and comment surfaces from the learning subsystem](#remove-vote-and-comment-surfaces-from-the-learning-subsystem) removed the vote/comment feedback surfaces for lack of real usage, ending with an explicit reopening clause: a scoped feedback primitive can return "with real usage data behind it." The audit is that data.

A first attempt (PR #657, closed unmerged) added an explicit `orbit learning ack` CLI/MCP surface with ignored-by-default semantics. Daniel rejected it: an ack is an active, gameable step the agent must remember to take, it costs a reminder-block footer line and a new MCP tool in the frozen conformance surface, and "unacked = ignored" biases every silent session toward deprecation regardless of whether the learning was actually useless.

Separately, per-session injection dedup was dead in interactive sessions — `ORBIT_SESSION_ID` was exported on 0/2,374 observed fires, and the ppid-tmpfile fallback re-keys per invocation because every hook fire runs under a fresh parent shell (L-0077 injected 10× in one session).

Alternatives considered:

| Approach | Profile |
|----------|---------|
| **Explicit `learning ack` surface (PR #657)** | Active, gameable, adds an MCP tool to the frozen conformance fixture and a reminder footer line; silence forced to mean "ignored." Rejected by Daniel. |
| **Full-content injection + no signal (status quo)** | High token cost per fire, no usage data, no deprecation input. |
| **Teaser injection + show-as-signal (this ADR)** | Injection carries only id + summary + tags; opening the full body via `orbit learning show` is the passive, ungameable usage signal. Lower token cost, no new agent action, no new MCP tool. |

### Decision

1. **Teaser injection.** The injection layers project only the learning id, one-line summary, and scope tags into agent context (`render_reminder_block`). The full body is retrieved on demand via `orbit learning show <id>` — the reminder block already tells the agent how. This drops per-fire injection token cost and makes "read the full learning" an explicit, observable act.

2. **Show-as-usage-signal.** `orbit learning show` (CLI and `orbit.learning.show` MCP tool) records a `learning_shown` audit event in the host-global `~/.orbit/orbit.db`, keyed by learning id + session, alongside the existing `learning_injected` events. It is the passive signal: an agent that opens a learning found the teaser worth expanding. No ack, no new tool, no schema change — `orbit.learning.show` already exists.

3. **Aggregation.** `orbit learning stats` folds `learning_injected` + `learning_shown` per learning into injected count, shown count, shown ratio, and last-injected/last-shown timestamps (CLI + `learning_usage_stats` runtime API). This rollup is the designed input for the downstream deprecation sweep (ORB-10318); a low shown ratio (injected often, never read) is the deprecation-candidate signal. No deprecation logic lives here.

4. **Fail-open instrumentation.** An unavailable audit backend logs a warning and injection still renders; `learning show` logs a warning and still returns the learning when the `learning_shown` emit fails. The signal is best-effort observability and must never break the read or injection path.

5. **Session dedup** keys on the first resolvable anchor: `ORBIT_SESSION_ID` env (engine-managed runs export it, pre-seeded with layer-1 injections) → the `session_id` field the hook payload itself carries (Claude Code sends it on every hook event) → ppid-tmpfile last resort.

**No ack surface.** There is deliberately no `orbit learning ack` CLI, no `orbit.learning.ack` MCP tool, and no ack instruction in the injected block.

### Consequences

- The rollup is the designed input for downstream deprecation policy (ORB-10318); decay/TTL is deliberately follow-up work, not implemented here.
- The `learning_shown` / `learning_injected` contract lives in audit-event conventions (`target_type` + `arguments_json`) enforced by store-level fold tests, not a schema migration — consistent with the injection events it joins against.
- Signal quality depends on agents actually opening learnings they use, but `show` is far harder to game than an ack and costs the agent nothing extra to emit; the ratio is directional input for a human/automated sweep, not an automated gate.
- Cost: one audit row per `show`, plus scope tags added to each teaser line. No change to the MCP conformance surface (no new tool), unlike the rejected ack design.

## Keep `orbit hook install` opt-in; remove `--hooks` from `workspace init`

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [ORB-10366], [ORB-10346], [ORB-10736]
**Paths:** `crates/orbit-cli/src/command/workspace/init.rs`, `crates/orbit-cli/src/command/hook/**`, `crates/orbit-cmd/src/hook_install.rs`

### Context

[ORB-10346] removed the learning-reminder PreToolUse registrations this repo's own `.claude/settings.json` and `.codex/config.toml` carried, per the pull-discovery direction ([Push-based discovery via context injection, not pull-only via search](#push-based-discovery-via-context-injection-not-pull-only-via-search)/[Three-layer push pipeline (engine pre-prompt + MCP sidecar + Claude Code hook), not single-layer](#three-layer-push-pipeline-engine-pre-prompt-mcp-sidecar-claude-code-hook-not-single-layer) supersession, [Teaser learning injection + show-as-usage-signal (replaces rejected ack design)](#teaser-learning-injection-show-as-usage-signal-replaces-rejected-ack-design) amendment). It deliberately left the writer mechanism itself untouched — `orbit workspace init --hooks` and `orbit hook install` both still silently wrote those registrations back for any repo that invoked them, and the tree still carried two now-inert tracked shim files (`.claude/hooks/orbit-learning-reminder`, `.codex/hooks/orbit-learning-reminder`) left over from before the retirement. [ORB-10366] closes that gap.

Two call sites write the registration via `orbit_cmd::hook_install::install_for_workspace`:

1. `orbit workspace init --hooks` (`crates/orbit-cli/src/command/workspace/init.rs`) — an implicit side effect of the init flow, easy to invoke unintentionally from a script or muscle memory (`orbit init --hooks` from an older doc, a stale redeploy script, etc.).
2. `orbit hook install` (`crates/orbit-cli/src/command/hook/install.rs`) — a standalone, explicitly human-invoked command with no other purpose.

### Decision

Remove the `--hooks` flag from `orbit workspace init` entirely (clap now rejects it as an unknown argument rather than silently ignoring it) and delete the two tracked inert shim files. Keep `orbit hook install` / `orbit hook uninstall` as an explicit, opt-in, human-invoked escape hatch — do not remove the command.

The distinction that matters is *automatic* vs. *deliberate*. [Push-based discovery via context injection, not pull-only via search](#push-based-discovery-via-context-injection-not-pull-only-via-search)/[Three-layer push pipeline (engine pre-prompt + MCP sidecar + Claude Code hook), not single-layer](#three-layer-push-pipeline-engine-pre-prompt-mcp-sidecar-claude-code-hook-not-single-layer)'s problem was learnings being pushed into agent context without anyone choosing that — the failure mode was "agents not knowing they should look," closed by pull discovery instead. An `--hooks` flag riding along on `workspace init` (a command run for unrelated reasons — bootstrapping a new checkout) reproduces exactly that: a side effect nobody asked for in the moment. `orbit hook install` has no such ambiguity — it is the only thing the command does, so running it is itself the deliberate choice, same category as a human explicitly opting back into the old delivery model for a specific reason (e.g. a non-Claude-Code agent runtime that has no other discovery path, or reproducing pre-retirement behavior for comparison). Removing it forecloses that choice for no safety gain, since `orbit hook uninstall` (needed regardless, to clean up pre-ORB-10366 registrations like this repo's own) is already the same shape of explicit command.

### Consequences

- No code path reachable from `orbit init` or `orbit workspace init` writes a learning-reminder registration; a fresh `workspace init` against a temp workspace root leaves `.claude/settings.json` / `.codex/config.toml` untouched (asserted in `crates/orbit-cli/tests/hook_install.rs`).
- `orbit workspace init --hooks` now fails with a clap "unexpected argument" error instead of silently succeeding — a stale script or doc still passing it breaks loudly at the call site instead of appearing to work.
- `orbit hook install` / `orbit hook uninstall` are unchanged; `orbit_cmd::hook_install::{install,uninstall}_for_workspace` and the underlying JSON/TOML merge helpers are untouched, so the only diff is at the two CLI call sites plus the flag itself.
- The independently-registered `scripts/orbit-file-lock` PreToolUse guard and the `orbit hook pretooluse` / `learning_hook::run_pretooluse` runtime path are unrelated to this change and keep working exactly as before.
- Cost: `orbit hook install` remains capable of re-registering the retired delivery mechanism if a human runs it deliberately. This is accepted as the intended behavior of an opt-in escape hatch, not a gap — the alternative (removing the command) would require touching `orbit-cli`'s audit-metadata match, `operation.rs`'s exhaustive command arm, and the `--help` template for a command with a legitimate use case and no automatic trigger.
- Cost: the two tracked inert shim files this repo carried are deleted rather than kept as reference examples; `orbit hook install` regenerates them byte-identically if ever re-run, so nothing is lost.

## Gate learning authoring surfaces on caller role derived from the agent-identity env

**Superseded by:** [Remove the native project-learning subsystem](#remove-the-native-project-learning-subsystem)
**Recorded:** 2026-08-13 · [ORB-10364], [ORB-10469], [ORB-10736]

**Context.** Policy since 2026-07-18: task executors file *frictions*; project learnings are authored by the orchestrator or by Daniel. The rule lived entirely in orchestrator-side prompt text and nothing on the box enforced it. Three different executors violated it in a single day's queue (friction F2026-07-102): L-0108 from a sol executor, L-0109 plus an update to L-0082 from an opus executor. The content was not the problem — the *surface* was. Because the learning store feeds scope injection and curation sweeps, unreviewed executor-authored entries accumulate as a side effect of ordinary task runs, and curation work (ORB-10349 / ORB-10362) then spends real effort re-anchoring and narrowing exactly those entries. Three consecutive failures is evidence that prompt text is the wrong enforcement layer.

Alternatives considered:

| Approach | Profile |
|----------|---------|
| Keep it in prompt text | Zero code. Already failed three times in one day; the failure mode is silent and only visible during curation. |
| Gate the store-level chokepoint (`create_learning` / `update_learning` / `supersede_learning`) | One call site instead of six. But it also catches the dashboard's HTTP write actions, and ORB-10352 had just moved dashboard write attribution *off* server env precisely because the dashboard can run with an ambient agent identity — gating there would block Daniel's own dashboard edits. Rejected. |
| Gate the authoring surfaces (this ADR) | The CLI subcommands and `orbit.learning.*` tools an executor can actually reach, via shared role-gated runtime wrappers. Six call sites, no collateral on the broker or fixtures. |

**Decision.** `orbit learning add` / `update` / `supersede` and their `orbit.learning.*` tool equivalents route through role-gated `OrbitRuntime::author_learning{,_update,_supersede}` wrappers that call `ensure_learning_write_allowed` (`crates/orbit-core/src/command/learning_authoring.rs`). The caller's role is derived from the agent-identity env pair the audit middleware already reads — `ORBIT_AGENT_NAME` / `ORBIT_AGENT_MODEL`, assembled for every spawned run by orbit-engine's `provenance_env` builder — consumed through the existing `ActorIdentity::from_env`. **No parallel identity mechanism.** An executor-context caller is refused with `OrbitError::PolicyDenied` whose message names `orbit friction add` as the correct channel and echoes the attempted summary/body (truncated at 1500 chars) so the observation is not lost. `ORBIT_LEARNING_AUTHOR=1` is the sanctioned opt-in for an orchestrator dispatching curation work *as* an agent; the provenance builder never emits it, so it cannot be inherited by accident, and only the exact affirmative spellings `1` / `true` / `TRUE` count.

**Consequences.**
- Enforcement moves from prompt text into code at the two surfaces an executor can actually reach (CLI and tool dispatch). The two entry points share one gate, so CLI and MCP cannot drift.
- Reads are untouched in every context: `show`, `list`, `search`, `stats`, and scope injection behave identically for human, executor, and opted-in orchestrator callers.
- `sync`, `prune`, `archive`, the multi-host owner-finalize path, and test fixtures keep the ungated store-level methods, which now carry doc comments pointing at the `author_*` wrappers.
- The dashboard needed one explicit change: `PATCH /api/learnings/:id` delegated to the `orbit.learning.update` tool (to inherit the superseded-record rejection), which would have put a human's dashboard edit behind a gate keyed on the *server process's* environment. The tool handler is now split into shared payload parsing plus the gated write, and the route calls a new ungated `OrbitRuntime::update_learning_from_request`. `POST .../supersede` already called the ungated runtime method and is unchanged.
- `finalize_preallocated_learning` stays ungated deliberately: by the time a caller reaches finalize the global id is already consumed and cannot be released, so a refusal there would burn an id. When ORB-10274 (F3) routes public `orbit.learning.add` through the broker, the gate belongs in the preflight *before* `compose_preallocated_knowledge_add` allocates.
- `crates/orbit-cli/tests/learning.rs`, `crates/orbit-cli/tests/docs.rs`, and the tool-host learning tests now declare their caller context explicitly instead of inheriting whatever the suite was launched with (the ORB-10350 ambient-leak hazard, which this gate turns from a latent inconsistency into a hard failure).
- Cost: the gate is applied at six call sites rather than one chokepoint, so a future authoring surface must remember to use the `author_*` wrappers — a tradeoff accepted to keep the dashboard's request-derived attribution working.

## Task References

- [T20260510-11] — Design + build project-learnings system as native Orbit primitive. The task that produced this folder.
- [ORB-10046] — Remove the vote and comment surfaces from the learning subsystem ([Remove vote and comment surfaces from the learning subsystem](#remove-vote-and-comment-surfaces-from-the-learning-subsystem) supersedes [Rank matched learnings by task-anchored decay-weighted upvotes](#rank-matched-learnings-by-task-anchored-decay-weighted-upvotes)).
- [ORB-10316] — Teaser injection + `learning_shown` usage signal + `orbit learning stats` rollup + payload-derived session dedup ([Teaser learning injection + show-as-usage-signal (replaces rejected ack design)](#teaser-learning-injection-show-as-usage-signal-replaces-rejected-ack-design)).
- [ORB-10346] — Retired the Claude Code `PreToolUse` hook layer (one of three automatic-delivery layers) while retaining pull discovery, `learning_shown`, and historical usage stats. Engine pre-prompt injection and the MCP sidecar decorator remain active.
- [ORB-10366] — Removed the `--hooks` flag from `orbit workspace init` and the tracked inert shims; kept `orbit hook install` as an opt-in escape hatch ([Keep `orbit hook install` opt-in; remove `--hooks` from `workspace init`](#keep-orbit-hook-install-opt-in-remove---hooks-from-workspace-init)).
- [ORB-10364] — Gated the learning authoring surfaces on caller role and redirected executors to `friction add` ([Gate learning authoring surfaces on caller role derived from the agent-identity env](#gate-learning-authoring-surfaces-on-caller-role-derived-from-the-agent-identity-env)).
- [ORB-10452] — Made the legacy learning-layout migration report-only by default and require the standard `--confirm` apply flag ([Workspace-scoped, checked into git (not workspace-private state)](#workspace-scoped-checked-into-git-not-workspace-private-state) amendment).

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Remove the native project-learning subsystem

**Recorded:** 2026-08-13 · [ORB-10736]
**Supersedes:** [Push-based discovery via context injection, not pull-only via search](#push-based-discovery-via-context-injection-not-pull-only-via-search), [Native Orbit primitive (learning resource) over a flat markdown directory](#native-orbit-primitive-learning-resource-over-a-flat-markdown-directory), [Workspace-scoped, checked into git (not workspace-private state)](#workspace-scoped-checked-into-git-not-workspace-private-state), [Phase-1 scope = path globs + tags, ranked by recency; semantic and symbol-aware deferred](#phase-1-scope-path-globs-tags-ranked-by-recency-semantic-and-symbol-aware-deferred), [Three-layer push pipeline (engine pre-prompt + MCP sidecar + Claude Code hook), not single-layer](#three-layer-push-pipeline-engine-pre-prompt-mcp-sidecar-claude-code-hook-not-single-layer), [Remove vote and comment surfaces from the learning subsystem](#remove-vote-and-comment-surfaces-from-the-learning-subsystem), [Workspace-scope the learning envelope index in the shared host-global database](#workspace-scope-the-learning-envelope-index-in-the-shared-host-global-database), [Teaser learning injection + show-as-usage-signal (replaces rejected ack design)](#teaser-learning-injection-show-as-usage-signal-replaces-rejected-ack-design), [Keep orbit hook install opt-in; remove --hooks from workspace init](#keep-orbit-hook-install-opt-in-remove---hooks-from-workspace-init), [Gate learning authoring surfaces on caller role derived from the agent-identity env](#gate-learning-authoring-surfaces-on-caller-role-derived-from-the-agent-identity-env)

### Context

The native resource was justified primarily by automatic scope-matched delivery. In practice that signal did not justify a separate model, file store, SQLite projections, lifecycle API, CLI/MCP/HTTP surface, search corpus, hook and session state, dashboard, metrics, and scheduled curation machinery. Keeping a read-only compatibility resource or translating records into another artifact would preserve most of the maintenance burden and create a new long-term contract.

### Decision

Remove the native project-learning subsystem and every executable or advertised surface that depends on it. Existing files under `.orbit/learnings/**` remain byte-for-byte inert historical data: they are not read, indexed, injected, migrated, rewritten, or copied elsewhere. Preserve the shipped SQLite migration ledger and append a forward migration that drops the retired tables and learning vector rows for both upgraded and freshly initialized databases. Unified search continues for tasks, docs, ADRs, and frictions; the retired kind is rejected.

### Consequences

- Orbit has no native learning model, store, lifecycle, delivery path, tool, route, dashboard surface, metric, hook installer, or scheduled curation task.
- Existing historical files remain available only to repository archaeology and confer no compatibility promise.
- The narrower product surface reduces schema, conformance, security, and cross-layer maintenance cost.
- Cost: automatic delivery of scoped project rules is gone. Teams that need durable guidance must use ordinary reviewed documentation or existing repository instructions, without a native replacement resource or content migration.
- Rejected alternative: retaining a read-only compatibility layer. It would continue advertising a resource whose lifecycle and delivery semantics no longer exist.
