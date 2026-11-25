---
title: Architecture Restructure — Overview
owner: claude
created_date: 2026-11-25
updated_date: 2026-11-25
last_validated: 2026-11-25
status: Accepted
stage: designing
next: "File the step 0 tasks: the ratchet check and the app table [ORB-28318]"
feature: architecture-restructure
doc_role: overview
type: design
summary: Approved design to put the runtime behind orbit-core and split it into four layers that each add a capability, slice the application layer into one unit per subject with one entry point, give orbit-store one repository per record behind one write path, and read the environment once.
tags: [architecture-restructure, orbit-core, orbit-store, orbit-runtime, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**", "crates/orbit-store/**", "crates/orbit-engine/src/context/hosts/**", "crates/orbit-cmd/**", "crates/orbit-common/src/env_vars.rs"]
related_features: [orbit-core, operations-as-data]
related_artifacts: [ORB-28318, ORB-27886, ORB-17906, ORB-15932, ORB-10358, ORB-10016]
---

# Architecture Restructure — Overview

Daniel approved this design on 2026-11-25; nothing in it is built yet. It describes the shape Orbit's code is moved to, a step at a time, so that five things become true: a surface (the CLI, the dashboard, the MCP server) never holds the runtime; every subject a user can name is one unit in `orbit-core` with one entry point; the runtime is four layers, and each operation names the lowest one it needs; `orbit-store` has one repository per record behind one write path; and the process environment is read in one place. It changes no behaviour, no persisted format and no crate list. It is the restructure that [the recorded STD-02 deviations](../orbit-core/4_decisions.md#standing-against-std-025-r2-r6-r18-r39r44) defer to, and it gives the handlers of [operations as data](../operations-as-data/1_overview.md) a home.

Daniel set the direction on 2026-11-25: "I want to slowly start fixing up orbit", naming the runtime god trait, `orbit-store` on the store design the Rust CLI template now uses, and one place for environment variables; and "runtime can stay in orbit-runtime crate. and runtime is used in orbit-core, not orbit-cli". On the first draft he added: "I think we need layers of runtime. the lowest level runtime will encompass all operations. […] each layer can be thought of as bestowing certain capabilities to operations", and asked for a crate structure "clear enough to suggest this layers of runtime" [ORB-28318].

## Decisions

Daniel approved all eleven as proposed: "let's take it easy and go with your current design. design approved" [ORB-28318]. They are recorded in [4_decisions.md](4_decisions.md); the alternatives are in [3_vision.md §1](3_vision.md#1-open-questions).

| # | Decision | Decided |
|---|---|---|
| 1 | What a surface holds | An opaque `Core` handle. It calls `app::<subject>::exec` and nothing else; `orbit-core` stops exporting `OrbitRuntime`, `orbit_runtime::*` and store items. |
| 2 | Where operations live | Functions in `orbit-core/src/app/<subject>/`, not methods on `OrbitRuntime`. The `OrbitRuntime` wrapper and its `unsafe` cast are deleted at the end; the runtime stays in `orbit-runtime`. |
| 3 | What an operation runs against | A runtime in four layers, each a struct holding the one below: `Base` (every operation), `Stored` (the workspace and the store), `Host` (this machine as a place work runs), `Run` (the run this process carries). An operation takes the lowest layer it needs. `orbit-runtime/src` is laid out as those four. |
| 4 | The engine's `RuntimeHost` | Split along its five existing groups into five traits; configuration getters become one value passed in at run start. |
| 5 | Store API | Concrete repositories, one per record, reached through `Snapshot` and `Tx`. The 32 `*Backend` traits go; rules move out of the store into the calling operation. |
| 6 | The five SQLite databases | Keep the five files. Give each the same layout, one ordered schema list and one migration mechanism. |
| 7 | Existing table names | Keep the plural names and record the deviation from STD-12 §R1; new tables are singular. |
| 8 | Environment | Keep `env_vars.rs` as the one declaration; add one capture at process start and ban every other read with Clippy. |
| 9 | `orbit-cmd` | It only opens a `Core`. Its command groups become apps in `orbit-core`. |
| 10 | Unit of work | The subject. One subject's registry spec, app and repositories are converted together, in the order operations-as-data already uses. |
| 11 | Order | Ratchets first, then environment, store, the surface boundary, the inside of core, and the engine host last. |

## 1. Motivation

Orbit's crate graph is sound: twenty crates in declared layers, enforced by `scripts/check-dependency-direction.sh`, with a machine-checked map. The problem is inside and across three of them. All counts below are from `agent-main` at `b81eca83` (2026-11-25), outside test files unless noted.

| What | Measured | What it costs |
|---|---|---|
| Every operation is a method on the runtime | `impl OrbitRuntime` or `impl RuntimeKernel` blocks in 366 files (325 in `orbit-core`, 41 in `orbit-runtime`); about 1,000 methods in `orbit-core`'s blocks, 884 of them under `application/`; a `&OrbitRuntime` parameter 1,075 times in 330 files | Any function can call any operation of any subject. The module system cannot see the calls, so the order inside `application/` is kept by a script that derives method names from the `impl` blocks. |
| Surfaces hold the runtime | `OrbitRuntime` is named in 132 files of `orbit-cli`, 56 of `orbit-web` and 31 of `orbit-cmd`; `orbit-core` re-exports all of `orbit_runtime` (`runtime/mod.rs:13`) and items of `orbit_store` (`lib.rs:218–279`) | "A surface parses, calls one operation and renders" (STD-02 §R2) cannot be enforced: the surface has the whole object. The same operation is reached three ways: `orbit task add` goes through the tool path, the dashboard's `POST /api/tasks` calls `add_task_screened` directly, and `orbit run ship` calls three runtime methods in sequence. |
| The engine's host trait | `RuntimeHost` has 153 required methods, one production implementation (17 files, about 3,500 lines) and 81 test doubles; engine functions take it at 430 sites | Every engine function can call all 153. A test of one function needs a double that answers all of them, which is why `NullRuntimeHost` exists. |
| The runtime is one struct for every kind of process | 34 fields, 14 of them `Option`; the worker invocation is asked for at 109 places and the coordination owner at 85; following calls, 1,385 of the 2,925 functions in `orbit-core` that take the runtime reach a worker or drain field; `orbit-runtime/src` has 32 top-level modules | An operation's signature says nothing about what it needs, and every operation can reach what only a drain has. The crate's layout does not say either. |
| The kernel calls back into core | `ApplicationPort` has 20 methods; `from_kernel_ref`, an `unsafe` cast, is used at 21 sites | Kernel mechanisms finish through use cases, so the crate that must not know core reaches it through a static and a pointer cast. |
| The store decides and has no single write path | 32 `*Backend` traits behind `Arc<dyn …>`; operation-named methods such as `admit_owner_wave` and `verify_worker_claim`; `with_transaction` in 44 files, but about 35 driver files lock the writer directly and three stores open their own connection; `Store::open` is called from 29 places outside the crate | Rules that belong to an operation sit in persistence (STD-02 §R40), and "every write is one transaction" is a convention, not a property. |
| Five databases, four migration mechanisms | `orbit.db` (95 steps, about 88 tables), `tasks/index.sqlite`, `semantic.db`, `authority.db`, `lease-tokens.db`; one ledger, a second ledger, an unnumbered `ensure_schema`, and a `user_version` array | A schema change is written four different ways depending on the file. |
| The environment is declared once but read everywhere | `env_vars.rs` declares 136 names and generates the reference in `docs/CONFIG.md`; there are 232 `env::var` calls in 12 crates, 88 of them through a local constant or a computed name; nothing forbids a new one | The declaration is not the only way in, so it cannot be trusted to be complete, and code far below composition reads the environment (STD-02 §R3). |
| A command is declared in five places | the Clap enum, the `operation()` match, the help tables, 13 noun registries, and plugin manifests | Adding a command means five edits that the compiler only partly links. |

Two earlier designs already point here. [Orbit core](../orbit-core/1_overview.md) made the direction `runtime ← application ← adapter` and moved the kernel into its own crate [ORB-17906]. [Operations as data](../operations-as-data/1_overview.md) made each operation's surface contract one spec [ORB-10358], and decided the remaining nouns migrate on a plan [ORB-15932]. The STD-02 standing entry [ORB-27886] then recorded seven deviations "deferred until restructure". This proposal is that restructure: it says what is behind a registry handler, and what a handler may reach.

## 2. Core Concepts

- **Surface.** Code that parses input, calls one operation and renders the result: `orbit-cli`, `orbit-web`, and the MCP transport with its host in the CLI.
- **Core handle (`Core`).** The opaque value a surface holds for one opened workspace. It has no public method and no public field. It is built by composition and passed to `exec`.
- **App.** One unit of `orbit-core` for one subject a user can name (`task`, `run`, `plugin`, `notice`). It holds that subject's `Command`, `Outcome` and `Error`, one `exec`, its flows and its rules. This is STD-02 §R45, and the template's `app/<subject>/`.
- **`exec`.** The one entry point of an app. It authorizes the command, wraps a mutating command in the audit guard, runs it, and returns the outcome. Every surface and every adapter ends here.
- **Runtime.** The `orbit-runtime` crate: four layers, `Base`, `Stored`, `Host` and `Run`, each adding a capability to the one below. It holds mechanisms and no subject's operation, and only `orbit-core` uses it.
- **Layer.** One struct of the runtime. A flow takes the lowest layer it needs and cannot reach above it.
- **Role.** What kind of process this is: an operator's command, a drain owner, a worker or a replica checkout. Resolved once at start and held in `Base`.
- **Side capability.** Something a few operations need that does not stack on the others (the search index, the recovery authority, plugins). A layer hands it out on first use.
- **Flow.** One function for one command: load, decide, effect, commit (STD-02 §R41).
- **Port.** A trait an app owns for something it needs from an app above it or from an external system (STD-02 §R42).
- **Host trait.** A trait a lower crate owns and `orbit-core` implements, so the lower crate can call up: the engine's `RuntimeHost`, the tool registry's `OrbitToolHost`.
- **Repository.** The code for one stored record: its SQL or file format and its row mapping. Reached only as `snapshot.<record>()` or `tx.<record>()`.
- **Snapshot and Tx.** What a read and a write hand to the caller: one consistent read view, and one write transaction.
- **Driver.** The code that sets one storage mechanism up once and hands back a handle: the file name, the open call, the pragmas, the schema.
- **Schema step.** One numbered, frozen change to a database's schema, in one ordered list per database.
- **Env.** The value holding the process environment, captured once in `main`.
- **Ratchet.** A CI check that holds a count at or below its checked-in baseline, so the old shape cannot grow while the new one is being built.

## 3. At a Glance

Nothing is built, so the Task column is empty. The File column is where each concern lives after the restructure.

| Concern | File | Task |
|---|---|---|
| What a surface may name | `crates/orbit-core/src/lib.rs` (exports `app`, `Core`, `Error` only) | — |
| One unit per subject | `crates/orbit-core/src/app/<subject>/` | — |
| The entry point, authorization and audit | `crates/orbit-core/src/app/exec.rs` | — |
| What every operation has | `crates/orbit-runtime/src/base/` | — |
| The opened workspace and the store | `crates/orbit-runtime/src/stored/` | — |
| This machine as a place work runs | `crates/orbit-runtime/src/host/` | — |
| The run a process carries | `crates/orbit-runtime/src/run/` | — |
| Port bindings between apps | `crates/orbit-core/src/app/port.rs` | — |
| Engine host, as five traits | `crates/orbit-engine/src/context/hosts/`, implemented in `crates/orbit-core/src/adapter/engine_host/` | — |
| Opening a `Core` for a registered workspace | `crates/orbit-cmd/src/registry/` | — |
| Record shapes | `crates/orbit-store/src/contract/` | — |
| One repository per record | `crates/orbit-store/src/repository/<record>.rs` | — |
| One driver per storage mechanism, and its schema | `crates/orbit-store/src/driver/sqlite/<database>/schema/` | — |
| The read and write facade | `crates/orbit-store/src/store.rs` | — |
| Environment declaration and capture | `crates/orbit-common/src/env_vars.rs`, `crates/orbit-common/src/env.rs` | — |
| Ratchet baselines | `scripts/architecture-ratchet-baseline.json` | — |

## What does not change

- The crate list and the layer order in `ARCHITECTURE.md`. Crates follow build boundaries, not taxonomy (STD-02 §R7, bearing 5).
- Behaviour, command names, output, and every persisted format. Hosts on different versions keep working together as [state compatibility](../state-compatibility/1_overview.md) requires.
- The operation registry: one spec per operation still drives the CLI, MCP and runtime adapters.
- The YAML and SQLite task store and its commit boundary.
- The engine stays its own crate, and Orbit keeps its own code: it does not adopt the template's kit crates.

## Task References

- [ORB-28318] — carried this proposal to Daniel's approval.
- [ORB-27886] — recorded Orbit's standing against STD-02@5 and the deviations deferred until a restructure.
- [ORB-17906] — moved the runtime kernel into `orbit-runtime`.
- [ORB-15932] — started the planned noun-by-noun migration to the operation registry.
- [ORB-10358] — piloted operations as data on the friction noun.
- [ORB-10016] — extracted the CLI-facing command layer into `orbit-cmd`.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
