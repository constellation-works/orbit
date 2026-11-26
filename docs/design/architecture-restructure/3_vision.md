---
title: Architecture Restructure — Vision
owner: claude
created_date: 2026-11-25
updated_date: 2026-11-26
last_validated: 2026-11-26
status: Accepted
feature: architecture-restructure
doc_role: vision
type: design
summary: The options behind each of the restructure's eleven decisions, the questions left for the Orbit captain, and the prior work it builds on.
tags: [architecture-restructure, orbit-core, orbit-store, orbit-runtime, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**", "crates/orbit-store/**"]
related_features: [orbit-core, operations-as-data]
related_artifacts: [ORB-28318, ORB-27886, ORB-15932]
---

# Architecture Restructure — Vision

§1 lists the options behind each decision in [2_design.md](2_design.md), and the questions that are still open. Daniel approved the recommended option of all eleven on 2026-11-25 ([4_decisions.md](4_decisions.md)). §2 lists the earlier work this builds on, and what would come after.

## 1. Open Questions

### Decided by Daniel: option A of each

1. **What does a surface hold?**
   - A (recommended): an opaque `Core`, and it calls `app::<subject>::exec`. `orbit-core` exports nothing else. This is his direction ("runtime is used in orbit-core, not orbit-cli") made a compile error: the surfaces have no Cargo edge to `orbit-runtime`.
   - B: surfaces keep `OrbitRuntime` but only its application methods stay public. This needs no handle type, but every method is still callable and "one operation per handler" stays a review rule.
2. **Where do operations live?**
   - A (recommended): functions in `orbit-core/src/app/<subject>/`. Private modules then enforce the boundary between subjects, which inherent methods on one type cannot, and the `OrbitRuntime` wrapper with its `unsafe` cast has nothing left to carry.
   - B: keep inherent methods and tighten the guard script. Nothing moves, and the guard keeps deriving method names, which edges have already regrown past once ([orbit core](../orbit-core/1_overview.md) §5 records it).
3. **What does a flow run against?**
   - A (recommended, his direction of 2026-11-25): a runtime in four layers, `Base`, `Stored`, `Host` and `Run`, each a struct that holds the one below, with the process's role resolved once in `Base`. A flow takes the lowest layer it needs. The kernel already has these groups flattened into one struct: 14 of its 34 fields are optional because only some processes have them. `orbit-runtime/src` is laid out as the four layers ([2_design.md §4](2_design.md#4-the-runtime-layers-that-each-add-a-capability)).
   - B: the kernel as one wide context holding mechanisms only. It makes the conversion a pure move, and every operation can still reach what only a drain has. This was the first draft's recommendation.
   - C: a narrow handle per app, listing the stores and effects that app may use. It is the strongest boundary, and it adds a struct and its wiring for each of about forty units before any of them has shown the need. It can be added later for one app on top of A.
   - Within A he confirmed four layers in one crate with a checked module order, with search, the recovery authority and plugins as side capabilities. A finer form, where each flow lists the groups of a layer it needs as parameters, was weighed and left out: it changes every signature in a call chain during a move that should change nothing, and it can be added later for one group.
4. **How is `RuntimeHost` reduced?**
   - A (recommended): five traits along its five existing groups, configuration getters turned into one `RunSettings` value, then narrower bounds per engine module.
   - B: one trait per engine module from the start. Finer, but it means deciding every boundary before measuring which methods each module uses.
   - C: leave it. It works, and it is the smallest change. It keeps 153 methods behind every engine function and 81 test doubles alive.
5. **Does the store keep its backend traits?**
   - A (recommended): concrete repositories behind `Snapshot` and `Tx`; rules move to the calling operation. This is STD-02 §R40 and §R42, and the layout he settled for the template.
   - B: keep the traits and only move the SQL. Smaller, and it leaves operation-named methods and rules in persistence, the deviation [ORB-27886] recorded.
6. **Five SQLite databases: merge or keep?**
   - A (recommended): keep the five files, with one layout, one ordered schema list each and one migration mechanism. The files are separate for reasons that still hold: `orbit.db` and the task index are global, the search index is per workspace, the lease tokens belong to the listener user, and the recovery authority is isolated on purpose.
   - B: merge what can be merged into `orbit.db`. One file is simpler to reason about, and it is a data migration across every host with no behaviour to show for it.
7. **Existing table names.**
   - A (recommended): keep them and record the deviation from STD-12 §R1; new tables are singular. Most of about 88 tables are plural, and a rename is a schema step per table across hosts that run different versions, with every statement edited.
   - B: rename them in one ordered set of steps after the repositories exist, when each table's SQL is in one file.
   - Orbit has not adopted STD-12 yet; this answer is recorded when it does.
8. **Environment.**
   - A (recommended): keep `env_vars.rs`, add one capture in `main`, and ban other reads with Clippy.
   - B: keep reads where they are and only require the declared constant. No capture, so code below composition still reads the environment (STD-02 §R3).
9. **`orbit-cmd`.**
   - A (recommended): it only opens a `Core`; its command groups become apps.
   - B: also fold what is left into `orbit-core`'s composition and delete the crate. Decide after A, when its size is known.
10. **Does this merge with the operations-as-data plan?**
    - A (recommended): yes. The unit of work is the subject: its registry spec, its app and its repositories together, in that plan's order. STD-02 §R45's test is that a change to one subject touches one app and one repository.
    - B: two separate tracks. Each is simpler to describe, and every subject is then opened twice.
11. **Order.**
    - A (recommended): ratchets, environment, store, the surface boundary, the inside of core, the engine host.
    - B: the runtime first, since it is the largest pain. It is also the largest change, and it is easier once the surface can no longer reach inside.

### Open, for the Orbit captain

12. Which of the current `application/` modules is an app, which a subsystem unit, and what is the order between them?
13. Which writes bypass `with_transaction` for a reason that should stay?
14. Which `ApplicationPort` callers can take a value instead of calling back?
15. Does any `*Backend` trait have a second implementation that a test depends on?
16. How many `RuntimeHost` environment methods are plain getters?
17. Does any operation need the store without a workspace, or a workspace without the store? That decides whether the workspace binding sits in `Base` or `Stored`.
18. Which of the runtime modules named for a subject (`task/`, `application_consent/`, `friction.rs`, `cost.rs`) are mechanisms, and which are use cases that move up to an app?
19. Do a worker's writes fit the same task access as its reads, or do routed tool calls need their own path in `Run`? Answered: their own path ([4_decisions.md](4_decisions.md#a-workers-routed-tool-calls-get-their-own-path-in-run)) [ORB-28393].

## 2. Prior Work

### Inside Orbit

- **[Orbit core](../orbit-core/1_overview.md)** set the direction `runtime ← application ← adapter`, moved the kernel to its own crate, and introduced ports between application modules. This proposal keeps all three and changes what carries an operation: a function in an app, not a method on the runtime. It also divides the kernel into layers.
- **[Operations as data](../operations-as-data/1_overview.md)** gave each migrated operation one spec and one handler entry. This proposal says what the handler calls and what it may reach.
- **The STD-02 standing entry** ([decisions](../orbit-core/4_decisions.md#standing-against-std-025-r2-r6-r18-r39r44)) measured Orbit against §R2, §R6, §R18 and §R39 to §R44 and deferred the fixes to a restructure. Sections 3 to 9 of the design answer those deviations one by one.

### Outside Orbit

- **The Rust CLI template** (`codebases/rust-cli-template`) is the worked example of the target at small size: `app/<subject>/` with `Command`, `Outcome`, `Error` and `exec`; a runtime private to the core; a store with `contract/`, `repository/`, `driver/sqlite/schema/`; one read and one write path. Orbit takes the shape, not the kit crates.
- **STD-02** §R40 to §R45 and §R47 state the rules this shape satisfies.

### What would come after

- A flow per command in the load, decide, effect, commit shape, with its decisions as pure functions (STD-02 §R41). The restructure makes room for it; it does not do it.
- Untangling `task` and `job`, which the 31 ports between application modules point at.
- Splitting `orbit-core` by subject, if build time asks for it, once the apps are acyclic.

## 3. What May Be Distinctive

Nothing in the target is new: it is the template's shape and STD-02's rules. What is specific to Orbit is the route. The code base lands many unrelated changes every day and cannot stop for a rewrite, so the design leans on ratchets that only let the old shape shrink, on conversions that are moves, and on closing the outside boundary before moving the inside.

## 4. References

**Orbit-internal**

- [1_overview.md](1_overview.md), [2_design.md](2_design.md)
- [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) and [`docs/architecture/`](../../architecture/README.md)
- [`scripts/check-dependency-direction.sh`](../../../scripts/check-dependency-direction.sh), [`scripts/check-architecture-map.py`](../../../scripts/check-architecture-map.py)
- [Orbit core decisions](../orbit-core/4_decisions.md), [operations-as-data decisions](../operations-as-data/4_decisions.md)

**External**

- Constellation standards STD-01 §R25, STD-02 §R2, §R3, §R6, §R7, §R18, §R24, §R39 to §R45, vendored in [`docs/standards/`](../../standards/).

## Task References

- [ORB-28318] — carried this proposal to Daniel's approval.
- [ORB-27886] — recorded the STD-02 deviations deferred until a restructure.
- [ORB-15932] — set the noun-by-noun order.
- [ORB-28393] — answered question 19.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
