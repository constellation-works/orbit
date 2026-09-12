---
summary: "Worktree Artifacts - Vision"
type: design
title: "Worktree Artifacts - Vision"
owner: codex
last_updated: 2026-05-20
status: Accepted
feature: worktree-artifacts
doc_role: vision
tags: ["worktree-artifacts"]
paths: ["crates/orbit-core/**", "crates/orbit-store/**", "crates/orbit-cli/**"]
related_features: ["worktree-artifacts"]
related_artifacts: ["ORB-00199", "ORB-00200", "ORB-00201"]
last_validated: 2026-09-12
---

# Worktree Artifacts - Vision

> This vision records retired ADR and learning-store behavior. ORB-10726 retired
> the ADR store and tool surface, and ORB-10736 removed the native learning
> subsystem; the shared/local runtime-root split remains, while current decisions
> live in each feature's `4_decisions.md`.

The worktree artifact model should make knowledge artifacts feel like ordinary branch files without weakening Orbit's shared coordination state.

## 1. Open Questions

1. Should remote stubs grow an optional cached envelope so `include_remote` can show titles and summaries without reading body files?
2. Should Orbit offer a repair command for stale `body_path` rows after users move or delete linked worktrees manually?
3. Should ADR and learning updates refuse to mutate bodies outside the current worktree, or offer an explicit remote-edit mode?
4. Should sync/hosted modes replace local filesystem body paths with content-addressed blobs?

## 2. Prior Work

### Runtime Roots

[ORB-00199] established the root split that this design depends on. The important precedent is that existing shared state stays bound to `shared_root` unless a task deliberately flips it.

### Allocation

[ORB-00200] established SQLite as the ID authority and moved learning IDs to `L-NNNN`. That made per-worktree body storage possible without filesystem-scan collisions.

### Artifact Discipline

The historical ADR and learning surfaces required tool-mediated writes instead of direct YAML edits. [ORB-00201] changed where those tool-mediated writes landed, not the authorship rule.

## 3. What May Be Distinctive

The model separates artifact identity from artifact body locality. IDs and allocation rows are global enough for cross-worktree references; bodies stay branch-local enough for normal git staging and review.

The remote-stub behavior also gives agents a gentle failure mode: they can see that an artifact exists without pretending to know its unreadable content.

## 4. References

- [1_overview.md](./1_overview.md) summarizes the root and artifact split.
- [2_design.md](./2_design.md) describes the implemented read/write paths.
- [4_decisions.md](./4_decisions.md) records the accepted architectural choice.

## Task References

- [ORB-00199] introduced shared/local root resolution.
- [ORB-00200] introduced shared ID allocation and `L-NNNN`.
- [ORB-00201] implemented local artifact bodies and federation.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
