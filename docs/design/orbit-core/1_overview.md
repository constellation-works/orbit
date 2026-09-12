---
title: Orbit Core — Overview
owner: codex
last_updated: 2026-08-16
last_validated: 2026-09-10
status: Accepted
feature: orbit-core
doc_role: overview
type: design
summary: Directional application, runtime, adapter, bootstrap, and composition boundaries inside orbit-core.
tags: [orbit-core, orbit-cmd, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-cmd/**", "crates/orbit-registry/**"]
related_features: [orbit-core]
related_artifacts: [ORB-10016, ORB-10886]
---

# Orbit Core — Overview

`orbit-core` contains Orbit's runtime kernel and the use cases and adapters
that drive it. Those concerns remain in one crate, but they are directional:

```text
runtime <- application <- adapter
   ^             ^           ^
   +------ composition -------+
              ^
           bootstrap
```

Composition loads resolved `orbit-config`, runs bootstrap, constructs the
runtime, and attaches adapters. Runtime production code never imports
application or adapter code.

## 1. Motivation

After [ORB-10016] extracted CLI-facing commands, Core still had a two-way
internal dependency: runtime construction and hosts imported DTOs, defaults,
and business rules from `command`, while command operations reached into
runtime and adapter storage. [ORB-10886] replaces that cycle with explicit
owners without externalizing it as new crates.

## 2. Core Concepts

- **Runtime kernel** — stores, engine handles, event bus, audit, claims,
  reservations, process/tool execution mechanisms, and construction from an
  already-resolved config value.
- **Application operations** — use-case DTOs and coordinated task, job,
  workflow, docs, search, semantic, and health behavior. Store-coordinated
  lifecycle transitions live here; pure invariants live in `orbit-types`.
- **Adapters** — command audit/dispatch, `orbit.*` tool-host translation, and
  `orbit-engine::RuntimeHost` callback translation. Adapters may use both
  application operations and runtime mechanisms.
- **Bootstrap** — initialization, managed/default assets, policy seeding, and
  forward-only startup migrations.
- **Composition** — root resolution and the sole join of `orbit-config`,
  bootstrap, runtime construction, and adapters.
- **Registry-aware composition** — opening a selected registered checkout and
  composing routine discovery from local `host.toml` plus `workspaces.json`
  lives in `orbit-cmd::registry_runtime` and `orbit-cmd::registry_routines`,
  over persistence and validation owned by `orbit-registry`.
- **Consumer-justified re-export** — a root `pub use` kept only because a
  consumer crate (`orbit-cli`, `orbit-web`, `orbit-cmd`) genuinely
  imports it from the root.

## 3. At a Glance

| Concern | File | Task |
|---|---|---|
| Runtime mechanisms | `crates/orbit-core/src/runtime/` | [ORB-10886] |
| Application operations and DTOs | `crates/orbit-core/src/application/` | [ORB-10886] |
| Tool/engine/command adapters | `crates/orbit-core/src/adapter/` | [ORB-10886] |
| Scheduling-domain host port (`AutomationHost`) | `crates/orbit-core/src/adapter/automation_host/` | [ORB-12262] |
| Initialization and managed defaults | `crates/orbit-core/src/bootstrap/` | [ORB-10886] |
| Config loading and runtime assembly | `crates/orbit-core/src/composition.rs` | [ORB-10886] |
| Context assembly | `crates/orbit-core/src/context.rs` | — |
| Resolved-config consumption | `crates/orbit-core/src/runtime/builder.rs` | [ORB-10885], [ORB-10886] |
| Config layering (owning crate) | `crates/orbit-config/src/` | [ORB-10885] |
| Registered runtime + routine composition | `crates/orbit-cmd/src/registry_runtime.rs`, `crates/orbit-cmd/src/registry_routines.rs` | — |
| Host identity + local workspace catalog | `crates/orbit-registry/src/` | — |
| Tool dispatch and command-audit boundary | `crates/orbit-core/src/adapter/command/dispatch.rs` | [ORB-10886] |
| Root re-export policy | `crates/orbit-core/src/lib.rs` | [ORB-10016] |
| Extracted command layer | `crates/orbit-cmd/src/` | [ORB-10016] |

## 4. Store Access Boundary

`OrbitStores` is Core's internal holder for typed `orbit-store` backends; it is
not an external SDK surface. Code inside Core delegates ordinary persistence
to the owning backend traits, while outer crates use explicit `OrbitRuntime`
methods and application APIs. Runtime exposes mechanisms; coordination across
stores or side effects belongs to the owning application operation.

## Task References

- [ORB-10012] — introduced the versioned workspace-layout migration pre-flight the runtime open runs.
- [ORB-10016] — extracted `orbit-cmd` from orbit-core and trimmed the root re-export surface.
- [ORB-10355] — removed the hand-maintained store delegation layer in favor of direct typed backend access.
- [ORB-10886] — removed the internal command/runtime cycle and established the directional module graph.
- [ORB-12262] — moved the scheduling domain (`application/{automation,auto_tasks,routines}`) down into
  `orbit-automation`; Core's remaining share is the `adapter/automation_host` port implementation.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
