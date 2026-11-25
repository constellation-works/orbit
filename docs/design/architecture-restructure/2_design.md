---
title: Architecture Restructure — Design
owner: claude
created_date: 2026-11-25
updated_date: 2026-11-25
last_validated: 2026-11-25
status: Draft
feature: architecture-restructure
doc_role: design
type: design
summary: The proposed target shape (surfaces hold an opaque Core, apps by subject with one exec, the runtime as the one context, split host traits, repositories behind Snapshot and Tx, one environment capture), the gates that hold it, and the order of work.
tags: [architecture-restructure, orbit-core, orbit-store, orbit-runtime, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**", "crates/orbit-store/**", "crates/orbit-engine/src/context/hosts/**", "crates/orbit-cmd/**", "crates/orbit-common/src/env_vars.rs"]
related_features: [orbit-core, operations-as-data]
related_artifacts: [ORB-28318, ORB-27886, ORB-17906, ORB-15932, ORB-10358]
---

# Architecture Restructure — Design

This doc describes the proposed target, not current code: every section says what exists today and what it becomes. Nothing here is implemented. The choices Daniel is asked to make, with their alternatives, are in [3_vision.md](3_vision.md); the measurements behind the motivation are in [1_overview.md](1_overview.md). Counts are from `agent-main` at `b81eca83`.

## 1. The target in one picture

```text
orbit-cli · orbit-web        surfaces: parse, call one exec, render
  holds: Core (opaque)         cannot name the runtime, the store or the engine

orbit-cmd                    composition: opens a Core for a selected workspace; nothing else

orbit-core                   the apps
  app/<subject>/               Command, Outcome, Error, one exec, flows, rules, ports
  app/exec.rs                  the one chokepoint: authorize, audit, run
  app/port.rs                  binds each port between apps to its implementation
  adapter/                     implements the engine's and the tool registry's host traits by calling apps
  composition/                 builds the runtime and the Core

orbit-runtime                the runtime: what every app runs against
  RuntimeKernel                stores, resolved config, paths, actor, clock, events, execution
                               no subject's operation; used only by orbit-core

orbit-engine · orbit-tools   call up through host traits they own

orbit-store                  persistence
  contract/                    the record shapes
  repository/                  one per record: its SQL or file format and row mapping,
                               reached only through snapshot.<record>() and tx.<record>()
  driver/                      one module per storage mechanism: set up once, hand back a handle
    sqlite/<database>/         the file name and the open call
      schema/                  one module per table, and mod.rs: the one ordered step list
    file/                      the task bundle layout, locks and atomic writes
```

The call path of every operation, from every entry:

```text
CLI command ─┐
HTTP handler ─┼─> app::<subject>::exec(&core, Command) ─> authorize ─> audit guard ─> flow(rt, …)
tool / MCP  ─┘                                                                        │
engine callback ─> adapter::engine_host ─> app::<subject>::<flow>(rt, …) ────────────┤
                                                                                      v
                                                       rt.store().read() / .write(|tx| tx.<record>()…)
```

## 2. Surfaces hold a `Core` and call `exec`

**Today.** A surface opens an `OrbitRuntime` through `orbit_cmd`'s `RegisteredRuntimeFactory` and calls methods on it. The CLI authorizes and audits in its own code (`crates/orbit-cli/src/entry/run.rs`, `audit_middleware.rs`). The dashboard keeps an `Arc<OrbitRuntime>` per workspace and calls runtime methods in 58 of its API files. The same operation has three paths: through `run_tool` and the tool host, by a direct method from the CLI, and by a direct method from the dashboard.

**Proposed.** `orbit-core`'s public face is three things:

```rust
pub mod app;                 // one module per subject
pub use composition::Core;   // opaque: no public field, no public method
pub use error::Error;
```

A surface handler is then always the same three lines:

```rust
let command = task::Command::Add(new_task_from(args));   // parse
let outcome = app::task::exec(&core, command)?;           // one operation
render(outcome)                                           // render
```

- `Core` is what composition returns for one opened workspace. The dashboard keeps `Arc<Core>` where it keeps `Arc<OrbitRuntime>` today. A surface cannot call a method on it, because it has none.
- `orbit-core` stops exporting `OrbitRuntime`, `orbit_runtime::*` (`runtime/mod.rs:13`) and the `orbit_store` items (`lib.rs:218–279`). The surfaces have no Cargo edge to `orbit-runtime`, `orbit-store` or `orbit-engine`, so after that a surface that names the runtime does not compile. That is a crate edge, the strongest gate there is (STD-02 §R6), and it replaces `scripts/check-cli-imports.sh` and the missing guard for `orbit-web`.
- A read model the dashboard needs (`query_task_rows`, `delivery_attention_map`, `list_job_runs`) is a read command of the app that owns the data, and its `Outcome` is the model.
- The tool path keeps its JSON entry. Its handler match, which the operation registry already has for migrated nouns, calls `app::<noun>::exec`. A surface with typed input calls `exec` directly. Both end in the same function.

## 3. Apps: one unit per subject

**Today.** `crates/orbit-core/src/application/` has about 44 sibling modules. A use case is an inherent method on `OrbitRuntime` (about 884 of them there) or a free function that takes `&OrbitRuntime`. There are no command types except where the operation registry made them.

**Proposed.** `crates/orbit-core/src/app/<subject>/`, one per subject a user can name (STD-02 §R45), shaped as in the Rust CLI template:

```text
app/task/
  mod.rs     Command, Outcome, Error, exec; re-exports what sibling apps may call
  add.rs     one flow per command: load, decide, effect, commit
  list.rs
  rule.rs    the pure decisions the flows share
  port.rs    traits this app owns for what it needs from above or outside
```

```rust
/// What the task app can be asked to do.
pub enum Command { Add(NewTask), List(Filter), Show(TaskId), /* … */ }

/// What a command produced.
pub enum Outcome { Added(Task), Listed(TaskList), Shown(TaskDetail), /* … */ }

/// Run one command: authorize it, audit it when it mutates, and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, Error> {
    app::exec::run::<TaskApp>(core, command)
}
```

- **The chokepoint** (`app/exec.rs`) is written once, as in the template: it authorizes the command against the operation's governance row, starts the audit guard for a mutating command, runs the flow, tells the guard the outcome, and maps the app's typed error into `orbit-core`'s `Error`. An app supplies only what differs. No surface authorizes or audits, and none can skip it (STD-02 §R24).
- **A flow** is a function, not a method: `pub(super) fn add(rt: &RuntimeKernel, new: NewTask) -> Result<Outcome, Error>`. It follows load, decide, effect, commit (STD-02 §R41) where it can; one that cannot says why in its doc comment.
- **`Command` and the registry.** For a noun in the operation registry, `Command` is the typed form of that noun's verb enum and parameters: the spec in `orbit-common` stays the cross-surface contract, and `Command` is what the handler builds from validated input. A noun becomes an app and a registry noun in the same piece of work (§11).
- **Module privacy does the enforcing.** A flow file is a private child module, so a sibling app can call only what `mod.rs` exports. Today the same boundary needs a script that reads `impl` blocks.
- **Which modules become apps.** The rule is STD-02 §R45: a subject a user can name is an app (`task`, `run`, `workspace`, `plugin`, `routine`, `auto_task`, `notice`, `review`, `config`, `skill`, `search`, `codebase`, `experiment`, `resident`, `friction`, `doctor`, `update`, `host`). Machinery several subjects use and that has its own identity (admission, the distributed and pull drains, crew pools, landing, the backlog forecast) is a subsystem unit beside the apps, called by them, with no `Command` of its own. Legacy readers (`tool_name_migration`, `task_bundle/migration.rs`) go to one `migrate` unit (STD-02 §R44). The complete table, one row per current module, is written into `docs/architecture/orbit-core.md` by the first task; `scripts/check-architecture-map.py` already fails when a module is missing from that page.
- **`application/contracts/`** (18 files of shapes several modules share) dissolves as its users become apps: a shape goes to the app that owns it, or stays only if two apps truly share it (STD-02 §R42, §R43).

## 4. The runtime: what every app runs against

**Today.** `RuntimeKernel` lives in `orbit-runtime` [ORB-17906]. `orbit_core::OrbitRuntime` wraps it, `#[repr(transparent)]`, with `Deref`, so that `orbit-core` can hang inherent methods on a type from another crate. Kernel mechanisms that must finish through a use case call `ApplicationPort` (20 methods), which `orbit-core` implements on a static; each method receives `&RuntimeKernel` and turns it back into `&OrbitRuntime` with `from_kernel_ref`, an `unsafe` pointer cast (20 sites, plus one in `orbit-cmd`).

**Proposed.**

- **The kernel stays in `orbit-runtime` and is the context.** STD-02 §R45 asks for exactly one named unit that every app runs against and that holds no subject's rules. A flow takes `rt: &RuntimeKernel`. Only `orbit-core` and the crates below it can name it.
- **The `OrbitRuntime` wrapper is deleted.** It exists only to carry inherent methods. Once operations are functions it carries nothing, and `Deref`, `into_kernel`, `from_kernel_ref` and the `unsafe` block go with it. `Core` holds the kernel privately.
- **The kernel holds mechanisms only.** A function belongs in `orbit-runtime` when it names no application type and needs no callback into `orbit-core`. Each `ApplicationPort` method marks a place where that is false today (task reads, orphaned-run reconciliation, crew redraw). For each, the mechanism that calls it moves up into the app that owns the use case, or receives the value it needs as an argument, and the port method is deleted. When the count is zero, the port, its static and the kernel's `application` field are deleted.
- **The kernel's own surface is grouped.** It has 278 public functions in 41 files today. The ones that remain after subject code moves up are reached through a few named handles (`rt.store()`, `rt.config()`, `rt.paths()`, `rt.clock()`, `rt.events()`, `rt.exec()`), so the type itself stays inside STD-02 §R18's size check.

This is a wide context on purpose. §12 says what that costs.

## 5. Calls between apps

**Today.** Eleven modules of `application/` have a declared order, enforced by `scripts/check-dependency-direction.sh`. A lower module that needs behaviour from a higher one owns a trait, and the higher module implements it on `OrbitRuntime`. There are 31 such traits (`TaskJobPort`, `JobDrainPort`, `AutomationReviewPort`, …). Because the implementor is the runtime every function already holds, the guard also has to derive the higher module's method names and forbid them by name.

**Proposed.**

- **One declared order over every app and subsystem unit**, in `docs/architecture/orbit-core.md`, extending the eleven-module chain (STD-02 §R39). The same script enforces it, as a plain import rule: unit A does not name unit B. The method-name derivation is removed, because a function in a private module cannot be called by name from elsewhere.
- **Downward**, an app calls what the lower app's `mod.rs` exports. It does not call the lower app's `exec` from inside a flow: that would authorize and audit twice.
- **Upward**, the port traits stay as they are; they already belong to the caller (STD-02 §R42). The implementor changes from `OrbitRuntime` to a unit struct in the higher app (`job::TaskPort`), and one file, `app/port.rs`, binds each trait to its implementor in a static table. It is the only file that names both sides.

The number of ports is not reduced by this design. §12 says why that matters.

## 6. Host traits

**Today.** Three lower crates call up into `orbit-core` through a trait each owns: `orbit-engine`'s `RuntimeHost` (153 required methods, declared by macros in five files: lifecycle 30, review 32, environment 45, dispatch 19, recovery 27), `orbit-tools`' `OrbitToolHost` (7 methods) and `orbit-runtime`'s `ApplicationPort` (§4). `RuntimeHost` is implemented once, on `OrbitRuntime`; engine entry points take `&dyn RuntimeHost` (42 sites) and inner helpers a generic `H: RuntimeHost` (388 sites). Tests implement `NullRuntimeHost`, a twin trait with a default for every method (81 implementors).

**Proposed for `RuntimeHost`.**

1. **Five traits along the five groups that already exist**: the five macro files become `RunLifecycle`, `ReviewGate`, `HostEnvironment`, `Dispatch` and `Recovery`, and `RuntimeHost` becomes their supertrait, so all 430 sites keep compiling.
2. **Values instead of getters.** A method that only returns resolved configuration (`required_validation_commands`, `data_root`) is not a capability. Those become fields of one `RunSettings` value that composition resolves and the engine receives when a run starts (STD-02 §R3). How many of the 45 environment methods are getters is measured by the task that does it.
3. **Narrower bounds, one engine module at a time.** A function's bound names only the traits it uses (`H: RunLifecycle + Dispatch`). A test double then implements what the function under test needs, and `NullRuntimeHost` shrinks with each module.
4. **The implementor is an adapter, not the runtime**: `adapter::engine_host::EngineHost<'_>` holds `&RuntimeKernel` and answers each method by calling an app or a kernel mechanism.

`OrbitToolHost` is already narrow and only changes its implementor in the same way. The MCP host trait stays implemented in the CLI and calls `exec`.

## 7. Store

**Today.** `orbit-store` has `contracts/` (32 `*Backend` traits, parameter types, and some classification and validation rules), `driver/sqlite/` (about 30 `*_store.rs` that implement those traits on `Store` and hold the SQL), `driver/file/`, `repository/` (only where two drivers meet), `workflow/` (import, export, repair) and `compose/`. `orbit-runtime`'s `OrbitStores` holds twelve `Arc<dyn …Backend>` and the SQLite `Store`. `Store` has one writer connection and a read pool; `with_transaction` is used in 44 files, about 35 driver files lock the writer directly, three stores open their own connection, and the task registry owns a second writer. `Store::open` is called from 29 places outside the crate.

**Proposed layout**, the template's, with Orbit's two storage mechanisms:

```text
orbit-store/src/
  contract/                 record shapes and query parameters; no trait, no rule
  repository/<record>.rs    one per record: its SQL or file format and its row mapping
  driver/
    sqlite/
      host/                 orbit.db: file name, open, pragmas, read pool; returns the handle
        schema/             one module per table; mod.rs is the one ordered step list
      task_index/           tasks/index.sqlite: the same shape
    file/                   task bundle layout, locks, atomic writes; returns the handle
  boundary/                 the task commit boundary: the one protocol that writes two stores
  migrate/                  import, export, repair, layout upgrade (today's workflow/)
  store.rs                  the facade: read() and write()
```

- **One way to read and one way to write.**

  ```rust
  let snapshot = rt.store().read()?;                 // one consistent read view
  let notice = snapshot.notice().get(id)?;

  rt.store().write(|tx| {                            // one IMMEDIATE transaction
      let current = tx.notice().get(id)?;
      rule::may_close(&current, actor)?;             // the caller's rule, inside the transaction
      tx.notice().close(id, now)
  })?;
  ```

  `Store::write` is the only place a write transaction begins. The writer connection is private to the driver, and a repository's writing methods take `&Tx`, so a write outside a transaction does not compile. Each of today's bypasses is examined when its record is converted; one that exists for a reason (for example an audit line that must survive a rolled-back command) stays as a named second path with the reason in its doc comment.
- **Repositories are concrete types.** `snapshot.notice()` returns the notice repository, not `Arc<dyn NoticeStoreBackend>`. The project's own storage is a concrete type until a second backend exists (STD-02 §R42). `OrbitStores`' twelve trait objects become one `Store`.
- **The store stores; it does not decide.** A repository method is named for the record (`get`, `put`, `list`, `claim_row`). An operation-named method (`admit_owner_wave`, `resolve_execution_claims`, `verify_worker_claim`, `record_task_decision_snapshots`) is split: its rule moves to the app that owns the operation and runs inside `write`, and its reads and writes stay as record methods (STD-02 §R40). The classification and validation code in `contracts/` moves to its app the same way.
- **The SQL moves from the driver to the repository.** `driver/sqlite/notice_store.rs` becomes `repository/notice.rs`. The driver keeps what sets the database up: the file name, the open call, the pragmas, the pool and the schema.
- **Schema.** `orbit.db` already has one append-only ordered list, v1 to v95. The steps are frozen; they are moved, not rewritten: each step's constant goes to `schema/<table>.rs` for the table it creates or alters, and `schema/mod.rs` keeps the list in the same order with the same numbers. A step that touches two tables lives with the table it creates, or the first it alters. The baseline step that creates many tables stays whole in `schema/baseline.rs`. The existing migration-freeze test proves the move changed no statement. `codebase_facts_store.rs`, which creates its table inside a write, gets a schema step.
- **`Store::open` is called by composition only.** The other 28 callers receive the handle.
- **The task store.** A task is files plus index rows plus reservations in `orbit.db`. One transaction cannot span them, and the existing commit boundary is the protocol that makes the write safe. It stays, as the one named place that writes more than one store; no app sequences two stores itself.
- **The other three databases** stay where they are and take the same shape: `semantic.db` in `orbit-search`, `lease-tokens.db` in `orbit-auth`, and `authority.db`, whose SQL moves from `orbit-runtime` into `orbit-store` as a third SQLite driver. The migration ledger moves down to `orbit-common`'s SQLite helpers so `orbit-search` and `orbit-auth` can use it without depending on `orbit-store`. After that there is one migration mechanism, and `rusqlite` is a dependency of `orbit-store`, `orbit-search`, `orbit-auth` and `orbit-common` only: `orbit-core` and `orbit-runtime` drop it, and Cargo enforces that.
- **Names.** Modules become singular (`contracts/` → `contract/`, `notice_store.rs` → `repository/notice.rs`). Existing tables keep their names (decision 7).

## 8. Environment

**Today.** `crates/orbit-common/src/env_vars.rs` declares 136 names with meaning, default and matching flag, and a golden test generates the reference in `docs/CONFIG.md`. That part of STD-01 §R25 is met. But there are 232 `env::var` calls and 15 `env::vars` calls across 12 crates; 144 of the 232 use a declared constant, and the rest use a local constant or a computed name. There is no capture at startup, and nothing stops a new read.

**Proposed.**

- **`env_vars.rs` stays the one declaration.** Every local `*_ENV` constant is replaced by its entry there.
- **One capture.** Each binary's `main` calls `Env::capture()` once, before logging starts. `Env` holds the declared variables as typed accessors and the full variable list for the two cases that need it: building a child process's environment (`security/child_env`, `operator_env`), and a name known only at run time (a plugin manifest naming its secret). `main` passes it to composition.
- **Resolved values flow down.** Composition turns `Env`, flags and config into resolved settings (STD-02 §R3). Code below composition receives a value: the kernel's settings, the engine's `RunSettings` (§6), the output sink's settings. It does not read the environment.
- **Test hooks** (the 17 `ORBIT_TEST_*` names) are read only behind the `test-support` feature and are marked as such in the table.
- **The gate** is `clippy.toml`: `disallowed-methods` for `std::env::var`, `var_os`, `vars`, `vars_os`, `set_var` and `remove_var`, with the one module that implements `Env::capture` exempt by a module-level `#![expect]` with its reason (STD-02 §R6).

## 9. Composition and `orbit-cmd`

**Today.** `orbit-cmd` is the composition tier. It owns `RegisteredRuntimeFactory`, which opens a runtime for a selected registered workspace, but also command groups that sequence domain and persistence calls (`doctor/`, `hosts/`, `update/`, `browser_kit/`, `workspace_removal.rs`), a deviation from STD-02 §R2 recorded in [ORB-27886].

**Proposed.** `orbit-cmd` only constructs: given `Env`, flags and a workspace selection, it returns a `Core`. Its command groups become apps in `orbit-core` (`app/doctor/`, `app/host/`, `app/update/`), which already depends on `orbit-registry`. Whether `orbit-cmd` then remains a crate is an open question (3_vision.md).

## 10. Gates

Each rule gets the strongest mechanism available (STD-02 §R6).

| Rule | End-state gate | Mechanism |
|---|---|---|
| A surface cannot name the runtime, the store or the engine | `orbit-core` exports `app`, `Core`, `Error` only; surfaces have no edge to the lower crates | crate edge and visibility |
| An app exposes `Command`, `Outcome`, `Error`, `exec`; its flows are private | private child modules; the map script checks each `app/<subject>/` has the four | visibility and `check-architecture-map.py` |
| Apps and subsystem units follow one declared order | import rule per unit | `check-dependency-direction.sh` |
| Every operation is authorized and audited once | `app/exec.rs` is the only caller of the authorization and audit functions | visibility |
| A write happens in one transaction | the writer connection is private to the driver; writing methods take `&Tx` | visibility |
| Only stores that own a database use SQLite | `rusqlite` in four manifests | crate edge |
| The store is opened once | `Store::open` disallowed outside composition | `clippy.toml` |
| The environment is read once | `std::env` readers disallowed outside `Env::capture` | `clippy.toml` |

**Ratchets while the work is in flight.** The end-state gates cannot be switched on until their count is zero, and Orbit lands many unrelated changes a day. So the first task adds one CI check with a checked-in baseline, the pattern `scripts/dashboard-css-literals-baseline.json` already uses: a count may fall, and the baseline is lowered in the same change; it may not rise. The counts: files with an `impl OrbitRuntime` block; files in each surface crate that name `OrbitRuntime`; `RuntimeHost` methods; `ApplicationPort` methods; `*Backend` traits; `Store::open` calls outside composition; writer locks outside `Store::write`; `std::env` reads outside the capture.

## 11. Order of work

Each step lands on its own, leaves every check green and changes no behaviour. Steps 1 and 2 do not depend on the runtime question and are useful alone.

0. **Decisions and ratchets.** Daniel's answers are recorded in `4_decisions.md`; the ratchet check and its baseline land; `docs/architecture/orbit-core.md` gets the table of which module becomes which app or subsystem unit, and the declared order.
1. **Environment.** `Env::capture`, the local constants folded into `env_vars.rs`, readers converted crate by crate, then the Clippy ban.
2. **Store.** First `Store::read` and `Store::write` with the private writer. Then one record at a time: its SQL to `repository/<record>.rs`, its `Backend` trait to a concrete type, its operation-named methods split. The schema files are moved in one move-only change. The other three databases follow.
3. **The surface boundary.** `Core` and `app/exec.rs` land with the first app. Surfaces switch one subject at a time. When a surface crate's count is zero its ratchet locks; when all are zero, `orbit-core` closes its exports.
4. **Inside core.** One subject at a time, `impl OrbitRuntime` blocks become functions in `app/<subject>/` and its ports are rebound in `app/port.rs`. When no block is left the wrapper and the `unsafe` cast are deleted, and `ApplicationPort` is emptied.
5. **The engine host.** The five traits, `RunSettings`, then narrower bounds one engine module at a time.

- **Why the surface before the inside.** Step 3 freezes what the outside can call to the set of commands. After it, step 4 can move anything inside `orbit-core` without touching a surface.
- **The unit of work is the subject.** A subject's registry spec, its app and its repositories are converted in the order operations-as-data already set [ORB-15932], starting with the nouns whose handlers are already single entry points (friction, auto-task, task, workflow run, search, codebase).
- **A conversion is a move.** Function bodies do not change, only where they live and what they receive. Tests are not rewritten, and the frozen surface fixtures stay as they are.
- **One writer per subject.** Two tasks never convert the same `app/<subject>/` or the same record at once; a conversion of a hot subject waits for a quiet moment or takes a path lease.

## 12. Concerns & Honest Limitations

- **Two idioms for a long time.** There are about 1,000 methods to move. Until the last subject is converted, a reader meets both `runtime.add_task(…)` and `app::task::add(rt, …)`. The ratchets make the direction visible; they do not shorten it.
- **The context stays wide.** A flow that holds `&RuntimeKernel` can reach every mechanism. The alternative, a narrow handle per app, was not chosen (decision 3). So this design stops a function from calling another subject's operation, but it does not stop it from reading any store.
- **Pure decisions are not enforced.** The shape load, decide, effect, commit is asked of each flow, and a conversion that is only a move will not produce it. Reaching STD-02 §R41 is later work per subject.
- **The ports are a symptom this does not treat.** There are 31 port traits, and `task` needs 49 methods from `job` through `TaskJobPort` alone. Rebinding the traits keeps the order honest; it does not untangle the two subjects.
- **"Move only" rests on review.** Nothing mechanical proves a moved body is unchanged. An agent that tidies while it moves can change behaviour, and the existing tests are the only net.
- **Collisions with in-flight work.** A conversion touches files that drains change every day. Small changes per subject reduce it; they do not remove it.
- **The task store keeps three stores and a protocol.** One transaction per write holds per database, not across them.
- **Existing tables keep plural names**, a recorded deviation from STD-12 §R1 if decision 7 stands.
- **`orbit-core` stays one large crate.** About 278,000 lines sit under `application/`. An acyclic set of apps makes a later split by subject possible; this design does not make one.
- **A captured environment is fixed for the life of the process.** A variable changed afterwards is not seen. That is the intent, and it is a change for any code that relied on a late read.
- **What was not measured.** The counts are searches of the source at one commit, and some include comments. Not examined: how many `RuntimeHost` methods are plain getters; the reason for each write that bypasses `with_transaction`; whether any `*Backend` trait has a second implementation in tests; how many `ApplicationPort` callers can simply receive a value.

## Task References

- [ORB-28318] — carries this proposal to Daniel's decision.
- [ORB-27886] — recorded the STD-02 deviations this design is the restructure for.
- [ORB-17906] — moved the runtime kernel into `orbit-runtime`.
- [ORB-15932] — set the noun-by-noun order the conversions follow.
- [ORB-10358] — piloted the split spec and handler table.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
