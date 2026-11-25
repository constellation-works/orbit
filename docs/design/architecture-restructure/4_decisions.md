---
title: Architecture Restructure — Decisions
owner: claude
created_date: 2026-11-25
updated_date: 2026-11-25
last_validated: 2026-11-25
status: Accepted
feature: architecture-restructure
doc_role: decisions
type: design
summary: Decision log for the architecture restructure — the opaque Core, apps by subject, the four-layer runtime, the split engine host, concrete repositories, the kept SQLite files and table names, the single environment capture, and the order of work.
tags: [architecture-restructure, orbit-core, orbit-store, orbit-runtime, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**", "crates/orbit-store/**", "crates/orbit-engine/src/context/hosts/**", "crates/orbit-cmd/**", "crates/orbit-common/src/env_vars.rs"]
related_features: [orbit-core, operations-as-data]
related_artifacts: [ORB-28318, ORB-27886, ORB-15932]
---

# Architecture Restructure — Decisions

Daniel approved the design as proposed on 2026-11-25: "let's take it easy and go with your current design. design approved" [ORB-28318]. Each entry below is one or more of the eleven decisions in [1_overview.md](1_overview.md); the options that were not chosen are in [3_vision.md §1](3_vision.md#1-open-questions). Nothing here is built yet.

- **[Surfaces hold an opaque `Core` and call one `exec`](#surfaces-hold-an-opaque-core-and-call-one-exec)** — Accepted.
- **[Operations are functions in one app per subject](#operations-are-functions-in-one-app-per-subject)** — Accepted.
- **[The runtime is four layers, and the role is resolved once](#the-runtime-is-four-layers-and-the-role-is-resolved-once)** — Accepted.
- **[The engine's host trait splits along its five groups](#the-engines-host-trait-splits-along-its-five-groups)** — Accepted.
- **[Concrete repositories behind one read and one write path](#concrete-repositories-behind-one-read-and-one-write-path)** — Accepted.
- **[Five SQLite files, one layout and one migration mechanism](#five-sqlite-files-one-layout-and-one-migration-mechanism)** — Accepted.
- **[Existing tables keep their plural names](#existing-tables-keep-their-plural-names)** — Accepted.
- **[One environment declaration and one capture](#one-environment-declaration-and-one-capture)** — Accepted.
- **[Ratchets first, and the surface boundary before the inside of core](#ratchets-first-and-the-surface-boundary-before-the-inside-of-core)** — Accepted.

## Surfaces hold an opaque `Core` and call one `exec`

**Recorded:** 2026-11 · [ORB-28318]
**Code anchors:** `crates/orbit-core/src/lib.rs`, `crates/orbit-cmd/src/registry/`

### Context

`OrbitRuntime` is named in 132 files of `orbit-cli`, 56 of `orbit-web` and 31 of `orbit-cmd`, and the same operation is reached by three paths. Daniel's direction: "runtime is used in orbit-core, not orbit-cli".

### Decision

A surface holds `Core`, which has no public field or method, and calls `app::<subject>::exec`. `orbit-core` exports `app`, `Core` and `Error` only. `orbit-cmd` only opens a `Core`; its command groups become apps (decisions 1 and 9).

### Consequences

- A surface that names the runtime, the store or the engine does not compile, because it has no crate edge to them.
- Cost: every read model the dashboard takes straight from the runtime today has to become a command of an app before `orbit-core` can close its exports.

## Operations are functions in one app per subject

**Recorded:** 2026-11 · [ORB-28318]
**Code anchors:** `crates/orbit-core/src/app/`

### Context

About 1,000 operations are inherent methods on one type, so any function can call any subject's operation and the order between modules is held by a script that derives method names.

### Decision

An operation is a function in `orbit-core/src/app/<subject>/`, reached through that app's one `exec`, which authorizes, audits and runs it. The `OrbitRuntime` wrapper and its `unsafe` cast are deleted when no method is left. A subject's repositories, its entry and its functions are converted in separate tasks, with the store, the surface boundary and the inside of core; moving a noun into the registry stays with operations-as-data [ORB-15932] (decisions 2 and 10; amended at filing, 2_design.md §11).

### Consequences

- Module privacy holds the boundary between subjects.
- Cost: until the last subject is converted a reader meets two idioms, `runtime.add_task(…)` and `app::task::add(rt, …)`.

## The runtime is four layers, and the role is resolved once

**Recorded:** 2026-11 · [ORB-28318]
**Code anchors:** `crates/orbit-runtime/src/`

### Context

The kernel is one struct for every kind of process: 14 of its 34 fields are optional, and code asks whether it is a worker at 109 places. Daniel's direction: "I think we need layers of runtime. the lowest level runtime will encompass all operations. […] each layer can be thought of as bestowing certain capabilities to operations", with a crate structure "clear enough to suggest this layers of runtime".

### Decision

The runtime is `Base`, `Stored`, `Host` and `Run`, each a concrete struct that holds the one below, in one crate laid out as those four directories. A flow takes the lowest layer it needs. The process's role is resolved once in `Base`, and `Stored` is built for it. Search, the recovery authority and plugins are side capabilities a layer hands out, not layers. A group inside a layer (the forge, the resource monitor) is its own module and struct, opened on first use behind an accessor; it is not a parameter of each flow. Asked whether that finer form was worth its complexity, Daniel chose the current design (decision 3).

### Consequences

- A flow's first parameter says what it can reach, and a command that needs only `Base` opens no store.
- A group can later become a parameter for one case without changing the others.
- Cost: nothing stops a flow from taking a higher layer than it needs, and folding the role checks changes code paths that drains depend on, so that step is not a pure move.

## The engine's host trait splits along its five groups

**Recorded:** 2026-11 · [ORB-28318]
**Code anchors:** `crates/orbit-engine/src/context/hosts/runtime_host/`, `crates/orbit-core/src/adapter/engine_host/`

### Context

`RuntimeHost` has 153 required methods behind every engine function, one production implementation and 81 test doubles.

### Decision

It becomes five traits along its existing groups, with `RuntimeHost` as their supertrait while bounds are narrowed one engine module at a time. Methods that only return resolved configuration become one `RunSettings` value. The implementor is an adapter that holds `&Run` (decision 4).

### Consequences

- A test double implements only what the function under test uses.
- Cost: the supertrait and the narrow bounds live side by side until the last engine module is converted.

## Concrete repositories behind one read and one write path

**Recorded:** 2026-11 · [ORB-28318]
**Code anchors:** `crates/orbit-store/src/`

### Context

The store has 32 `*Backend` traits, methods named for operations, and about 35 driver files that lock the writer outside `with_transaction` [ORB-27886].

### Decision

One repository per record, a concrete type reached only as `snapshot.<record>()` or `tx.<record>()`. `Store::write` is the only place a write transaction begins. A rule moves out of the store into the operation that owns it and runs inside the transaction (decision 5).

### Consequences

- A write outside a transaction does not compile.
- Cost: each operation-named store method has to be split by hand into a rule and record methods, one record at a time.

## Five SQLite files, one layout and one migration mechanism

**Recorded:** 2026-11 · [ORB-28318]
**Code anchors:** `crates/orbit-store/src/driver/sqlite/`

### Context

Orbit has five SQLite files and four ways to migrate one.

### Decision

The five files stay. Each gets the same layout, one ordered schema list and the same migration ledger. Existing schema steps are moved, not rewritten (decision 6).

### Consequences

- `rusqlite` is a dependency of `orbit-store`, `orbit-search`, `orbit-auth` and `orbit-common` only.
- Cost: one write transaction still holds per file, not across files.

## Existing tables keep their plural names

**Recorded:** 2026-11 · [ORB-28318]

### Context

Most of about 88 tables are plural, and a rename is a schema step per table across hosts that run different versions.

### Decision

Existing tables keep their names. New tables are singular (decision 7).

### Consequences

- When Orbit adopts STD-12, this is recorded as its deviation from the singular-name rule.
- Cost: the schema carries two naming conventions for as long as the old tables live.

## One environment declaration and one capture

**Recorded:** 2026-11 · [ORB-28318]
**Code anchors:** `crates/orbit-common/src/env_vars.rs`

### Context

`env_vars.rs` declares 136 names, but there are 232 `env::var` calls in 12 crates and nothing forbids a new one.

### Decision

`env_vars.rs` stays the one declaration. Each binary's `main` captures the environment once as `Env`, and Clippy bans every other read (decision 8).

### Consequences

- Code below composition receives resolved values and does not read the environment.
- Cost: a variable changed after start is not seen, which is a change for any code that relied on a late read.

## Ratchets first, and the surface boundary before the inside of core

**Recorded:** 2026-11 · [ORB-28318]
**Code anchors:** `scripts/architecture-ratchet-baseline.json`

### Context

Orbit lands many unrelated changes a day and cannot stop for a rewrite, so the old shape can regrow while the new one is built.

### Decision

The order is: ratchets, environment, store, the surface boundary, the inside of core, the engine host. A ratchet holds each old-shape count at or below its checked-in baseline. Each step lands on its own and changes no behaviour (decision 11). That is the order in which the gates close: the filed tasks let the environment, the store, the surface boundary and the engine host proceed side by side once the ratchet check lands, and carve the runtime's layers before the inside of core moves (2_design.md §11).

### Consequences

- After the surface boundary closes, anything inside `orbit-core` can move without touching a surface.
- Cost: the runtime, the largest pain, is treated second to last.

## Task References

- [ORB-28318] — carried the proposal to Daniel's approval.
- [ORB-27886] — recorded the STD-02 deviations this restructure answers.
- [ORB-15932] — set the noun-by-noun order the conversions follow.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
