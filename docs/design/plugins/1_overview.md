---
title: Plugins — Overview
owner: claude
created_date: 2026-09-20
updated_date: 2026-11-28
last_validated: 2026-11-28
status: Accepted
stage: shipped
next: "—"
feature: plugins
doc_role: overview
type: design
summary: "Implemented plugin manifest, lifecycle, grants, brokered agent calls, consented builds and declarative extensions."
tags: [plugins, tools, routines, auto-tasks, dashboard, cli]
---

# Plugins — Overview

Status: phases 1–4 shipped; provider plugins (phase 5) remain deferred vision.
Bearing: [Operations as data, not inherent methods](../orbit-core/4_decisions.md).
Precedents: `*.orbit-tool.yaml` sidecar manifests (orbit-graph still ships three; its migration
to `plugin.yaml` is pending), and orbit-research's version/capability allowlist against the
Orbit binary.

## Problem

Before this standard a "plugin" was three unrelated things: an **external tool** (executable
plus sidecar, registered by `orbit tool add`, unsandboxed, absent from MCP —
`crates/orbit-tools/src/external.rs`); the **Claude Code plugin mirror** under `plugin/`; and a
**separate product** (orbit-research, orbit-graph) shelling out to `orbit tool run`. Nothing
let one artifact declare tools, CLI, schedules, panels and skills together, or record what it
is *allowed* to do; a first-party surface cost up to nine hand edits [ORB-12724].

## Goal

One manifest (`plugin.yaml`, `schemaVersion: 2`, superseding the v1 tool sidecar) declaring
every contribution (tools, CLI, dashboard panels, routines, auto-tasks, activities, jobs,
skills, config); one lifecycle (`orbit plugin …`); one execution protocol; one set of
invariants enforced at load, enable and call time. A plugin is **data plus one backend
executable**: no Rust, no dashboard JavaScript. The committed standard shipped in
[ORB-12735]–[ORB-12738]; the host-side broker and consented install-time builds
are also implemented (see [2_design.md §7](./2_design.md#7-phases-each-its-own-pr-into-agent-main)).

### Non-goals for v1

- New Rust **deterministic activity actions** (`DeterministicAction` stays closed); plugins
  expose a tool and call it through the generic `plugin.tool_call` action ([2_design.md §4.5](./2_design.md#45-definitions)).
- New **agent providers / executor types**; opening `ProviderRegistry` is Phase 5.
- **Dashboard code with the dashboard's authority, top-level destinations or built-in
  feature toggles.** Plugins get declarative panels and links, and
  [pages](../plugin-pages/1_overview.md): sub-tabs under Plugins that run in a sandboxed
  frame and reach data only through the plugin's own `read_only` tools; a page runs one of
  its plugin's mutating tools only as a consented action the operator confirms in a dialog
  the dashboard draws outside the frame. A plugin adds no destination to the rail.
- Marketplace/discovery. Sources are a path, a git URL or an archive.

The full specification and contracts are in [2_design.md](./2_design.md); recorded decisions are in [4_decisions.md](./4_decisions.md).

## Task References

- [ORB-12724] — measured the first-party surface cost this standard removes.
- [ORB-12735] — phase 1: built the `plugin.yaml` v2 manifest, the
  `orbit plugin` lifecycle, and plugin tools reaching MCP `tools/list`.
- [ORB-12738] — phase 4: built the schema-derived `orbit <ns> <verb>` CLI,
  dashboard plugin panels and links, and `orbit plugin test`/`scaffold`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
