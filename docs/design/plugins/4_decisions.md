---
title: Plugins — Decisions
owner: claude
last_updated: 2026-11-15
last_validated: 2026-11-15
status: Draft
feature: plugins
doc_role: decisions
type: design
summary: "Plugins — Decisions: why an exact-file write grant may follow a final symlink inside the worktree, a deliberate departure from STD-05 §R7"
tags: [plugins]
paths: ["crates/orbit-tools/**"]
related_features: [plugins]
related_artifacts: [ORB-25145]
---

# Plugins — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

---

## Exact-file grants follow a final symlink inside the worktree

**Recorded:** 2026-11 · [ORB-25145]
**Code anchors:** `crates/orbit-tools/src/plugin/backend/write_file.rs::resolve_workspace_link`, `crates/orbit-tools/src/plugin/backend/sandbox.rs::PluginSandboxProfile`

### Context

`STD-05@1 §R7` says a write does not follow a symlink in its final component, and that a write refuses when what it created does not resolve to the identity that was validated. A `{{workspace}}/AGENTS.md` grant exists so the constellation `standards` sync can rewrite the managed block in a worktree's `AGENTS.md`. In Orbit's own layout that file is a symlink to `CLAUDE.md` in the same worktree, so refusing every link would refuse the case the grant is for.

### Decision

The leaf of an exact-file grant may be a symbolic link only when the call has a workspace, the link sits inside that workspace, and its canonical target is a safe regular file inside the same workspace, outside `.git` and `.orbit`. The grant is the target, never the link. Intermediate links are still refused, and so is any link whose target leaves the workspace, is missing, or lands in workspace metadata. Orbit does not write through the link itself: the plugin's write does, and the kernel grants only the checked target. Before the child starts, the spawn-time check re-resolves the requested path and refuses if it now names a different file, so a retarget between admission and spawn cannot widen the grant.

STD-05 §R9 governs state Orbit loads. The granted workspace file is repository content, so accepting a group-writable one is the task's stated requirement and is not recorded here as a deviation.

### Consequences

- The grant is decided on the physical target that the checked identity names, which keeps the check and the grant on one resolution.
- Alternative refused: refusing every link would honour `STD-05 §R7` literally but would refuse Orbit's own `AGENTS.md` layout, which is the case the grant exists for. A `{{workspace}}` directory grant minus `.git` and `.orbit` was also rejected; see [1_scope.md](./1_scope.md) (*Workspace root files*).
- Cost: the granted file follows whatever the worktree's link names at each admission. A checkout that retargets `AGENTS.md` changes which file is granted, and the manifest digest binds the spelled path, not the link's target in a given worktree.

## Task References

- [ORB-25145] — extended the exact-file write grant to a workspace root file that is a symlink inside the worktree, with the spawn-time re-check and conformance file creation.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
