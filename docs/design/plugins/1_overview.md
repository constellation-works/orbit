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

The plugin standard lets one manifest declare everything an add-on contributes to Orbit (tools, CLI, dashboard panels, routines, auto-tasks, activities, jobs, skills and config) and record what it is allowed to do, under one lifecycle and one execution protocol. A plugin is data plus one backend executable.

Status: phases 1–4 shipped; provider plugins (phase 5) remain deferred vision.

## 1. Motivation

Bearing: [Operations as data, not inherent methods](../orbit-core/4_decisions.md).
Precedents: `*.orbit-tool.yaml` sidecar manifests (orbit-graph still ships three; its migration
to `plugin.yaml` is pending), and orbit-research's version/capability allowlist against the
Orbit binary.

### The gap

Before this standard a "plugin" was three unrelated things: an **external tool** (executable
plus sidecar, registered by `orbit tool add`, unsandboxed, absent from MCP —
`crates/orbit-tools/src/external.rs`); the **Claude Code plugin mirror** under `plugin/`; and a
**separate product** (orbit-research, orbit-graph) shelling out to `orbit tool run`. Nothing
let one artifact declare tools, CLI, schedules, panels and skills together, or record what it
is *allowed* to do; a first-party surface cost up to nine hand edits [ORB-12724].

### Intended outcome

One manifest (`plugin.yaml`, `schemaVersion: 2`, superseding the v1 tool sidecar) declaring
every contribution (tools, CLI, dashboard panels, routines, auto-tasks, activities, jobs,
skills, config); one lifecycle (`orbit plugin …`); one execution protocol; one set of
invariants enforced at load, enable and call time. A plugin is **data plus one backend
executable**: no Rust, no dashboard JavaScript. The committed standard shipped in
[ORB-12735]–[ORB-12738]; the host-side broker and consented install-time builds
are also implemented (see [2_design.md §7](./2_design.md#7-phases-each-its-own-pr-into-agent-main)).

#### Out of scope for v1

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

## 2. Core Concepts

- **Plugin.** Data plus one backend executable: a `plugin.yaml` manifest (`schemaVersion: 2`) and a program, with no Rust and no dashboard JavaScript.
- **Contribution.** One thing a manifest declares: a tool, CLI group, dashboard panel, routine, auto-task, activity, job, skill or config entry.
- **Lifecycle.** The `orbit plugin …` commands that install, consent to, enable and remove a plugin; definitions a plugin seeds land disabled until an operator act.
- **Execution protocol.** The one way Orbit calls a plugin's backend, under invariants enforced at load, enable and call time and a sandbox with nothing granted by default.
- **Placement, never permission.** A plugin's declared contribution says where it appears; what it may do is a separate, operator-consented grant.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Contribution model | [2_design.md §1](./2_design.md#1-contribution-model) | [ORB-12735] |
| Manifest | [2_design.md §2](./2_design.md#2-manifest) | [ORB-12735] |
| Lifecycle and state | [2_design.md §3](./2_design.md#3-lifecycle-and-state) | [ORB-12735] |
| Contracts Orbit enforces | [2_design.md §4](./2_design.md#4-contracts-orbit-enforces) | [ORB-12735] |
| Schema-derived CLI and dashboard panels | [2_design.md §4.6–4.7](./2_design.md#46-cli) | [ORB-12738] |
| Install-time build | [specs/install-time-build.md](./specs/install-time-build.md) | — |
| Agent call broker | [specs/agent-call-broker.md](./specs/agent-call-broker.md) | — |
| Phases | [2_design.md §7](./2_design.md#7-phases-each-its-own-pr-into-agent-main) | [ORB-12735], [ORB-12738] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-12724] — measured the first-party surface cost this standard removes.
- [ORB-12735] — phase 1: built the `plugin.yaml` v2 manifest, the
  `orbit plugin` lifecycle, and plugin tools reaching MCP `tools/list`.
- [ORB-12738] — phase 4: built the schema-derived `orbit <ns> <verb>` CLI,
  dashboard plugin panels and links, and `orbit plugin test`/`scaffold`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
