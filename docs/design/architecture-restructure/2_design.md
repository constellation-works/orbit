---
title: Architecture Restructure — Design
owner: claude
created_date: 2026-11-25
updated_date: 2026-11-26
last_validated: 2026-11-25
status: Accepted
feature: architecture-restructure
doc_role: design
type: design
summary: The proposed target shape (surfaces hold an opaque Core, apps by subject with one exec, a runtime in four layers that each add a capability, split host traits, repositories behind Snapshot and Tx, one environment capture), the gates that hold it, and the order of work.
tags: [architecture-restructure, orbit-core, orbit-store, orbit-runtime, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**", "crates/orbit-store/**", "crates/orbit-engine/src/context/hosts/**", "crates/orbit-cmd/**", "crates/orbit-common/src/env_vars.rs"]
related_features: [orbit-core, operations-as-data]
related_artifacts: [ORB-28318, ORB-28362, ORB-28364, ORB-27886, ORB-17906, ORB-15932, ORB-10358]
---

# Architecture Restructure — Design

This doc describes the approved target: every section says what existed when it was written and what it becomes, and the sections whose work has landed carry a **Status** line (§2, §4, §7, §8, §10, §11). The environment, store and runtime-layer phases are built; the surface boundary is partly built; the inside of `orbit-core` and the engine host are not started. A "Today" paragraph is the design-time state, not the current one. The decisions are in [4_decisions.md](4_decisions.md) and the alternatives in [3_vision.md](3_vision.md); the measurements behind the motivation are in [1_overview.md](1_overview.md). Counts are from `agent-main` at `b81eca83`, before the work began.

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

orbit-runtime                the runtime: four layers, each adding a capability; used only by orbit-core
  base/                        Base: env, config, paths, actor, clock, events, the process's role
  stored/                      Stored: Base + the opened workspace and the store
  host/                        Host: Stored + this machine as a place work runs
  run/                         Run: Host + the run this process carries

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
HTTP handler ─┼─> app::<subject>::exec(&core, Command) ─> authorize ─> audit guard ─> flow(rt, …)   rt is the lowest layer the flow needs
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

**Status.** `Core` (`crates/orbit-core/src/composition/core.rs`) and the chokepoint `app/exec.rs` exist, and the `friction`, `task`, `run` and `plugin` apps are the entry for their surfaces [ORB-28396] to [ORB-28399]. Workspace commands are in progress [ORB-28400]; the other families, the generic tool and MCP entries, the CLI entry and the dashboard are filed and not built [ORB-28401] to [ORB-28407], [ORB-28415]. `orbit-core` still exports `OrbitRuntime`, so a surface can still name it [ORB-28417].

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
- **A flow** is a function, not a method: `pub(super) fn add(rt: &Stored, new: NewTask) -> Result<Outcome, Error>`. Its first parameter is the lowest runtime layer it needs (§4). It follows load, decide, effect, commit (STD-02 §R41) where it can; one that cannot says why in its doc comment.
- **`Command` and the registry.** For a noun in the operation registry, `Command` is the typed form of that noun's verb enum and parameters: the spec in `orbit-common` stays the cross-surface contract, and `Command` is what the handler builds from validated input. A noun that is not yet in the registry keeps its hand-written surface and changes only what it calls; moving it into the registry stays with operations-as-data [ORB-15932].
- **Module privacy does the enforcing.** A flow file is a private child module, so a sibling app can call only what `mod.rs` exports. Today the same boundary needs a script that reads `impl` blocks.
- **Which modules become apps.** The rule is STD-02 §R45: a subject a user can name is an app (`task`, `run`, `workspace`, `plugin`, `routine`, `auto_task`, `notice`, `review`, `config`, `skill`, `search`, `codebase`, `experiment`, `resident`, `friction`, `doctor`, `update`, `host`). Machinery several subjects use and that has its own identity (admission, the distributed and pull drains, crew pools, landing, the backlog forecast) is a subsystem unit beside the apps, called by them, with no `Command` of its own. Legacy readers (`tool_name_migration`, `task_bundle/migration.rs`) go to one `migrate` unit (STD-02 §R44). The complete table, one row per current module, is written into `docs/architecture/orbit-core.md` by the first task; `scripts/check-architecture-map.py` already fails when a module is missing from that page.
- **`application/contracts/`** (18 files of shapes several modules share) dissolves as its users become apps: a shape goes to the app that owns it, or stays only if two apps truly share it (STD-02 §R42, §R43).

## 4. The runtime: layers that each add a capability

**Today.** `RuntimeKernel` lives in `orbit-runtime` [ORB-17906]. `orbit_core::OrbitRuntime` wraps it, `#[repr(transparent)]`, with `Deref`, so that `orbit-core` can hang inherent methods on a type from another crate. Kernel mechanisms that must finish through a use case call `ApplicationPort` (20 methods), which `orbit-core` implements on a static; each method receives `&RuntimeKernel` and turns it back into `&OrbitRuntime` with `from_kernel_ref`, an `unsafe` pointer cast (20 sites, plus one in `orbit-cmd`).

The kernel is one struct for every kind of process. It has 34 fields, and 14 of them are `Option`, because only some processes have them: a worker's invocation, a drain owner's transport, the workspace binding. Code asks which kind of process it is at the point of use: the worker invocation is read at 109 places in 52 files, and the coordination owner at 85 places in 45. `get_task` is the clearest case. It first asks whether the process is a worker, and if so it reads through the drain owner; it has 202 callers. So, following calls, 1,385 of the 2,925 functions in `orbit-core` that take the runtime reach a worker or drain field, 528 stop at the stores, 332 stop at configuration and paths, and 427 reach no field at all. Only 27 of them read a kernel field directly. The rest go through the kernel's 306 methods: 89 of those stop at configuration and paths, 93 at the stores, 54 at the host mechanisms, and 50 reach a worker or drain field. `orbit-runtime/src` has 32 modules at its top level, and their names do not say which of them an operation needs. §12 says how these were counted.

**Proposed.** Daniel's direction (2026-11-25): "I think we need layers of runtime. the lowest level runtime will encompass all operations. So, this lowest runtime will contain only the things that all operations need, e.g. config, env compositions. Then, we find the next layer, where only certain type of operations need […]. So, each layer can be thought of as bestowing certain capabilities to operations."

- **Four layers, each a concrete struct that holds the one below.**

  | Layer | Adds | An operation that needs it |
  |---|---|---|
  | `Base` | the captured `Env`, the resolved configuration, the paths, the actor, the clock, the event log, the process's role | every operation (`orbit config show`) |
  | `Stored` | the opened workspace and the store | one that reads or writes a record (`orbit task show`) |
  | `Host` | this machine as a place work runs: the PR forge, the load and resource probes, the admission policy, command and sandbox execution | one that starts, validates or lands work (`orbit run ship`) |
  | `Run` | the run this process carries: the drain owner's transport, the execution location, what the engine asks of a run | code inside a drain or a worker |

- **A flow names the lowest layer it needs.** `fn show(rt: &Stored, id: TaskId)`, `fn ship(rt: &Host, …)`. It reaches down with `rt.base()`. There is no `Deref` between layers: that is how the wrapper exposes everything today. It cannot reach up, because a `Stored` holds no forge. The signature tells a reader, and the compiler, what the operation can touch.
- **Layers are built on demand.** `Core` holds `Base` and builds a higher layer the first time a flow asks for it: `Command::Show(id) => show(core.stored()?, id)`. `orbit config show` opens no store, and `orbit task show` starts no resource monitor. The 14 optional fields stop being optional, because the layer that holds a field always has it.
- **The role is resolved once, in `Base`.** Whether the process is an operator's command, a drain owner, a worker or a replica checkout is decided at start from `Env` and the workspace binding, as one `Role` value (STD-02 §R3). `Stored` is then built for that role: in a worker its task access goes through the owner, in a replica coordination writes are refused, and otherwise it is the local store. A flow calls `rt.task().get(id)` and does not ask. This is the one place the design puts two implementations behind one type, and the code has them already, chosen at 109 places. Without it the layers do not hold: a task read would need what only a worker has, and nearly every operation reads a task.
- **Side capabilities are not layers.** The search index (`semantic.db`), the recovery authority (`authority.db`) and the plugin broker are needed by a few operations and do not stack: search does not need the forge, and a drain does not need search. Each is a value a layer hands out and opens on first use (`rt.search()?` on `Stored`). A capability becomes a layer only when everything above it needs it.
- **Where a field or a mechanism goes**: the lowest layer in which every operation could use it. A mechanism only one subject uses is not runtime. It moves up into that subject's app.
- **The runtime holds mechanisms only.** A function belongs in `orbit-runtime` when it names no application type and needs no callback into `orbit-core`. Each `ApplicationPort` method marks a place where that is false today (task reads, orphaned-run reconciliation, crew redraw). For each, the mechanism that calls it moves up into the app that owns the use case, or receives the value it needs as an argument, and the port method is deleted. When the count is zero, the port, its static and the `application` field are deleted.
- **The `OrbitRuntime` wrapper is deleted.** It exists only to carry inherent methods. Once operations are functions it carries nothing, and `Deref`, `into_kernel`, `from_kernel_ref` and the `unsafe` block go with it.

### The crate's structure

Daniel (2026-11-25): "can you also define the orbit-runtime crate structure? the architecture should be clear enough to suggest this layers of runtime". The top level of `orbit-runtime/src` becomes the four layers and nothing else, so a listing of the directory is the design:

```text
orbit-runtime/src/
  lib.rs                   exports Base, Stored, Host and Run; nothing else
  base/                    what every operation has
    mod.rs                 Base, and Base::open(env, flags)
    config.rs              the loader and the resolved settings
    path.rs                global and workspace paths
    actor.rs               who is acting
    role.rs                operator, drain owner, worker or replica: resolved once
    event.rs               the event log
    policy.rs              authorization policy, as data
  stored/                  Base + the opened workspace and its records
    mod.rs                 Stored, and Stored::open(base)
    workspace/             binding, catalog, claim
    task_access.rs         local, through the owner, or refusing writes: chosen by the role
    audit/                 the audit writers
    search.rs              side capability: the search index
    recovery_authority/    side capability: the isolated authority store
  host/                    Stored + this machine as a place work runs
    mod.rs                 Host, and Host::open(stored)
    forge.rs               the PR forge
    signal.rs              host signals and load
    resource/              the resource monitor, profiles and forecast
    admission.rs           the cached admission policy
    exec/                  command execution, the git sandbox, scripts, tool execution
    plugin/                side capability: discovery, grants, broker, secrets
  run/                     Host + the run this process carries
    mod.rs                 Run, and Run::open(host)
    transport.rs           the drain owner's transport and worker routing
    location.rs            where automation executes
    crew/, cost.rs         what the engine asks of a run
    recovery.rs            recovery decisions
```

Where today's 32 modules go. This mapping is by module name; the task that does the move confirms it file by file.

| Today | Goes to |
|---|---|
| `orbit_runtime/{config,accessors,identity}.rs`, `context/{settings,actor,state}.rs`, `config_path.rs`, `paths.rs`, `cwd.rs`, `resolve.rs`, `event_bus.rs`, `authorization.rs` | `base/` |
| `context/stores.rs`, `orbit_runtime/persistence.rs`, `workspace/`, `audit/`, `recovery_authority/` | `stored/` |
| `host_resource/`, `host_signal.rs`, `orbit_runtime/{host,admission_policy}.rs`, `command_exec.rs`, `tool_exec.rs`, `git_sandbox.rs`, `scripts.rs`, `assets.rs`, `plugin/`, `task/pr_forge*.rs` | `host/` |
| `worker_coordination.rs`, `engine/`, `recovery_decision.rs`, `run_input.rs`, `upgrade_handover.rs` | `run/` |
| `builder.rs`, `orbit_runtime/{mod,build}.rs` | the four `open` functions; `RuntimeKernel` is deleted |
| `application_port.rs`, `application_consent/`, the rest of `task/` (locks, reservations, the provider hold), `friction.rs`, `cost.rs`, `activity_catalog.rs`, `mutation.rs` | not placed. Each is named for a subject, so each file is either a mechanism, which goes to a layer, or a use case, which goes up to its app in `orbit-core` |

- **Direction inside the crate.** `base` names none of the others, `stored` names only `base`, `host` only those two, and `run` any of them. `scripts/check-dependency-direction.sh` gets the four as one more declared chain. A crate for each layer would make the same rule a crate edge, which is the stronger gate (STD-02 §R6). It is not proposed: Daniel's direction keeps the runtime in one crate, and the script already holds the same kind of chain inside `orbit-core`.
- **How it gets there.** `RuntimeKernel` stays while the work is in flight, as a struct that holds the four layers, so the 1,075 places that take `&OrbitRuntime` keep compiling. First the fields are carved into the four structs and the files moved, with no caller changed. Then the role checks are folded into `Role` and the task access. Then, as each subject becomes an app (§3), its flows take the lowest layer they need. When nothing takes the whole kernel, it and the wrapper are deleted, and layers start being built on demand.

**Status.** The four layer structs exist [ORB-28387], laid out as `base/`, `stored/`, `host/` and `run/` under `orbit-runtime/src` [ORB-28388], [ORB-28389]. The role is resolved once in `Base` [ORB-28390] and a worker's task reads are chosen when `Stored` is built [ORB-28391]. Not yet done: the coordination writes and remaining worker-role checks [ORB-28392], [ORB-28393], the nine top-level modules still outside the layers (sorting them is in review [ORB-28394]), and emptying `ApplicationPort` [ORB-28395]. The wrapper, `from_kernel_ref` and on-demand layers wait for [ORB-28455].

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
4. **The implementor is an adapter, not the runtime**: `adapter::engine_host::EngineHost<'_>` holds `&Run` and answers each method by calling an app or a runtime mechanism.

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

**Status.** Built. `orbit-store` has `contract/`, one repository per record under `repository/`, `driver/sqlite/{host,task_index,authority}/` each with its schema, `boundary/`, `migrate/` and the `Store::read` and `Store::write` facade [ORB-28372] to [ORB-28380], [ORB-28381], [ORB-28384]. The backend traits are deleted and the store is closed [ORB-28386]. One migration ledger serves the SQLite files [ORB-28385], and the store is opened in composition only (Clippy disallows `Store::open` elsewhere). The task commit boundary is one module [ORB-28382] and the ownership of the task-index coordination methods was decided [ORB-28518]. Follow-ups: the writer-lock ratchet was restored after the schema split [ORB-28488], and the routine scheduler's store open goes through composition [ORB-28670]. Not yet true: `orbit-core` and three surface crates still list `rusqlite`, which the end state drops.

## 8. Environment

**At design time.** `crates/orbit-common/src/env_vars.rs` declared 136 names with meaning, default and matching flag. A golden test generated the reference in `docs/CONFIG.md` for STD-01 §R25. Across 12 crates, 232 `env::var` calls and 15 `env::vars` calls read directly from the process. Of the single-variable reads, 144 used a declared constant and the others used a local constant or computed name.

**Implemented** [ORB-28365] to [ORB-28371]. Resolved configuration carries the startup snapshot into the runtime's base layer. Root discovery, engine launcher and Git policy, tool dispatch, and the publication Git runner receive the snapshot or values resolved from it. The architecture ratchet held the remaining direct process-environment calls while conversion was in flight; the Clippy gate below now enforces the production boundary [ORB-28371]. Follow-ups: calls that fetch the whole environment instead of receiving a value are ratcheted [ORB-28646], and the CLI entry resolves the root, worker invocation and actor once [ORB-28647].

**Proposed.**

- **`env_vars.rs` stays the one declaration.** Every local `*_ENV` constant is replaced by its entry there.
- **One capture.** Each binary's `main` calls `Env::capture()` once, before logging starts. `Env` holds the declared variables as typed accessors and the full variable list for the two cases that need it: building a child process's environment (`security/child_env`, `operator_env`), and a name known only at run time (a plugin manifest naming its secret). `main` passes it to composition.
- **Resolved values flow down.** Composition turns `Env`, flags and config into resolved settings (STD-02 §R3). Code below composition receives a value: `Base`'s settings, the engine's `RunSettings` (§6), the output sink's settings. It does not read the environment.
- **Test hooks** (the 17 `ORBIT_TEST_*` names) are read only behind the `test-support` feature and are marked as such in the table.
- **The gate** is `clippy.toml`: `disallowed-methods` for `std::env::var`, `var_os`, `vars`, `vars_os`, `set_var` and `remove_var`, with the one module that implements `Env::capture` exempt by a module-level `#![expect]` with its reason (STD-02 §R6).
- **End state.** No function returns the environment from a static: `main` captures it, the entry passes it to composition, and `composition_env`, `captured_env` and `CAPTURED_ENV` do not exist once the restructure is built. Until then the ratchet holds `env_global_accessor_calls` and `env_process_env_method_calls` so they can only fall.

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
| A flow reaches its layer and the ones below, nothing above | a layer struct holds only the layers below it; there is no `Deref` between them | visibility |
| The runtime's layers follow one order | `base`, `stored`, `host`, `run` as a declared chain | `check-dependency-direction.sh` |
| The process's role is asked once | the worker invocation and the coordination owner are private to `base/role.rs` and `stored/task_access.rs` | visibility |
| The store is opened once | `Store::open` disallowed outside composition | `clippy.toml` |
| The environment is read once | `std::env` readers disallowed outside `Env::capture` | `clippy.toml` |
| No function returns the environment from a static | `composition_env`, `captured_env` and `CAPTURED_ENV` do not exist | architecture ratchet (`env_global_accessor_calls`, `env_process_env_method_calls`) |

**Ratchets while the work is in flight.** The end-state gates cannot be switched on until their count is zero, and Orbit lands many unrelated changes a day. So the first task adds one CI check with a checked-in baseline, the pattern `scripts/dashboard-css-literals-baseline.json` already uses: a count may fall, and the baseline is lowered in the same change; it may not rise. The counts: files with an `impl OrbitRuntime` block; files in each surface crate that name `OrbitRuntime`; `RuntimeHost` methods; `ApplicationPort` methods; `*Backend` traits; `Store::open` calls outside composition; writer locks outside `Store::write`; calls of the global environment accessors; `.process_env()` method calls; places that read the worker invocation or the coordination owner; top-level modules of `orbit-runtime` outside the four layers.

**Status of the gates.** The ratchet check and its baseline are in place (`scripts/check-architecture-ratchet.py`, `scripts/architecture-ratchet-baseline.json`) [ORB-28362], and the Clippy ban on environment reads is a gate [ORB-28371]. The other gates in the table wait for their phase.

## 11. Order of work

Each task lands on its own, leaves every check green and changes no behaviour. The work was filed as 95 tasks in eight phases, each tagged `architecture-restructure` and `architecture-restructure:phase-N`; follow-ups filed while building are listed under the table. Phases 0, 1 and 2 are done, phase 3 is nearly done, phase 4 is in progress and phases 5 to 7 are not started. The decided order (ratchets, environment, store, the surface boundary, the inside of core, the engine host) is the order in which the gates close. The tasks' dependencies say what must finish first. Two tasks with no dependency between them can still declare the same file (see the one-writer rule below).

| Phase | What | Tasks | Waits for | State |
|---|---|---|---|---|
| 0 | Ratchets and the app table | ORB-28362, ORB-28364 | nothing | done |
| 1 | Environment | ORB-28365 to ORB-28371 | ORB-28365 to ORB-28370 wait for the ratchet check (ORB-28362); ORB-28367 to ORB-28370 also wait for ORB-28365 and ORB-28366; ORB-28371 waits for ORB-28367 to ORB-28370 | done |
| 2 | Store | ORB-28372 to ORB-28386 | the ratchet check, except ORB-28373 (no dependencies) and ORB-28384 (waits for ORB-28373 only) | done |
| 3 | Runtime layers | ORB-28387 to ORB-28395 | the carve waits for nothing; the file layout waits for phases 1 and 2 | ORB-28387 to ORB-28391 done; ORB-28394 in review; ORB-28392, ORB-28393 and ORB-28395 backlog |
| 4 | The surface boundary | ORB-28396 to ORB-28407, ORB-28415 to ORB-28417 | the ratchet check, the app table and ORB-28365 (ORB-28416 waits for it) | ORB-28396 to ORB-28399 done; ORB-28400 in progress; the rest backlog |
| 5 | Inside core | ORB-28418 to ORB-28454 | phases 2, 3 and 4 complete | proposed |
| 6 | The engine host | ORB-28408 to ORB-28414 | the ratchet check, except ORB-28408 (no dependencies) | proposed |
| 7 | Finish | ORB-28455 to ORB-28457 | phases 5 and 6 | proposed |

Follow-ups filed while building: [ORB-28488] restored the writer-lock ratchet after the schema split; [ORB-28518] decided who owns the task-index coordination methods; [ORB-28542] routed task coordination through the task commit boundary; [ORB-28646] ratcheted calls that fetch the whole environment; [ORB-28647] resolves the root, worker invocation and actor once in the CLI entry (done); [ORB-28670] routed the routine scheduler's store open through composition; [ORB-28616] will route claim handoff and consent commands via `exec` (phase 4, backlog); [ORB-28962] will expose run metrics through `app::run` and [ORB-28963] will move core metrics into it (phase 5, proposed).

0. **Ratchets.** The ratchet check and its baseline land [ORB-28362]. `docs/architecture/orbit-core.md` gets the table of which module becomes which app or subsystem unit, and the declared order [ORB-28364].
1. **Environment.** `Env::capture`, the local constants folded into `env_vars.rs`, readers converted by crate tier, then the Clippy ban.
2. **Store.** First `Store::read` and `Store::write` with the private writer, and the schema files moved in one move-only change. Then one family of records at a time: its SQL to `repository/<record>.rs`, its `Backend` trait to a concrete type, its operation-named methods split. Then the store is opened in composition only, the other databases take the same layout and ledger, and the backend traits are deleted.
3. **Runtime layers.** The kernel's fields are carved into the four layers inside `orbit-runtime`, with `RuntimeKernel` still holding them, so no caller changes [ORB-28387]. The files move to the layout of §4 after the store and the environment are done, because those phases edit the same files at their old paths. Then the role is resolved once in `Base`, the role checks are folded into `Role` and the task access, and `ApplicationPort` is emptied.
4. **The surface boundary.** `Core` and `app/exec.rs` land with the friction app [ORB-28396]. The other surface families switch side by side, then the generic tool and MCP entries, the CLI entry and the dashboard. When no surface names `OrbitRuntime`, `orbit-core` closes its exports [ORB-28417].
5. **Inside core, in two passes.**
   - **Pass A moves.** Each module's `impl OrbitRuntime` blocks become functions in `app/<subject>/` or `unit/<name>/` that still take `rt: &OrbitRuntime`, and its ports are rebound in `app/port.rs`. The eleven first-wave work items (ORB-28418 to ORB-28428) cover more than eleven modules, so a work item is not a module. Dependency readiness does not establish file independence: all eleven share the same predecessors, yet every pair declares `app/port.rs` and `scripts/check-dependency-direction.sh`, and no dependency orders any pair. They can run side by side only once each pair that shares a file is ordered by a dependency, and the filed dependencies do not do that yet. The chain (ORB-28429 to ORB-28450) then moves from the top down, one module at a time, with `task` last. The last task dissolves `contracts` and deletes `application/` [ORB-28450].
   - **Pass B narrows.** A report script gives the lowest layer each function reaches. Four tasks change the parameter to that layer, lowest first: `&Base`, `&Stored`, `&Host`, `&Run` [ORB-28451] to [ORB-28454].
6. **The engine host.** The five traits, `RunSettings`, an adapter struct, then narrower bounds one engine module at a time, and `NullRuntimeHost` deleted.
7. **Finish.** `RuntimeKernel`, the wrapper and the `unsafe` cast are deleted and each process builds only the layers it needs [ORB-28455]. The remaining ratchets become gates, and these docs are marked built.

- **Why the surface before the inside.** Phase 4 freezes what the outside can call to the set of commands. After it, phase 5 can move anything inside `orbit-core` without touching a surface.
- **Why two passes.** A function can take `&Stored` only when everything it calls takes `&Stored` or lower, and modules call each other in both directions through ports. Moving and narrowing in one change would force each task to narrow functions in modules it does not own. Pass A needs no agreement between modules. Pass B goes lowest layer first because a function that reaches only `Base` calls only functions that reach only `Base`.
- **Why top-down in pass A.** Nothing below a module in the declared order calls it except through a port, so moving the top module first changes the fewest call sites.
- **Why the layers are carved early.** The carve changes only `orbit-runtime` and keeps every accessor. Folding the role checks is the one part that is not a move (§12), and it has to be done before pass B can sort flows into the four layers.
- **The unit of work differs by phase:** a family of records in the store, a surface family at the boundary, a module inside core. A subject's repositories, its entry and its functions are therefore converted in three tasks, not one.
- **A conversion is a move.** Function bodies do not change, only where they live and what they receive. Tests are not rewritten, and the frozen surface fixtures stay as they are.
- **One writer per set of files.** Tasks that may run side by side declare disjoint files. Readiness is not disjointness: two tasks with the same predecessors can declare the same file, and a dependency must order any two that share one.
- **Decided at filing, not measured.** The app table in [ORB-28364] is a starting table that the task checks against the code. The two passes and the place of the runtime layout come from reading the code, not from a trial conversion.

## 12. Concerns & Honest Limitations

- **Two idioms for a long time.** There are about 1,000 methods to move. Until the last subject is converted, a reader meets both `runtime.add_task(…)` and `app::task::add(rt, …)`. The ratchets make the direction visible; they do not shorten it.
- **A layer bounds mechanisms, not records.** A flow that holds `&Stored` cannot start a process or reach the forge, but it can read every record. A narrow handle for each app would bound that too; it was not chosen (decision 3), and it can be added later for one app.
- **Nothing stops a flow from taking a higher layer than it needs.** `rt: &Run` on a read compiles. The lowest layer is a review rule, and the count of flows at each layer is reported so a drift shows.
- **Folding the role checks is not a move.** About 194 places ask the role today. Replacing them with one choice made when `Stored` is built changes code paths that drains depend on, and whether a worker's writes fit the same task access as its reads is not examined (`route_worker_tool` routes whole tool calls, at 9 places).
- **The four layers are read from field names and a name-based call graph.** Until the role checks are folded, calls do not sort into the four: 615 functions reach a worker or drain field and the stores but no host mechanism. The design expects them to fall to `Stored` afterwards; that is not measured. The file mapping in §4 is by module name.
- **Pure decisions are not enforced.** The shape load, decide, effect, commit is asked of each flow, and a conversion that is only a move will not produce it. Reaching STD-02 §R41 is later work per subject.
- **The ports are a symptom this does not treat.** There are 31 port traits, and `task` needs 49 methods from `job` through `TaskJobPort` alone. Rebinding the traits keeps the order honest; it does not untangle the two subjects.
- **"Move only" rests on review.** Nothing mechanical proves a moved body is unchanged. An agent that tidies while it moves can change behaviour, and the existing tests are the only net.
- **Collisions with in-flight work.** A conversion touches files that drains change every day. Small changes per subject reduce it; they do not remove it.
- **The task store keeps three stores and a protocol.** One transaction per write holds per database, not across them.
- **Existing tables keep plural names**, a recorded deviation from STD-12 §R1 if decision 7 stands.
- **`orbit-core` stays one large crate.** About 278,000 lines sit under `application/`. An acyclic set of apps makes a later split by subject possible; this design does not make one.
- **A captured environment is fixed for the life of the process.** A variable changed afterwards is not seen. That is the intent, and it is a change for any code that relied on a late read.
- **How the layer counts were made.** A script read every non-test function in `orbit-core` and `orbit-runtime` that is a method of the runtime or takes it, recorded which kernel fields it reads, and followed calls by name. Inherent methods of one type have unique names, so those calls resolve exactly; a free function is followed only when its name is defined once. Calls through a port or another trait object are not followed, so the figures undercount there.
- **What was not measured.** The counts are searches of the source at one commit, and some include comments. Not examined: whether an operation needs the store without a workspace; how many `RuntimeHost` methods are plain getters; the reason for each write that bypasses `with_transaction`; whether any `*Backend` trait has a second implementation in tests; how many `ApplicationPort` callers can simply receive a value.

## Task References

- [ORB-28318] — carried this proposal to Daniel's approval.
- [ORB-28362] — added the ratchet check and its baseline; the first of the filed tasks (`orbit task list --tag architecture-restructure`).
- [ORB-28364] — wrote the app and unit table.
- [ORB-28365] — added the one environment capture in `main`.
- [ORB-28371] — banned environment reads outside the capture with Clippy; the last of the environment tasks.
- [ORB-28372] — added `Store::read` and `Store::write`, the first store task.
- [ORB-28380] — converted the job run record, the last of the record conversions.
- [ORB-28381] — gave the task index its own SQLite driver.
- [ORB-28382] — named the task commit boundary as one module.
- [ORB-28384] — moved `authority.db` under `orbit-store`.
- [ORB-28385] — shared one migration ledger across the SQLite files.
- [ORB-28386] — deleted the store's backend traits and closed it.
- [ORB-28387] — carved the kernel into the four layer structs.
- [ORB-28388] — laid out the base and stored layers of `orbit-runtime`.
- [ORB-28389] — laid out its host and run layers.
- [ORB-28390] — resolved the process role once in `Base`.
- [ORB-28391] — chose a worker's task reads when `Stored` is built.
- [ORB-28392] — will choose coordination writes from the role.
- [ORB-28393] — will fold the remaining worker-role checks.
- [ORB-28394] — is sorting the runtime modules named for a subject.
- [ORB-28395] — will empty the kernel's `ApplicationPort`.
- [ORB-28396] — added `Core`, `exec` and the friction app.
- [ORB-28399] — routed plugin commands through `app::plugin`, the last of the four apps built.
- [ORB-28400] — is routing workspace commands through `app::workspace`.
- [ORB-28401] — will route config, skill and audit commands through `exec`.
- [ORB-28407] — will remove authorization and audit from the CLI entry.
- [ORB-28415] — will give the dashboard a `Core` instead of the runtime.
- [ORB-28417] — will close `orbit-core`'s exports to `app`, `Core` and `Error`.
- [ORB-28450] — will end pass A by dissolving `contracts` and deleting `application/`.
- [ORB-28451] — will start pass B by narrowing flows to `&Base`.
- [ORB-28454] — will end pass B by narrowing the remaining flows to `&Run`.
- [ORB-28455] — will delete the wrapper and build layers on demand.
- [ORB-28488] — restored the writer-lock ratchet after the schema split.
- [ORB-28518] — decided who owns the task-index coordination methods.
- [ORB-28542] — routed task coordination through the task commit boundary.
- [ORB-28616] — will route claim handoff and consent commands via `exec`.
- [ORB-28646] — ratcheted calls that fetch the whole environment instead of receiving a value.
- [ORB-28647] — resolved the root, worker invocation and actor once in the CLI entry.
- [ORB-28670] — routed the routine scheduler's store open through composition.
- [ORB-28962] — will expose run metrics through `app::run`.
- [ORB-28963] — will move core metrics into the run app.
- [ORB-27886] — recorded the STD-02 deviations this design is the restructure for.
- [ORB-17906] — moved the runtime kernel into `orbit-runtime`.
- [ORB-15932] — set the noun-by-noun order the conversions follow.
- [ORB-10358] — piloted the split spec and handler table.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
