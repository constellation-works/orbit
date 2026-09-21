---
title: Orbit Core — Decisions
owner: claude
last_updated: 2026-09-21
status: Accepted
feature: orbit-core
doc_role: decisions
type: design
summary: Decision log for orbit-core crate-boundary decisions, starting with the ORB-10016 orbit-cmd extraction.
tags: [orbit-core, orbit-cmd, architecture, north-star]
paths: ["crates/orbit-core/**", "crates/orbit-cmd/**"]
related_features: [orbit-core]
related_artifacts: [ORB-10026, ORB-10545, ORB-12772]
last_validated: 2026-09-21
---

# Orbit Core — Decisions

This document preserves the feature's non-obvious decisions and their reasoning.

---

## Extract the CLI-facing command layer into orbit-cmd

**Recorded:** 2026-07-04 10:17:48.375334Z · [ORB-10016]

Split the CLI-facing command layer out of the orbit-core god-crate into a new internal crate orbit-cmd that depends on orbit-core (never the reverse). Moved groups (doctor, migrate, diagnostics, task templates, agent-rules, hook install, learning/review-thread PreToolUse hook, direct v2 activity runner) expose OrbitRuntime methods as *Commands extension traits. Runtime-entangled command groups (task, learning, docs, search, semantic, job, tool, audit, pipeline, init/seeding, workflow, skill, activity, policy, executor, backend-resolver, task-migration, review-thread-hook) remain in orbit-core because the runtime tool hosts / engine hosts / bootstrap invoke them. orbit-core root re-exports trimmed to the consumer-justified set. Long-form narrative in docs/design/orbit-core/4_decisions.md.

## North-star architecture bearing: operations as data behind an operation registry

**Recorded:** 2026-07-14 21:04:22.153730Z · [ORB-10026], [ORB-10200], [ORB-10358]

### Context
Orbit's four consumer surfaces — the CLI (`orbit-cli`), MCP (`orbit-mcp`), the web dashboard (`orbit-web`), and the in-runtime agent tool hosts — are four hand-wired adapter layers over the same underlying operations, so every new operation is plumbed by hand up to four times. The same shape keeps constraining refactors: inherent `impl OrbitRuntime` methods plus the orphan rule forced the ORB-10016 / [Extract the CLI-facing command layer into orbit-cmd](#extract-the-cli-facing-command-layer-into-orbit-cmd) orbit-cmd extraction to leave the runtime-entangled command groups behind in orbit-core as documented residuals, and the same wall shelved the docs+search pluginization (docs/design/orbit-docs-plugin/1_scope.md — pending commit). Repeated point refactors treat symptoms; the missing piece is a recorded long-term bearing that future refactors steer by. Real alternatives existed: keep the status quo and continue paying per-surface wiring, or mandate a big-bang plugin/microkernel rewrite.

### Decision
Record five bearings as orbit's north star. This is an **incremental bearing, not a rewrite mandate**: no code changes are required by this ADR, and existing code is not wrong for predating it.

1. **Operations as data, not inherent methods.** Every orbit operation is eventually defined as a serializable request/response pair with a handler registered in an operation table. The four consumer surfaces become derived adapters over that registry instead of four hand-wired layers, and the recurring inherent-impl/orphan-rule constraint ([Extract the CLI-facing command layer into orbit-cmd](#extract-the-cli-facing-command-layer-into-orbit-cmd) residuals; the shelved docs+search pluginization) dissolves because handlers are registry entries, not inherent methods on `OrbitRuntime`.
2. **Knowledge/execution split.** Orbit is two products — a knowledge store (tasks, learnings, ADRs, docs) and an execution engine (activities, jobs, agent providers) — glued by one runtime. Bearing: two systems sharing only a kernel (IDs, errors, audit), mirroring the constellation split (polaris = knowledge, worker = execution).
3. **Events over side-effects.** The task-mutation → semantic-index coupling becomes a transactional SQLite outbox consumed by the indexer, replacing the lossy in-process `EmbedWorker` enqueue (best-effort batches, drops on queue-full, debug-level failure logging).
4. **One retrieval trait, two backends.** orbit-search (workspace-local) and sextant (constellation-wide) become deployment choices behind one retrieval interface, dissolving the two-stack question.
5. **Crates follow build boundaries, not taxonomy.** Crate splits are justified by compile-graph and dependency-direction needs, not by conceptual category. Explicitly kept as-is under this bearing: the YAML+SQLite layered store in orbit-store, and the stability-tier markers (ARCHITECTURE.md §Stability tiers).

**Adoption model.** Incremental and opportunistic: when a new surface is added or an existing command group is touched for other reasons, move that slice to request/response + registry then. Future decisions should cite this bearing when steering by it, or supersede it if the bearing itself changes.

**Alternatives rejected.** (a) *Status quo as the implicit bearing*: keeps charging up to 4× adapter wiring per operation and guarantees the next boundary refactor hits the same inherent-impl/orphan-rule wall with no recorded direction — the cost that motivated this ADR. (b) *Big-bang registry rewrite*: months of churn across every surface with no incremental payoff and high regression risk in a codebase that ships continuously; rejected in favor of the touch-it-move-it model.

### Consequences
- Future architecture ADRs have a fixed point to cite or supersede; "which direction were we going?" is answerable from the store.
- Slices touched after this ADR should trend toward request/response + registry; reviewers can ask "why not the registry shape?" when a new hand-wired adapter appears.
- The knowledge/execution split (bearing 2) gives crate and module moves a destination: kernel-shared code is IDs/errors/audit only.
- The transition is unbounded by design, so registry-shaped and inherent-method slices will coexist indefinitely; the registry idiom is the tie-breaker, not a deadline.
- No single code anchor; convention enforced via review.
- Cost: two coexisting idioms during the (unbounded) incremental transition — readers must recognize both the registry shape and the legacy inherent-method shape — and request/response indirection adds per-operation boilerplate (a request type, a response type, a registration) compared to calling an inherent method directly.

## Pilot outcome — bearing 1, friction noun [ORB-10358]

**Status of bearing 1:** piloted and proven on one noun. Not yet applied to the
other three hand-copied nouns; that is the ratchet below, not a backlog item.

**What shipped.** Every friction verb (`add`, `list`, `show`, `stats`, `tags`,
`update`, `resolve`) is declared exactly once as an `OperationSpec` in
`orbit_common::friction::operations`. All four surfaces are now derived adapters
over that table: `orbit-tools` builds each `ToolSchema` and MCP exposure policy
from the spec (seven hand-written `Tool` impls deleted), `orbit-cli` builds the
clap subcommand tree and the tool input from the spec (seven `Args` structs and
seven `Execute` impls deleted), `orbit-web` takes its tool names and
parameter names from the registry, and `orbit-core` holds the handler half of
the table keyed on `FrictionVerb`. `OrbitBuiltinAction`'s seven `Friction*`
variants collapsed to one `Friction(FrictionVerb)`.

**Contract stability was proven, not asserted.** `crates/orbit-cli/tests/snapshots/mcp_tools_list.json`
is byte-unchanged, and `orbit friction [<verb>] --help` was captured from the
pre-migration binary and frozen as fixtures under
`crates/orbit-cli/src/command/tests/friction_help/`; the derived CLI reproduces
all eight help pages byte-for-byte.

**The layering correction to the bearing.** Bearing 1 as written implies one
table holding both the operation definition and its handler. That is not
achievable while handlers need `&OrbitRuntime`, which lives well above the leaf
crate every surface can read. The working shape is a **split table joined by a
typed verb enum**: the spec table is `&'static [OperationSpec<V>]` in
`orbit-common`, the handler table is one exhaustive `match` on `V` in
`orbit-core`, and the compiler rejects a verb that has a spec but no handler.
Future noun migrations should adopt that shape rather than trying to co-locate
handlers with specs.

**What did not become data, deliberately.** Response rendering stayed per-noun
(the CLI's friction table/record printers know friction field names), and
dashboard route shapes stayed hand-written — a REST path is an HTTP design
choice, not a property of the verb. The registry declares *which* rendering a
verb wants; the renderer itself is presentation.

**Touch-it-move-it ratchet.** The next feature that touches any hand-copied noun
migrates that noun to the registry as part of that work. "Touches" means adding
or removing a verb, changing a verb's parameters, or changing how any surface
wires that noun — not an unrelated edit inside an existing handler. A reviewer
seeing a new hand-wired adapter for a noun that could have been migrated should
ask for the migration or an explicit reason in the PR. Migration cookbook, with
the friction diff as the worked example: `docs/design/operations-as-data/`.

**Costs the pilot actually surfaced.**
- The registry is a wall of literal strings, and every one of them is shipped
  contract. This trades scattered-but-local duplication for centralized
  contract, which is the point, but it makes the registry file a high-blast-radius
  edit.
- Two descriptions per parameter where MCP and CLI wording legitimately differ
  (`show`'s id is `friction ID` over MCP and `Friction record id, e.g.
  F2026-05-001` on the CLI). The spec models this rather than forcing them to
  converge, because converging them would have been a wire change.
- `clap::Arg::value_name` takes only `&'static str`, so building a clap surface
  from runtime data requires interning. The adapter leaks a bounded number of
  short strings for the process lifetime.
- Derived clap `--help` output is only byte-stable because the adapter
  reproduces `#[derive(Args)]`'s conventions exactly (arg id, SCREAMING_SNAKE
  value name, declaration-order display). Any future noun migration must freeze
  its pre-migration help output as fixtures before starting, as this one did.

## Exact-id ADR restore is an operator CLI surface that repairs abandoned allocations

**Recorded:** 2026-08-01 19:25:22.376783Z · [ORB-10479], [ORB-10538]
**Paths:** `crates/orbit-cli/src/command/adr.rs`, `crates/orbit-store/src/file/adr_store/**`, `crates/orbit-store/src/sqlite/id_allocator/**`, `crates/orbit-tools/src/builtin/orbit/adr/**`

### Context

[ORB-10538] shipped `orbit.adr.restore`, the exact-id repair for an ADR whose allocation survived but whose body did not. Applying it to the 18 records in [ORB-10479] exposed two gaps between that contract and the population it was built for.

First, the tool was registered with `register_inactive` and a comment stating it stayed "available to `orbit tool run`". It did not: `orbit tool run` dispatches through `execute_tool_command_dispatch_*`, which gates on `ensure_tool_agent_facing`, and that rejects every inactive tool. With no CLI subcommand either, the tool had no reachable caller at all.

Second, `restore_allocated_adr` resolved the allocation through `adr_allocation`, whose SQL excludes `status = 'abandoned'` rows. But [ORB-10501]'s `abandon_orphaned` marks an allocation abandoned precisely when its pinned worktree is reaped — the dominant cause of the body loss in [F2026-07-163]. Four of the 18 ([Rank matched learnings by task-anchored decay-weighted upvotes](../project-learnings/4_decisions.md#rank-matched-learnings-by-task-anchored-decay-weighted-upvotes), [Default Claude to opus/sonnet CLI aliases; centralize model defaults in orbit-common::model_defaults](../agent-families/4_decisions.md#default-claude-to-opussonnet-cli-aliases-centralize-model-defaults-in-orbit-commonmodeldefaults), [PR handoff recovery follows job checkpoints and exact remote leases](../activity-job/4_decisions.md#pr-handoff-recovery-follows-job-checkpoints-and-exact-remote-leases), [Provider launchers resolve at the shared CLI spawn boundary](../activity-job/4_decisions.md#provider-launchers-resolve-at-the-shared-cli-spawn-boundary)) were in that state and were unrepairable by the tool built to repair them.

The alternatives for the second gap were to leave abandoned rows unrepairable and re-allocate fresh IDs for them (rejected by [ORB-10458]: the retired lookup had no ID-to-legacy fallback at citation sites, so every inline reference would stay broken), or to hand-edit `.orbit/`, which the repo agent guide forbids.

### Decision

Exact-id ADR restore is an operator surface reached through `orbit adr restore`, and it repairs abandoned allocations as well as live ones.

1. `orbit adr restore` is a CLI subcommand that calls `runtime.run_tool`, which bypasses `ensure_tool_agent_facing` while preserving every guard the tool enforces. This is the same bypass `orbit adr list` uses for the same reason ([ORB-00289]); registering a tool inactive is a statement about the *agent* surface, and any inactive tool that operators must still invoke needs a CLI subcommand to be reachable.
2. `restore_allocated_adr` resolves its allocation through `adr_allocation_for_restore`, which includes `abandoned` rows. This is sound because an abandoned row still owns its ID permanently — `max_sequence` counts abandoned rows, so the ID is never reissued and a restore into one cannot collide with a different record. Ordinary reads keep using `adr_allocation` and continue to hide abandoned rows.
3. A successful restore moves the allocation's `status` to `merged` inside the existing compare-and-set, because the repair has just written a readable body into the current worktree. The `WHERE` clause still pins the full pre-restore snapshot, so a concurrent change to any field — `status` included — still loses the race.

### Consequences

- The 18 [ORB-10479] narratives were restorable at their existing IDs, with no ID reallocated and no inline citation broken.
- Reviving the allocation keeps the invariant that a locally readable ADR has a live allocation row, so a later `resolve_adr_artifact` from another checkout reports `remote_artifact_unavailable` rather than `not_found`.
- The inactive-plus-CLI-subcommand pairing is now the established shape for operator-only tools; adding one without the subcommand ships an unreachable surface, which is what happened here.
- Cost: `restore_body_path_if_unchanged` now writes `status` as well as location, so it is no longer a pure relocation primitive. Any future caller that wants to move an allocation's body path *without* asserting the record is merged needs a separate function rather than reusing this one — the ADR-only `kind` guard is what keeps that blast radius small today.
- Cost: restore remains reachable only from a local CLI. Agent sessions and the MCP surface still cannot repair a lost body, so the repair depends on an operator noticing the loss; [F2026-07-163] stays open for the detection half of the problem.

## Publish superseded ADR bodies as durable decision history

**Recorded:** 2026-08-01 20:52:31.864062Z · [ORB-10545]
**Paths:** `.gitignore`, `.orbit/adrs/superseded/**`, `crates/orbit-store/**`, `crates/orbit-cli/**`, `crates/orbit-engine/**`

### Context
Superseded ADRs retain the rejected alternatives and constraints that explain current architecture. Ignoring their bundles made clean clones incomplete and created a recovery deadlock: exact-id restore refused a readable federated copy while guarded worktree GC correctly refused to delete its only copy.

### Decision
Superseded ADR bundles are published decision history and travel with the repository at their original IDs and supersession metadata. Proposed drafts remain unpublished and ignored. Orbit provides an operator reconciliation command that copies a complete byte-identical bundle from an explicitly named registered worktree into the current registered checkout without changing allocation ownership or lifecycle state; it validates source and destination identity, metadata and body completeness, destination absence or byte equivalence, and the allocation snapshot before the atomic publication rename.

### Consequences
- Clean clones retain accepted, superseded, and deleted decision history, including rejected alternatives.
- Rejected alternative: keep superseded bodies local-only and rely on design-doc summaries. This loses the authoritative body and recreates the guarded-GC deadlock.
- Rejected alternative: permit manual file copies or make exact-id restore overwrite a readable federated record. This bypasses validation or fabricates fresh metadata instead of preserving the published bundle.
- Cost: repository history and checkout size grow with every superseded ADR, and reconciliation adds locking and validation complexity to the operator CLI.

## Remove operation mode rather than keep an unused authorization layer

**Recorded:** 2026-09-21 · [ORB-12769], [ORB-12770], [ORB-12771], [ORB-12772]

### Context
Operation mode [ORB-11332] shipped presets (`supervised`/`autonomous`),
durable scoped grants with prepare/promote/complete rights, grant-bound
drains with atomic child rechecks, evidence-driven promotion, and aggregate
recovery budgets. None of it was used: the box store held no grant, no drain
ran grant-bound, and managed completion never ran outside the tests written
for it. What it did do was sit in the admission path of every child job run,
the handoff acceptance path, and the task-commit journal, and carry a second
vocabulary of authority beside the one Orbit actually uses (an operator
approving a handoff).

### Decision
Remove it in four layers at Daniel's request on 2026-09-21: the dashboard
panel and `/api/operation/*` [ORB-12769], the `orbit operation` CLI group and
MCP tools [ORB-12770], the Core application module and Common governance
module [ORB-12771], and finally the persisted surface — the preset-managed
`operation.*` config keys, the `operation_grants` and `operation_recovery`
tables, and the feature's design folder [ORB-12772]. The `[operation]` table
name stays for the review keys that were always independent of the preset;
the removed keys are warned about by name and ignored at config load so an
existing `config.toml` keeps working. A `done` ship contract, which only a
grant could authorize, now fails closed at handoff rather than being silently
downgraded to review.

### Consequences
- The child-admission transaction has one guard again (the admissions stop
  flag) instead of two, and handoff acceptance has one authority source.
- Rejected alternative: keep the feature dormant behind the default
  `supervised` preset. It costs nothing at runtime but keeps a second
  authorization model in the two most security-relevant transactions in the
  store, where a future reader must understand both to change either.
- Rejected alternative: keep the config keys as no-ops. The keys describe
  scheduling and completion behaviour that no longer exists; leaving them
  settable would state authority Orbit cannot honour.
- Cost: reinstating unattended operation means designing authority again from
  scratch — the grant shape, the atomic rechecks, and the recovery ledger are
  gone from the tree, recoverable only from history. The removal migration is
  breaking: an older binary reads `operation_grants` at handoff commit.

## Task References

- [ORB-10016] — extracted `orbit-cmd` from orbit-core, converted moved command groups to extension traits, and trimmed root re-exports.
- [ORB-10026] — authored the [North-star architecture bearing: operations as data behind an operation registry](#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry) north-star operation-registry architecture bearing.
- [ORB-10479] — restored the 18 design-doc ADRs whose allocation survived their body, and made the [ORB-10538] repair surface reachable and abandoned-allocation aware ([Exact-id ADR restore is an operator CLI surface that repairs abandoned allocations](#exact-id-adr-restore-is-an-operator-cli-surface-that-repairs-abandoned-allocations)).
- [ORB-10545] — made superseded ADR bodies repository-published history and
  added allocation-pinned federated reconciliation ([Publish superseded ADR bodies as durable decision history](#publish-superseded-adr-bodies-as-durable-decision-history)).
- [ORB-12769] — removed the dashboard panel and `/api/operation/*` routes, and decomposed the removal.
- [ORB-12770] — removed the `orbit operation` CLI group, `run auto --grant`, and the MCP tools.
- [ORB-12771] — removed the Core `application/operation` module and the Common governance module.
- [ORB-12772] — removed the config keys, the `operation_grants` table, and the design folder ([Remove operation mode rather than keep an unused authorization layer](#remove-operation-mode-rather-than-keep-an-unused-authorization-layer)).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
