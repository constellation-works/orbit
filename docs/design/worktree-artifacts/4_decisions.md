---
summary: "Worktree Artifacts - Decisions"
type: design
title: "Worktree Artifacts - Decisions"
owner: codex
last_updated: 2026-08-11
status: Accepted
feature: worktree-artifacts
doc_role: decisions
tags: ["worktree-artifacts"]
paths: ["crates/orbit-core/**", "crates/orbit-store/**", "crates/orbit-engine/**", "crates/orbit-cli/**"]
related_features: ["worktree-artifacts", "host-registry", "mcp-bridge"]
related_artifacts: ["ORB-00199", "ORB-00200", "ORB-00201", "ORB-10272", "ORB-10297", "ORB-10330", "ORB-10501", "ORB-10535", "ORB-10545", "ORB-10668", "ORB-10669", "ORB-10725"]
last_validated: 2026-09-12
---

# Worktree Artifacts - Decisions

> Entries about ADR/learning stores, allocation, federation, and publication are
> retired history. ORB-10726 retired the ADR store and tool surface, and
> ORB-10736 removed the native learning subsystem; current feature decisions live
> in each feature's `4_decisions.md`. The stationary-primary and delivery-summary
> decisions below describe current code paths.

Decision log for worktree artifact storage. Entries are addressed by title; task references retain their implementation provenance.

## Worktree-local ADR and learning bodies with shared ID allocation

**Recorded:** 2026-05-20 07:03:09.624062Z · [ORB-00201]

### Context
Linked worktrees need ADR and learning bodies committed with the code branch that created them, but IDs must remain collision-free across all worktrees. ORB-00199 introduced shared/local root resolution and ORB-00200 introduced the shared SQLite allocator. The remaining choice is whether body files follow the allocator into shared_root or follow the editing branch into local_root.

### Decision
Write ADR and learning body files under the current worktree local_root while keeping ID allocation, migration, and allocation metadata in shared_root/.orbit/state/semantic.db. Lists read through id_allocations: default output includes only locally readable bodies, while include-remote returns stubs that name the recorded worktree and branch.

### Consequences
- ADR and learning files can be staged in the same PR as the implementation that created them.
- Shared ID allocation still prevents cross-worktree collisions and records where each body lives.
- Readers get predictable defaults without failing on missing sibling-worktree files.
- Cost: list/show paths now carry a federation boundary and must handle body_path metadata, remote stubs, and stale worktree paths.

## Detect and retire id allocations pinned to a reaped worktree

**Recorded:** 2026-07-27 02:56:21.490547Z · [ORB-10501], [ORB-10535]
**Paths:** `crates/orbit-store/src/sqlite/id_allocator/**`, `crates/orbit-engine/src/executor/automation/vcs/worktree/cleanup.rs`, `crates/orbit-cmd/src/doctor.rs`, `crates/orbit-cli/src/command/doctor.rs`

### Context

Learning and ADR ids come from one shared SQLite allocator and are pinned to the worktree that allocated them, with the body written into that worktree ([Worktree-local ADR and learning bodies with shared ID allocation](#worktree-local-adr-and-learning-bodies-with-shared-id-allocation)). Lists model a body that is not readable here as a *remote stub*, which assumes the body still exists in some other checkout.

That assumption has no steady state. When a job-run worktree is reaped before its body was finalized and merged, the allocation row outlives every path that could resolve it: the row stays visible as `reserved`/`merged` forever, the body is unrecoverable, and nothing detects or prunes it. F2026-07-161 measured 35 of 113 allocated learning ids in `ws_orbit` as unreadable remote stubs (17 `reserved`, 18 `merged`), several pinned to worktrees confirmed gone from disk. The same pattern hit the unrecoverable task-artifact workspace-binding decision and the bodies now preserved as [MCP ambient workspace session context](../mcp-session-context/4_decisions.md#mcp-ambient-workspace-session-context), [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed), [Default Claude to opus/sonnet CLI aliases; centralize model defaults in orbit-common::model_defaults](../agent-families/4_decisions.md#default-claude-to-opussonnet-cli-aliases-centralize-model-defaults-in-orbit-commonmodeldefaults), and [PR handoff recovery follows job checkpoints and exact remote leases](../activity-job/4_decisions.md#pr-handoff-recovery-follows-job-checkpoints-and-exact-remote-leases). `learning sync` cannot help — it reconciles only from locally readable YAML (F2026-07-094 b).

The allocator already had `abandon_learning`/`abandon_adr`, but they were reachable only from create-rollback and refuse any row that recorded a body path, which is exactly what a stranded `merged` row has.

### Decision

Define an **orphaned allocation** as a row satisfying both conditions: its pinned `worktree_root` no longer exists on disk, *and* its body is unreadable both canonically and through the recorded `body_path`. Both are required — a live sibling worktree is an ordinary remote stub, and a canonically present body makes a stale `worktree_root` harmless.

Before automated cleanup removes a worktree, the shared removal path reads live allocation rows under the allocator lock and refuses when a body pinned to the target has no byte-identical readable copy in another registered worktree. The preflight applies to forced pipeline cleanup and ordinary GC alike and reports the affected IDs with reconciliation instructions (ORB-10535).

Orphans that already exist are reported through the `id-allocations` `orbit doctor` check and retired with the guarded `orbit doctor --fix-orphaned-allocations`. Repair flips `status` to `abandoned` rather than deleting the row: `max_sequence` counts abandoned rows, so a retired id is never reissued, and the row keeps its recorded worktree, branch, and `body_path` for forensics. The allocator repair entry point differs from the create-rollback `abandon` in accepting a `merged` row and in guarding on the missing worktree, re-checked inside the write transaction. The owning store re-verifies both orphan conditions immediately before each write, so a caller working from a stale scan cannot retire a recoverable id. Learning repair additionally drops the stale envelope index row, which is pinned to the same dead body.

### Consequences

- Automated cleanup fails closed before data loss, while a body already landed byte-for-byte in the canonical or another registered checkout remains eligible for cleanup.
- The prevention guard and ORB-10501 repair remain separate: bodyless reservations and already-missing worktrees are still doctor concerns rather than cleanup-time repairs.
- The permanently-orphaned class is detectable rather than inferred by hand-reading `learning list --include-remote`, and repairable without hand-editing `.orbit/` or the SQLite store.
- Retired ids stay consumed, so repair can never cause a collision with an id that was cited in a commit message or a doc.
- Deleting the row was rejected: it would let `max_sequence` reissue the id, and would destroy the only remaining record of where the body was written.
- Repair is opt-in behind an explicit flag; the check itself only warns, and an ordinary `orbit doctor` run mutates nothing.
- Cost: cleanup now takes the shared allocator lock across its preflight and destructive Git operation, so concurrent knowledge creation can briefly delay worktree collection.
- Cost: the orphan test remains duplicated per artifact kind in the owning stores because ADR and learning bodies resolve differently; lifting artifact-layout knowledge into the allocator would violate the boundary [Worktree-local ADR and learning bodies with shared ID allocation](#worktree-local-adr-and-learning-bodies-with-shared-id-allocation) draws.
- Cost: `worktree_root.exists()` is a liveness heuristic — a worktree on an unmounted volume reads as reaped. The refuse-on-recoverable guard and the opt-in flag bound the blast radius to a status flip that never touches a body file.

## Publish superseded ADR bodies as durable decision history

**Recorded:** 2026-08 - [ORB-10545]

Superseded ADR bundles, including their rejected alternatives and supersession
metadata, travel with the repository. Proposed drafts remain local-only. A
validated `orbit adr reconcile` operator path copies an existing complete
federated bundle byte-for-byte into the current registered checkout without
allocating a new ID or changing lifecycle/allocation metadata. The full
narrative and rejected alternatives are preserved in [Publish superseded ADR
bodies as durable decision history](../orbit-core/4_decisions.md#publish-superseded-adr-bodies-as-durable-decision-history).

## Publish every ADR lifecycle partition and resolve duplicates by explicit precedence

**Recorded:** 2026-08-09 06:44:59.988414Z · [ORB-10669]
**Paths:** `crates/orbit-cli/src/command/workspace/**`, `crates/orbit-store/src/file/adr_store/**`, `.gitignore`

### Context

[Publish superseded ADR bodies as durable decision history](#publish-superseded-adr-bodies-as-durable-decision-history) (ORB-10545) published superseded ADR bundles but kept `proposed/`
local-only, continuing ORB-10303. Two costs followed. The decision under review
was invisible in the PR that motivated it, so a reviewer could not read the
draft alongside the change. And every ADR authored inside a managed job-run
worktree was stranded on the box: the draft lived in an ignored directory that
died with the worktree unless an operator ran `orbit adr reconcile` first.

The ignore policy is generated, not hand-maintained. `ORBIT_GITIGNORE_BLOCK` in
`crates/orbit-cli/src/command/workspace/support.rs` is the managed block that
`orbit workspace init` writes and rewrites into every workspace, and it still
ignored both `.orbit/adrs/proposed/` and `.orbit/adrs/superseded/` — the latter
already contradicting [Publish superseded ADR bodies as durable decision history](#publish-superseded-adr-bodies-as-durable-decision-history). A hand-edited `.gitignore` was therefore
reverted on the next init or re-register.

Tracking the proposed partition also makes a latent ambiguity reachable.
Acceptance is a directory rename from `proposed/<id>` to `accepted/<id>`. With
both partitions tracked, a branch cut before acceptance still carries the
proposed bundle; merging it re-adds that directory next to the accepted one, and
because the two paths are unrelated git merges both without a conflict.
`locate_adr` resolved by scanning `AdrStateDir::all()` in declaration order —
proposed first — and returning the first hit with no duplicate detection, so the
stale draft would mask the accepted record and the ADR would silently read as
proposed again.

### Decision

Publish every ADR lifecycle partition. `proposed/`, `accepted/`, `superseded/`,
and `deleted/` all travel with the repository; only the rebuildable
`adrs/index.sqlite*` and the host-local `*.lock` files stay ignored. The managed
block carries this to every workspace and additionally *retires* the two ignore
lines that older blocks wrote. Retirement is load-bearing rather than cosmetic:
`!.orbit/adrs/` re-includes only the `adrs` directory itself, so a surviving
`.orbit/adrs/proposed/` above the appended block would still be the last pattern
matching that subdirectory and would keep the partition ignored. Stripping it is
what makes re-init converge on the current policy instead of preserving the old
one, without duplicating or stacking blocks.

Resolve a duplicated ID by one explicit, documented precedence: the
most-advanced lifecycle state wins, ranked `proposed` < `accepted` <
`superseded` < `deleted`. This is sound because every sanctioned transition
moves forward and `accepted -> proposed` is rejected outright, so the
lower-ranked copy is always the stale one. `AdrStateDir::lifecycle_rank` states
the ranking; `AdrStateDir::all()` is documented as scan order carrying no
resolution meaning. `locate_adr` collects every partition hit before choosing,
`list_adrs` collapses duplicates under the same rule so a stale draft cannot
double-count in a listing or race the accepted row into an index rebuild, and
each shadowed partition is named with its path in a `warn` log so the leftover
is observable and removable.

`orbit adr reconcile` keeps its stricter contract: a source checkout holding
more than one lifecycle artifact for an ID is refused, not resolved. Federated
reconciliation, artifact ownership, and the `artifact_not_local` guard are
unchanged — this decision governs publication, not ownership.

Drafts written in a managed job-run worktree get a defined disposition: they are
tracked, so the run's auto-commit sweeps them onto that branch and they ride the
PR. A run that is abandoned or rejected takes its draft with the branch — no
operator cleanup, no reconciliation, and the unused ID allocation is an ordinary
valid gap, not the orphaned-allocation condition ORB-10501 repairs.

### Rejected alternatives

- **Fail the read on a duplicate.** An error is more obviously deterministic,
  but it bricks `orbit adr show` at exactly the moment an operator needs it —
  immediately after a merge — and offers no path forward except manual
  filesystem surgery. Precedence plus a warning is deterministic *and*
  recoverable, and it still surfaces the leftover. Reconcile keeps the strict
  behavior where the operand set is under operator control.
- **Reorder `AdrStateDir::all()` so the accepted partition scans first.** This
  fixes the one reachable pair by moving the implicitness rather than removing
  it; the next reader still cannot tell that declaration order is load-bearing,
  which is the defect.
- **Sweep proposed drafts out of job worktrees before commit.** Deleting drafts
  in the run's worktree would destroy exactly the artifact publication is meant
  to preserve, and cannot distinguish an abandoned run from one whose PR is
  about to merge.
- **Only remove the two lines from the block, without a retirement list.**
  Existing checkouts would keep their old lines ahead of the appended block and
  never converge, so every already-initialized workspace would silently retain
  the old policy.

### Consequences

- A proposed ADR is reviewable in the PR that motivates it, and an ADR authored
  in a managed worktree lands on its own branch without an operator step.
- The shipped block now matches the accepted publication policy for every
  partition, including the `superseded/` line [Publish superseded ADR bodies as durable decision history](#publish-superseded-adr-bodies-as-durable-decision-history) had already invalidated.
- Re-init over a checkout carrying an older managed block converges on the
  current policy exactly once, with no stacked or duplicated block.
- A merged stale draft can no longer mask an accepted record on read, in a
  listing, or in an index rebuild, and it is reported rather than silent.
- Cost: `list_adrs` now accumulates through a keyed map instead of appending
  during the walk, so its output is ordered by ID rather than by partition scan
  order. Callers that need another order already sort explicitly.
- Cost: drafts from abandoned runs accumulate as dead objects in unmerged branch
  history. They are unreachable once the branch is deleted, but they are not
  actively pruned.
- Cost: the precedence is a repair, not a prevention. A duplicate still reaches
  the working tree on merge, and clearing the warning means deleting the stale
  directory by hand.

## orbit adr owns ADR authoring and lifecycle; reconcile stays the cross-checkout verb

**Recorded:** 2026-08-09 07:59:21.875343Z · [ORB-10668]
**Paths:** `crates/orbit-cli/src/command/adr/**`

### Context

`orbit adr` exposed only read/repair verbs (`list`, `show`, `restore`, `reconcile`). Authoring an ADR and moving one through its lifecycle existed only on the tool surface (`orbit tool run orbit.adr.add` / `orbit.adr.update`) and over MCP.

The gap bit hardest exactly where those surfaces are unavailable. An ADR authored inside a job worktree is federated relative to the hub, so a bridge/MCP write against it is refused with `artifact_not_local` (409) and can only succeed from the owning worktree. That leaves the operator on-box, inside that worktree, at a shell — and `orbit adr update <id> --status accepted` did not exist. Encountered 2026-08-08 accepting [Classify independent-review startup separately from reviewer rejection](../activity-job/4_decisions.md#classify-independent-review-startup-separately-from-reviewer-rejection) in `orbit-jrun-20260808-2029-5`.

The open question ORB-10668 raised was whether `orbit adr reconcile` was already the intended answer, making this a discoverability defect rather than a missing verb.

### Decision

It is a missing verb, and `reconcile` addresses a different case.

1. Add `orbit adr add`, `orbit adr update`, and `orbit adr supersede`. Each is a thin `runtime.run_tool` delegation to the matching `orbit.adr.*` tool. The tool surface remains the single implementation of ADR semantics: ID allocation, the `proposed -> accepted` related-task rule, the refusal of direct `superseded` writes, the managed-run executor restriction, and the `artifact_not_local` federation guard all stay there. The CLI shapes argv into tool input and renders the response; it re-derives no rule.

2. `reconcile` is **not** the answer for the reported case. In the owning worktree the ADR resolves as `Local`, so `orbit adr update` succeeds directly and reconciling would be a no-op detour that also moves the bundle out of the checkout that owns it. `reconcile` remains the answer for the other direction — mutating an ADR *from* a checkout that does not own it, where the bundle must be brought in first.

3. The discoverability half is still real, so it is fixed as help text rather than as behavior: `orbit adr update --help` states the lifecycle transitions, and names `artifact_not_local`, the `artifact_origin` worktree, and the `reconcile` escape hatch — so the federated path is reachable from the CLI's own help instead of from a 409.

4. `command/adr.rs` becomes `command/adr/` (the documented parent-command directory shape in `crates/orbit-cli/CLAUDE.md`), one file per subcommand plus `support.rs`. At seven verbs the single file was already the largest under `command/`.

### Consequences

- The federated ADR path is completable with `orbit adr` alone from the owning worktree; no `orbit tool run`, no hand-edited `.orbit/adrs/`.
- Locality enforcement is untouched: the CLI never resolves artifacts itself, so a non-local target still fails closed with `artifact_not_local` and the full `artifact_origin` payload. A regression test in `crates/orbit-cli/tests/worktree_resolution.rs` pins both halves.
- Two surfaces now reach the same tools (CLI and MCP), so an `orbit.adr.*` schema change must consider both. That is already true of `list` and `restore`.
- `--status` is passed to the tool as an unparsed string rather than a clap `value_enum`, matching the existing `adr list --status` filter. Status vocabulary stays defined in one place; the cost is that an invalid value is reported by the tool rather than by clap.
- Cost: the CLI's ADR surface grows from four verbs to seven, and the directory split moves ~320 lines, so `git log --follow` on the old `command/adr.rs` path needs rename detection.

## Alternatives rejected

- **Treat it as pure discoverability and only document `reconcile`.** Rejected: it prescribes a bundle move for a case where the ADR is already local, and still leaves no CLI verb for the mutation itself.
- **Reimplement the lifecycle rules in the CLI for better clap ergonomics.** Rejected: it duplicates the `proposed -> accepted` and supersession rules across two surfaces that would then drift.
- **Relax the federation guard so the hub can write a non-local ADR.** Rejected outright: the guard is the reason a federated bundle stays committable from exactly one checkout.

## Stationary primary HEAD tolerates record-store dirt only

**Recorded:** 2026-07-27 01:18:25.906846Z · [ORB-10493], [ORB-10471]
**Paths:** `crates/orbit-engine/src/activity_job/workspace.rs`

### Context

`WorktreeIntegrityGuard::verify` compares the registered primary checkout before and after a provider invocation. Until now it had exactly one benign case: `primary_fast_forward_is_benign`, which accepts a proven same-branch fast-forward whose dirt does not intersect `run_changed_paths` (ORB-10471). That helper rejects `before.head == after.head` on its first clause, so a primary that never moved but merely gained or lost dirt was always fatal.

F2026-07-166 is the cost: run `jrun-20260726-2223-8` (ORB-10467) lost a complete, validated 13-file implementation because an out-of-run learning-curation pass re-serialized 12 already-tracked `.orbit/learnings/*/learning.yaml` files in the primary while its HEAD and branch stood still. `conflicting_paths` was empty; the guard raised a non-retryable `primary_checkout_drift` anyway and burned the step's entire `max_attempts=1` budget. This dirt class is endemic, not exceptional: Orbit's own engine and curation passes rewrite the primary's `.orbit/` records continuously while pipelines run.

The obvious fix — reuse the fast-forward rule verbatim, accepting any stationary-HEAD delta disjoint from `run_changed_paths` — is unsafe, and the existing suite proves it. Five tests (`primary_escape_is_typed_non_retryable_and_preserves_both_checkouts`, `primary_content_mutation_is_typed_even_when_assigned_content_also_changes`, `staged_only_primary_delta_reports_its_path_and_index_identity`, `unchanged_pre_dirty_path_is_excluded_from_escape_diagnostic`, `primary_escape_is_checked_after_nonzero_exit_and_timeout`) encode the ORB-10134 data-loss hazard: a provider that escapes its assigned worktree and edits the primary. Every one of those escapes is disjoint from the run's own paths — `primary_content_mutation_is_typed_even_when_assigned_content_also_changes` is named for exactly that shape — so a pure disjointness rule silently accepts all five.

The asymmetry is evidential. A fast-forward carries positive proof that Git itself produced the delta (`merge-base --is-ancestor` over a moved HEAD). A stationary HEAD carries no such proof: at the Git layer, a curation pass rewriting a record and a provider stray-writing a source file are indistinguishable by authorship. Only the *path class* separates them.

### Decision

Add a second benign case, `primary_dirt_only_delta_is_benign`, that is deliberately narrower than the fast-forward rule. A stationary-HEAD primary delta is accepted only when all of the following hold:

1. `before.head == after.head` and `before.branch == after.branch`;
2. the delta is fully explained by dirt-path movement (`primary_dirt_mutations` is non-empty) — an unattributable fingerprint change falls through and fails closed;
3. no mutated path intersects `run_changed_paths` (the same gate ORB-10471 introduced);
4. **every** mutated path lives under `.orbit/`, Orbit's own record store.

Clause 4 is the load-bearing addition. `.orbit/` holds tasks, frictions, auto-tasks, and routines: content the engine driving the pipeline rewrites as a matter of course, and content that is never a run's code candidate. A primary mutation anywhere else — source, manifests, CI config — remains `primary_checkout_drift` regardless of disjointness, preserving ORB-10134 escape detection intact.

Acceptance is logged at `info` on `orbit.engine.cli_runner` with the ignored paths, matching the fast-forward case. The guard never cleans or reconciles the dirt it ignores.

### Consequences

- The F2026-07-166 class of loss is closed: concurrent record-store curation can no longer strand a validated implementation.
- Provider-escape detection is unchanged. All five ORB-10134 escape tests still fail closed, and `stationary_primary_source_edit_stays_fail_closed_even_when_disjoint` pins the divergence so a future "simplification" to pure disjointness fails loudly.
- The two benign cases now apply asymmetric rules, which is a genuine complexity cost to carry.
- **Cost:** clause 4 is a path-prefix heuristic, not a proof of authorship. A benign out-of-run pass that touches a primary file *outside* `.orbit/` still fails the guard, and a provider that escapes to write inside the primary's `.orbit/` is now tolerated. Both are chosen deliberately: the first errs fail-closed on the guard's own hazard, and the second is a records-not-code blast radius that the run's own record writes already produce. If either turns out to matter, the follow-up is authorship evidence (e.g. fingerprinting per-path mtime or an engine-owned write ledger), not a wider prefix list.
- **Cost:** this narrows the literal fix ORB-10493 proposed ("compare only the dirt paths that intersect `run_changed_paths`"). Its first acceptance criterion is met for the friction's actual repro, which is a record-store delta, but not for an arbitrary disjoint tracked file. Rejected alternative: implement the criterion literally and rewrite the five escape tests — that trades an intermittent, recoverable failure for a silent data-loss regression, which is the wrong direction on this guard.

## Derive the delivery execution summary from the change, not from the agent

**Recorded:** 2026-08-08 19:39:31.685834Z · [ORB-10603]
**Paths:** `crates/orbit-engine/src/executor/automation/vcs/commit/**`

### Context

Delivery refuses to hand off a task whose durable `execution_summary` is empty: `reject_failed_delivery` rejects an empty or placeholder summary before the commit step touches the index (ORB-10313), and `update_task_with_status_note_and_identity` refuses the `in-progress -> review` transition on the same grounds. The summary is real evidence — the PR body renders it, and reviewers read it.

Nothing in the pipeline ever wrote that field. The deterministic `update_task` action hardcodes `execution_summary: None`, and the only writer was instruction 14 of the `agent_implement` activity, prose asking the implementing agent to persist one. Agents skip it often, and every run that skipped it wedged at commit with a change sitting uncommitted in the worktree.

### Decision

The commit step derives the summary from the change it is about to deliver, and only when durable state carries none.

1. `commit_batch_changes` calls `ensure_durable_execution_summary` after read-only checkout resolution and validation, and before the delivery gate. It no-ops when `meaningful_execution_summary` already finds one, so an agent-authored summary always wins.
2. The derived text is read out of `git status --porcelain=v1 --untracked-files=all -z` in the delivery worktree — the same file set `git add --all` will stage — and names each path with its change kind, capped at 25 entries plus a remainder count. It claims no outcome, only what the diff shows.
3. It is persisted to the task record through `apply_task_automation_update` with a `execution_summary_derived` event, so it is durable before any Git mutation and re-checkable afterwards with `git show --stat` on the delivery commit.
4. When there is no change to describe, nothing is derived and nothing is persisted; the gate rejects as before.

The gate's contract is untouched. What changed is that its rejection is no longer reachable in the ordinary case.

### Rejected alternatives

- *Lift the summary out of the agent's returned envelope.* It would satisfy the gate with one line of code, and it is a doctrine violation (L-0115): agent-loop output is advisory, the runner states provider output is not the system of record, and the activity's own instruction says the returned object is not persisted. A pipeline decision must not read it.
- *Relax the guard on the local path.* Deletes the evidence rather than producing it. Downstream consumers, the PR body included, read this field.
- *Derive in the `update_task` action at `mark_review`.* Too late: commit gates first, so the run still wedges before delivery, and that action would then be in the business of authoring task content.
- *Derive after staging, from `git diff --cached --numstat`.* Gives line counts, but only by mutating the index before the delivery gate — the exact ordering ORB-10313 established.

### Consequences

- A task that has been through implementation and commit carries a non-empty summary in durable state whether or not the agent wrote one, so delivery, the `in-progress -> review` transition, and the PR body all have their evidence.
- The `agent_implement` instruction to persist a real summary still stands and still produces the better artifact; the derived one is a floor, not a replacement.
- Checkout resolution and branch/merge validation now run before the delivery gate, since the derived summary reads the worktree the gate protects. Nothing ahead of the gate mutates Git state.
- `Cost:` a derived summary describes the shape of a change, not its intent. A PR whose body carries one tells a reviewer which files moved and nothing about why, which is weaker than an agent-authored account and could be mistaken for one if the opening line is not read.
- `Cost:` the parser is coupled to `git status --porcelain=v1 -z` record framing, including the rename/copy source field that follows its record.
- `Cost:` a task tagged `no-diff-expected` with an empty summary still has nothing to derive from and still fails the gate, unchanged from before this decision.

## Publish every ADR state partition, proposed drafts included

**Recorded:** 2026-08-09 04:39:56.163296Z · [ORB-10669]
**Supersedes:** [Publish superseded ADR bodies as durable decision history](#publish-superseded-adr-bodies-as-durable-decision-history)
**Paths:** `.gitignore`, `.orbit/adrs/**`, `crates/orbit-cli/src/command/workspace/support.rs`, `crates/orbit-store/src/file/adr_store/**`

### Context
[Publish superseded ADR bodies as durable decision history](#publish-superseded-adr-bodies-as-durable-decision-history) published accepted, superseded and deleted bundles but kept proposed drafts local-only, reasoning that an unaccepted draft is not yet decision history. In practice the proposed partition is where every ADR authored inside a managed job worktree first lands, so the decision under review is invisible in the pull request that motivates it, review happens against a bundle only the box can read, and promotion requires an operator reconcile step. The real alternative was to keep drafts ignored and treat reconciliation as the publication path, accepting the review blind spot as the price of a history free of abandoned drafts.

### Decision
All four ADR state partitions — proposed, accepted, superseded and deleted — are tracked and travel with the repository; only the rebuildable SQLite index and lock files stay ignored. The managed `.gitignore` block written by `orbit workspace init` is the single expression of that policy for every workspace, not a per-checkout edit. Because a tracked draft can be re-added by a merge after acceptance, ADR resolution must no longer let a stale `proposed/` bundle mask a more advanced state for the same ID.

### Consequences
- A proposed ADR is reviewable in the change that introduces it, and promotion to accepted is an ordinary tracked rename rather than an on-box operator step.
- Reconciliation from [Publish superseded ADR bodies as durable decision history](#publish-superseded-adr-bodies-as-durable-decision-history) remains the mechanism for adopting a federated bundle into another checkout, but publication no longer depends on it.
- Duplicate-ID resolution becomes load-bearing rather than unreachable: resolution currently returns the first match scanning proposed-first, so lifecycle precedence must be made explicit or a duplicate must be surfaced as an error.
- Drafts written inside managed job worktrees become tracked files in the run's branch, so abandoned and rejected runs can leave proposed bundles behind and need a defined disposition.
- Cost: the repository accumulates decisions that were never accepted, including drafts from failed runs, and readers must treat `proposed/` as a slush pile rather than as decisions the project stands behind.

## Force-stage run-allocated proposed ADR bundles at delivery

**Recorded:** 2026-08-09 06:43:36.172167Z · [ORB-10653]
**Paths:** `crates/orbit-engine/src/executor/automation/vcs/commit/**`

### Context
Workspace init keeps `.orbit/adrs/proposed/` gitignored because proposed drafts are local-only until publication, so `git add --all` in the delivery step silently skips a draft documenting the very code being shipped. The implementing agent cannot close the gap itself: in a linked run worktree `.git` points at the main checkout's worktree metadata, which is bound read-only for the sandboxed implementer, so taking `index.lock` fails. Two alternatives were real — un-ignoring the proposed partition (rejected: local-until-publication is a deliberate policy, and it would publish every speculative draft), and leaving the gap (rejected: it either drops the decision or tempts an executor into fabricating an ADR id).

### Decision
The unsandboxed commit step force-stages exactly the ignored `proposed/*/{adr.yaml,body.md}` bundles present in the delivery worktree, before `git add --all`, verifies each landed in the index, and otherwise refuses delivery with a diagnostic naming the bundle and the supported host-side staging path.

### Consequences
- A proposed ADR allocated during a run ships in the same commit as the code it documents, without any change to the gitignore policy or to the accepted and superseded partitions.
- Discovery uses `git check-ignore --stdin`, which answers from ignore rules without locking the index, so it still works when worktree metadata is read-only and can report that condition precisely.
- Cost: delivery now fails closed on an unstageable draft, so a genuinely read-only checkout blocks the commit until an operator stages the bundle host-side; a refused commit is accepted as strictly better than a dropped decision or an invented id.

## Task References

- [ORB-00199] introduced shared/local root resolution.
- [ORB-00200] introduced shared ID allocation and `L-NNNN`.
- [ORB-00201] implemented this decision.
- [ORB-10297] amended the ADR show and mutation boundary with four-state resolution and typed origin/error payloads.
- [ORB-10272] amended the allocation boundary with the dormant Remote-v2 hub-global
  sequence, full legacy reconciliation, immutable correlation ledger and atomic
  audit while retaining standalone compatibility and owner-local bodies.
- [ORB-10330] added the owner-side preallocated finalizers and the gated broker
  composition that consume a hub allocation into the exact owner checkout — one
  hub allocation, one owner finalization, correlated by `mcp_call_id`, with
  replica/foreign-spoke rejection before allocation and no local sequence advance.
- [ORB-10501] added detection and guarded repair for allocations whose pinned
  worktree was reaped, closing the steady-state gap the remote-stub model left
  open.
- [ORB-10535] added the shared pre-removal guard that prevents cleanup from
  creating that orphaned state when the target still holds the unique body.
- [ORB-10545] added federated ADR reconciliation, published superseded bodies,
  and resolved the guarded-cleanup deadlock under [Publish superseded ADR bodies as durable decision history](#publish-superseded-adr-bodies-as-durable-decision-history).
- [ORB-10669] published the remaining ADR partitions, made the shipped
  `.gitignore` block retire its own superseded lines so re-init converges, and
  replaced first-hit-wins resolution with the explicit lifecycle precedence
  under [Publish every ADR lifecycle partition and resolve duplicates by explicit precedence](#publish-every-adr-lifecycle-partition-and-resolve-duplicates-by-explicit-precedence).
- [ORB-10668] added the `orbit adr add` / `update` / `supersede` CLI verbs so the
  owning worktree can complete the lifecycle without `orbit tool run`, under
  [orbit adr owns ADR authoring and lifecycle; reconcile stays the cross-checkout verb](#orbit-adr-owns-adr-authoring-and-lifecycle-reconcile-stays-the-cross-checkout-verb).

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
