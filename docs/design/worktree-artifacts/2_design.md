---
summary: "Worktree Artifacts - Design"
type: design
title: "Worktree Artifacts - Design"
owner: codex
last_updated: 2026-08-15
last_validated: 2026-09-12
status: Accepted
feature: worktree-artifacts
doc_role: design
tags: ["worktree-artifacts"]
paths: ["crates/orbit-core/**", "crates/orbit-store/**", "crates/orbit-cli/**"]
related_features: ["worktree-artifacts", "host-registry", "mcp-bridge"]
related_artifacts: ["ORB-00199", "ORB-00200", "ORB-00201", "ORB-10272", "ORB-10297", "ORB-10330", "ORB-10545", "ORB-10668", "ORB-10669", "ORB-10725"]
---

# Worktree Artifacts — Design

> Learning-specific storage and federation references below are retired history.
> [ORB-10736] / [Remove the native project-learning subsystem](../project-learnings/4_decisions.md#remove-the-native-project-learning-subsystem) remove the native learning subsystem and leave its
> existing repository files inert.

> Decision-store storage, federation, allocation, and repair references below are
> also retired history. [ORB-10726] retired the tool surface and moved reasoning
> into feature decision docs; [ORB-10805] removed the redundant tracked store and
> its IDs.

The historical implementation treated decision and learning bodies as branch-local files with workspace-local IDs (the workspace-scoped knowledge-key model from [ORB-10725]). The root split remains relevant to task execution, while the artifact-specific mechanisms below document retired behavior.

## 1. Runtime Roots

`OrbitRuntime` carries `shared_root` and `local_root`. On the main checkout they are equal. In a linked worktree, `shared_root` points to the main checkout `.orbit/`, and `local_root` points to the linked worktree `.orbit/`.

Explicit `--root` and `ORBIT_ROOT` overrides pin both roots to preserve the old single-root mental model when the operator asks for it.

## 2. Allocation Metadata

In the retired standalone/worktree implementation, `id_allocations` lived in
`shared_root/state/semantic.db`. The allocator serializes ID creation with a
shared lock, then body writes update the row with:

- `worktree_root`: the recorded worktree root for the body.
- `branch`: best-effort current branch.
- `body_path`: the body file path relative to `worktree_root`.

Backfilled shared-root artifacts received `body_path` during allocator initialization so old ADRs and migrated learnings remained readable from any worktree. ORB-10736 removed this allocator and the native learning projections; current store migrations explicitly drop `id_allocations`, while any old `.orbit/learnings/` files remain inert historical data.

The retired design had one allocator, and every create path used it. Under [ORB-10725],
knowledge used `(workspace_id, artifact_key)` as its key, so an ID was unique within its workspace
and made no claim outside it; [ORB-10725] deleted the hub-global sequence that
§2.1 and §2.2 once described.

### 2.1 The withdrawn hub-global sequence substrate

[ORB-10272] added Remote feature migration v2: dormant hub-global ADR and learning
sequences in the hub's config-resolved `orbit.db`, per-workspace reconciliation
state, an immutable `mcp_call_id` allocation ledger, and a dormant/active authority
marker. [ORB-10330] added the owner-side `finalize_preallocated` paths and the
gated broker composition that paired one hub allocation with one owner-checkout
finalization, correlated by `mcp_call_id`.

**Both are removed** ([ORB-10725]). Public issuance never activated, so
no ID was ever drawn from the sequence and nothing had to be renumbered; what the
substrate encoded was a superseded model, which is why it was deleted rather than
parked alongside the registry tables that [V1 has no fleet control plane](../host-registry/4_decisions.md#v1-has-no-fleet-control-plane) keeps dormant for v2. Remote
feature v2 keeps its ledger slot — feature-migration names are immutable, so a
database that recorded it must still find it — but the slot is a no-op, and Remote
feature v3 drops the tables `IF EXISTS` so a database that applied the original v2
converges on the same shape as a fresh one.

The retired design left the shape §2 described: one workspace-local allocator per
workspace, reached only by the owning machine. `orbit.learning.add` and
`orbit.adr.add` were single owner-local transactions — no reservation, no expiry,
no orphaned ID, no finalize/pull race — and the [ORB-10364] authoring role gate
sat on that one surface with nothing at stake on refusal.

## 3. Write Path

In the retired implementation, ADR creation wrote `adr.yaml` and `body.md` under `local_root/adrs/proposed/ADR-NNNN/`. Learning creation wrote `learning.yaml`, `votes.jsonl`, and `comments.jsonl` under `local_root/learnings/L-NNNN/`.

The first write into a linked worktree creates only the subtree needed for the artifact type. It does not scaffold local `state/`, `audit/`, `tasks/`, scoreboards, or registry files.

## 4. Read Federation

In the retired implementation, ADR `show` resolved the envelope and body together in the store and carried exactly one of four states through Core, HTTP, and local MCP:

1. `local`: a structurally complete, non-empty bundle exists under the current local root. This wins over any readable sibling allocation, and the read does not rewrite the allocation row.
2. `federated`: no current-local bundle exists and the allocation resolves to a structurally complete, readable sibling bundle.
3. `remote_artifact_unavailable`: an allocation exists, but its `body_path`, `adr.yaml`, or `body.md` is absent, unreadable, inconsistent, or empty.
4. `not_found`: neither a current-local bundle nor an allocation exists.

Successful local and federated reads preserve the ADR envelope, return the exact non-empty `body`, and add:

```json
{
  "artifact_origin": {
    "mode": "local | federated",
    "worktree_root": "credential-safe path",
    "branch": "string or null"
  }
}
```

`mode` has no other values. Public origin payloads never include `body_path`, URLs, bearer material, or credentials. HTTP maps `remote_artifact_unavailable` to 409 while retaining its string `error`; local MCP uses the same stable code in its structured error. An unknown ID remains HTTP 404 / MCP `not_found`.

List and search retain their existing defaults. Readable allocation-owned bundles participate as before; unreadable rows are omitted unless `include_remote` requests the existing stub shape.

## 5. Mutation Boundary

In the retired implementation, ADR document update, accept, and supersede were local-only. A federated or unavailable allocation-owned artifact failed preflight with `artifact_not_local` (HTTP 409 or the same local MCP code) before any bundle, allocation, lifecycle timestamp, index, or audit mutation. Supersede preflighted both operands before its first write. Landing the bundle in the current checkout restored ordinary local mutation semantics; a sibling-owned allocation row remained unchanged.

In that retired design, local-only was a boundary on *where* the write ran, not
on which surface could run it. `orbit adr add`, `orbit adr update`, and `orbit adr
supersede` ([orbit adr owns ADR authoring and lifecycle; reconcile stays the
cross-checkout verb](./4_decisions.md#orbit-adr-owns-adr-authoring-and-lifecycle-reconcile-stays-the-cross-checkout-verb),
[ORB-10668]) delegated to the same `orbit.adr.*` tools and inherited this
preflight unchanged.

### 5.1 Federated ADR reconciliation

The retired `orbit adr reconcile <id> --source-worktree <path>` command localized an already
published ADR bundle from an explicitly named registered sibling worktree. It
does not allocate, reconstruct metadata, change lifecycle state, or repoint the
allocation row. The store requires both checkouts to appear in the same Git
worktree registry, reads a complete non-empty bundle whose metadata status
matches its lifecycle partition, and accepts an existing destination only when
both files are byte-equivalent.

In the retired design, the source and destination ADR locks covered validation, while the shared
allocator lock pins the complete allocation snapshot across the final atomic
rename. A changed source, allocation, incomplete bundle, lifecycle mismatch,
unregistered checkout, or divergent destination fails before the canonical
destination changes. This is deliberately distinct from `adr restore`: restore
authors a replacement only when no readable copy exists; reconciliation
preserves a readable published bundle verbatim.

## 6. Publication Policy and Duplicate Partitions

In the retired implementation, every ADR lifecycle partition — `proposed/`, `accepted/`, `superseded/`,
`deleted/` — is tracked by git and travels with the repository [ORB-10669].
Publishing `proposed/` puts the decision under review in the same PR as the
change that motivates it, and means an ADR authored inside a managed job
worktree lands on that run's branch instead of stranding on the box until an
operator reconciles it. Only the rebuildable `adrs/index.sqlite*` and the
host-local `*.lock` files stay ignored. The managed `.gitignore` block that
`orbit workspace init` writes carries this policy to every workspace; re-init
over an older block retires that block's `proposed/` and `superseded/` ignore
lines rather than leaving them to out-rank the appended re-include.

Publishing every partition makes a duplicate ID reachable. Acceptance is a
directory rename, so a branch cut before acceptance still carries
`proposed/<id>`; merging it re-adds that directory next to `accepted/<id>`, and
git merges both without a conflict because the two paths are unrelated.
Resolution follows one explicit precedence: **the most-advanced lifecycle state
wins** (`proposed` < `accepted` < `superseded` < `deleted`), because every
sanctioned transition moves forward and `accepted → proposed` is rejected
outright — so the lower-ranked copy is always the stale one. `get_adr`, every
mutation preflight, and `list_adrs` share that precedence, and each shadowed
partition is named in a `warn` log with its path, so the leftover is visible and
removable rather than silently deciding the read. This replaces the implicit
precedence that fell out of partition declaration order. `orbit adr reconcile`
keeps its stricter contract: a source checkout holding more than one lifecycle
artifact for an ID is refused, not resolved.

### 6.1 Managed job-worktree drafts

In the retired implementation, a managed job-run worktree scaffolded a real `.orbit/adrs/proposed` for its run.
Drafts written there were tracked, so the on-box auto-commit swept them into
the run's branch. That was the intended disposition for work that shipped: the
draft rode its PR and merged with the code. For a run that was abandoned or
whose PR is rejected, the disposition is that **the draft dies with the branch**
— no operator cleanup step, no reconciliation. Nothing reaches `agent-main`
unless the branch merges, and the ID allocation left behind is an ordinary valid
gap (§2). Deleting the worktree removes the only copy; that is expected and is
not the orphaned-allocation condition ORB-10501 repairs. An operator who instead
wants to keep a draft from a discarded branch pulls it over with
`orbit adr reconcile` before the worktree is reaped.

## 7. Indexing Behavior

In the retired implementation, learning reindex and docs/ADR search operated on locally readable bodies. Remote-only allocation rows were skipped without error; once the recorded worktree was present and readable again, the same list/reindex path could read and index the body.

## 8. Concerns & Honest Limitations

The retired remote stubs were intentionally envelope-poor. They exposed allocation metadata, not the artifact title, summary, or body, because those fields lived in the unreadable body file. Filters that required body fields could only apply to locally readable artifacts.

The retired `worktree_root` column preserved historical rows from earlier phases, so old shared-root rows could record a `.orbit/` path while new rows recorded a worktree root. Retired readers resolved `body_path` relative to the recorded value instead of normalizing that history away.

## Task References

- [ORB-00199] introduced the runtime root split.
- [ORB-00200] introduced allocation metadata and the learning ID migration.
- [ORB-00201] implemented local body writes and read federation.
- [ORB-10297] made ADR federation body-preserving and typed the read/mutation boundary.
- [ORB-10272] added the dormant, path-free Remote-v2 hub sequence and reconciliation
  substrate while preserving the standalone shared-root allocator and owner-local
  body/federation semantics. Never activated; removed by [ORB-10725].
- [ORB-10330] added the owner-side preallocated finalizers (`finalize_preallocated`
  on the ADR and learning stores) and the gated broker composition. Removed by
  [ORB-10725]: with no allocation step there is no preallocated ID to finalize.
- [ORB-10725] deleted both substrates under the workspace-scoped knowledge-key model, turned Remote feature v2
  into a no-op slot, and added Remote feature v3 to drop its tables from databases
  that had applied it.
- [ORB-10545] added exact-bundle reconciliation and made superseded ADR bodies
  repository-published decision history under [Publish superseded ADR bodies as durable decision history](./4_decisions.md#publish-superseded-adr-bodies-as-durable-decision-history).
- [ORB-10669] published the remaining partitions (§6) under [Publish every ADR lifecycle partition and resolve duplicates by explicit precedence](./4_decisions.md#publish-every-adr-lifecycle-partition-and-resolve-duplicates-by-explicit-precedence), made the
  managed `.gitignore` block retire its own superseded lines, and replaced
  first-hit-wins ADR resolution with the explicit lifecycle precedence.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
