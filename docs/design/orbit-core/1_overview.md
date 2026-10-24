---
title: Orbit Core — Overview
owner: codex
last_updated: 2026-10-24
last_validated: 2026-10-24
status: Accepted
feature: orbit-core
doc_role: overview
type: design
summary: Directional runtime, application, adapter, bootstrap, and composition boundaries across orbit-runtime and orbit-core.
tags: [orbit-core, orbit-cmd, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**", "crates/orbit-cmd/**", "crates/orbit-registry/**"]
related_features: [orbit-core]
related_artifacts: [ORB-10016, ORB-10886, ORB-17906]
---

# Orbit Core — Overview

Orbit's runtime kernel lives in the internal `orbit-runtime` crate.
`orbit-core` holds the use cases and adapters that drive it, and depends on
`orbit-runtime`. The concerns are directional:

```text
runtime <- application <- adapter
   ^             ^           ^
   +------ composition -------+
              ^
           bootstrap
```

Composition loads resolved `orbit-config`, runs bootstrap, constructs the
runtime, and attaches adapters. `orbit-runtime` never depends on `orbit-core`,
so kernel code cannot import application or adapter code. Where a kernel
mechanism finishes through a use case, it calls the `ApplicationPort` trait
that composition supplies when it builds the kernel.

`orbit_core::OrbitRuntime` stays the public runtime type. It owns a
`RuntimeKernel` and dereferences to it, so callers reach kernel methods
directly. Application and adapter methods are inherent on the wrapper.
`orbit_core::runtime` re-exports the kernel crate, so `orbit_core::runtime::…`
paths still resolve.

## 1. Motivation

After [ORB-10016] extracted CLI-facing commands, Core still had a two-way
internal dependency: runtime construction and hosts imported DTOs, defaults,
and business rules from `command`, while command operations reached into
runtime and adapter storage. [ORB-10886] replaced that cycle with explicit
owners inside the crate. [ORB-17906] moved the kernel into `orbit-runtime`,
so an application or adapter edit no longer recompiles it and Cargo, not a
source scan, enforces the runtime direction.

## 2. Core Concepts

- **Runtime kernel** — `orbit-runtime`'s `RuntimeKernel`: stores, engine
  handles, event bus, audit, claims, reservations, process/tool execution
  mechanisms, and construction from an already-resolved config value. The
  actor context, root paths and cost types live there too.
- **Application port** — `orbit_runtime::ApplicationPort`, the operations
  the kernel calls back into (task reads, automation updates, audit records,
  run reconciliation, crew redraws). `orbit-core`'s `runtime` module
  implements it on the `OrbitRuntime` that owns the kernel.
- **Application operations** — use-case DTOs and coordinated task, job,
  workflow, search, and health behavior. Store-coordinated
  lifecycle transitions live here; pure invariants live in `orbit-types`.
- **Adapters** — command audit/dispatch, `orbit.*` tool-host translation, and
  `orbit-engine::RuntimeHost` callback translation. Adapters may use both
  application operations and runtime mechanisms.
- **Bootstrap** — initialization, managed/default assets, policy seeding, and
  forward-only startup migrations.
- **Composition** — root resolution and the sole join of `orbit-config`,
  bootstrap, runtime construction, and adapters.
- **Runtime roots** — `OrbitRuntimeRoots` carries `global_root`,
  `shared_root`, and `local_root`. On the primary checkout `shared_root` and
  `local_root` are equal; in a linked worktree `shared_root` is the primary
  checkout's `.orbit/` and `local_root` is the worktree's own. An explicit
  `--root` or `ORBIT_ROOT` pins both. A submodule is an independent checkout:
  bootstrap stops at its repository boundary and runtime honors its registered
  data directory. Its gitfile does not make it a linked worktree of the
  superproject. When Git is unavailable, linked-worktree discovery requires a
  `worktrees/<name>` gitdir whose `commondir` points to the enclosing `.git`.
- **Registry-aware composition** — opening a selected registered checkout and
  composing routine discovery from the global `config.toml` `[machine]` table
  plus `workspaces.json`
  lives in `orbit-cmd::registry_runtime` and `orbit-cmd::registry_routines`,
  over persistence and validation owned by `orbit-registry`.
- **Consumer-justified re-export** — a root `pub use` kept only because a
  consumer crate (`orbit-cli`, `orbit-web`, `orbit-cmd`) genuinely
  imports it from the root.

## 3. At a Glance

| Concern | File | Task |
|---|---|---|
| Runtime mechanisms | `crates/orbit-runtime/src/` | [ORB-10886], [ORB-17906] |
| Kernel re-export, `OrbitRuntime` wrapper and application port | `crates/orbit-core/src/runtime.rs` | [ORB-17906] |
| Application operations and DTOs | `crates/orbit-core/src/application/` | [ORB-10886] |
| Tool/engine/command adapters | `crates/orbit-core/src/adapter/` | [ORB-10886] |
| Initialization and managed defaults | `crates/orbit-core/src/bootstrap/` | [ORB-10886] |
| Config loading and runtime assembly | `crates/orbit-core/src/composition.rs` | [ORB-10886] |
| Context assembly | `crates/orbit-runtime/src/context.rs` | — |
| Resolved-config consumption | `crates/orbit-runtime/src/builder.rs` | [ORB-10885], [ORB-10886] |
| Config layering (owning crate) | `crates/orbit-config/src/` | [ORB-10885] |
| Registered runtime + routine composition | `crates/orbit-cmd/src/registry/runtime/mod.rs`, `crates/orbit-cmd/src/registry/routines.rs` | — |
| Host identity + local workspace catalog | `crates/orbit-registry/src/` | — |
| Tool dispatch and command-audit boundary | `crates/orbit-core/src/adapter/command/dispatch/` | [ORB-10886] |
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
- [ORB-17906] — extracted the runtime kernel into the `orbit-runtime` crate behind the `OrbitRuntime` wrapper.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
