---
title: Architecture Restructure — Decisions
owner: claude
created_date: 2026-11-25
updated_date: 2026-11-26
last_validated: 2026-11-26
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

Daniel approved the design as proposed on 2026-11-25: "let's take it easy and go with your current design. design approved" [ORB-28318]. Each entry below is one or more of the eleven decisions in [1_overview.md](1_overview.md); the options that were not chosen are in [3_vision.md §1](3_vision.md#1-open-questions). An entry recorded by a later task was decided while building, from the code.

- **[Surfaces hold an opaque `Core` and call one `exec`](#surfaces-hold-an-opaque-core-and-call-one-exec)** — Accepted.
- **[Operations are functions in one app per subject](#operations-are-functions-in-one-app-per-subject)** — Accepted.
- **[The runtime is four layers, and the role is resolved once](#the-runtime-is-four-layers-and-the-role-is-resolved-once)** — Accepted.
- **[The engine's host trait splits along its five groups](#the-engines-host-trait-splits-along-its-five-groups)** — Accepted.
- **[Concrete repositories behind one read and one write path](#concrete-repositories-behind-one-read-and-one-write-path)** — Accepted.
- **[Five SQLite files, one layout and one migration mechanism](#five-sqlite-files-one-layout-and-one-migration-mechanism)** — Accepted.
- **[Existing tables keep their plural names](#existing-tables-keep-their-plural-names)** — Accepted.
- **[One environment declaration and one capture](#one-environment-declaration-and-one-capture)** — Accepted.
- **[Ratchets first, and the surface boundary before the inside of core](#ratchets-first-and-the-surface-boundary-before-the-inside-of-core)** — Accepted.
- **[A worker's routed tool calls get their own path in `Run`](#a-workers-routed-tool-calls-get-their-own-path-in-run)** — Accepted.

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

## Roll out STD-12 declaration and literal order

**Recorded:** 2026-11-26 · [ORB-28969]

The root and desktop workspaces use the template groups for
[STD-12@4 §R20 and §R23](../../standards/STD-12-source-style/STD-12.md#rust-profile).
Clippy checks declarations, tests-last placement and local struct literals.
The root production and folded test builds reject compiler warnings through
[the Cargo diagnostic checker](../../../scripts/check-rust-source-order.py).
Desktop uses the same Clippy configuration with its native Linux and macOS
`-D warnings` gate.

### Temporary deviation

The Linux root inventory at `85d04297c` plus the dashboard declaration fix
contains 2,074 declaration and 441 literal findings across 1,420 files.
It combines distinct diagnostic occurrences from production and all-targets
Clippy, including two declaration pairs specific to production. The desktop
inventory found 11 declaration and four literal occurrences on Linux.
Those were corrected along with the out-of-line desktop test module placement
and the macOS installer declarations.

The [root baseline](../../../scripts/rust-source-order-baseline.tsv) admits
only the recorded occurrences. The checker keys them by file, lint and a
fingerprint of the reported declaration pair or literal fields. Counts permit
line shifts and reject additional occurrences with the same fingerprint.
The PR guard rejects baseline growth against the merge base. Module and import
findings, test-module findings and unrelated warnings remain strict. A child
module that relies on an earlier lexical macro can use the reasoned, scoped
`expect` in §R20.

| Cleanup scope | Declarations | Literals | Task |
| --- | ---: | ---: | --- |
| Types | 196 | 12 | [ORB-29006] |
| Common | 129 | 37 | [ORB-29018] |
| Agent, auth, automation, clock, config, exec, policy, registry and search | 182 | 17 | [ORB-29019] |
| Command facade, MCP and tools | 169 | 55 | [ORB-29021] |
| Store and runtime | 216 | 62 | [ORB-29024] |
| Engine source | 183 | 41 | [ORB-29026] |
| Engine test and example targets | 113 | 8 | [ORB-29027] |
| Core application task operations | 114 | 50 | [ORB-29029] |
| Core application job operations | 97 | 24 | [ORB-29031] |
| Core application admission through experiment | 177 | 39 | [ORB-29033] |
| Remaining core application domains | 132 | 43 | [ORB-29035] |
| Core adapters, app, bootstrap, runtime and tests | 54 | 23 | [ORB-29036] |
| CLI | 174 | 22 | [ORB-29037] |
| Web | 138 | 8 | [ORB-29038] |

Remove a row when its occurrence is fixed. Preserve the evaluation order
of initializer expressions before reordering named fields. Retire the
baseline once these batches leave it empty.

### Source order and review coverage

**Recorded:** 2026-11-26 · [ORB-29039]

[The source guard](../../../scripts/check-rust-module-order.py) supplements
Clippy for STD-12@4 §R20. It reads file and inline-module scopes in `crates/`
and `desktop/`, including underscore-prefixed items and written declarations
in inactive platform and feature branches. Put test-only modules last whether
their bodies are inline or in another file. The guard recognizes direct
`cfg(test)` and positive `all`/`any` combinations that require `test`.
Treat alternative cfg branches as part of the same written sequence. For
example, group platform-specific types before the free functions.

The repository guard job compares the working tree against the PR merge base
(`CI_TEST_BASE`, with the `agent-main` merge-base fallback). It rejects new
findings, including in added files. It tracks existing occurrences by module
scope and declaration pair, so line shifts and detected renames preserve the
temporary debt allowance. Checked-in source labeled `@generated` in the
leading comments is exempt. Clippy retains the existing strict module/import
and tests-last gates, as well as the root compiler baseline.

A module-scope `#![expect(clippy::arbitrary_source_item_ordering, reason = "...")]`
permits a lexical `macro_rules!` definition before a child module or its
matching public macro re-export. Ordinary imports still follow the child
declarations. The guard checks other inversions in that scope and checks
tests-last placement. During review, confirm the named
child actually uses the macro in lexical scope or the re-export names the
local macro, and keep the expectation in the smallest affected module.

[The shared checker candidate](https://github.com/constellation-works/orbit/pull/11375)
parses source for §R21 impl adjacency and §R22 member ordering. §R20 group
and test-module ordering and §R23 type resolution are outside its rule set.
The complementary guard uses the repository's Rust tokenizer and leaves the
shared checker and its pins independent.

The [seeded tests](../../../scripts/test-rust-source-order.py) reproduced
these results under Clippy 1.98.1.

| Seed | Clippy | Source guard |
| --- | --- | --- |
| Function after out-of-line test module | Silent | Reports |
| Module after inline test module | Silent | Reports |
| Function after inline test module | Reports | Reports |
| Misplaced underscore-prefixed constant or function | Silent | Reports |
| Type after function in macOS/Windows/feature-gated branch | Silent on default Linux, reports with the feature | Reports in either build |
| Constant emitted by an external macro after a type | Silent | Review generated item placement |
| Reversed cross-crate literal or local literal emitted by a macro | Silent | Review literal order |
| Reversed written literal of a local struct | Reports | Covered by Clippy |

Apply these review rules for the parts of STD-12@4 §R20 and §R23 where
accurate diagnostics need expansion, type resolution or another build:

- **§R20 macro-generated items.** When a change moves or adds an item-producing
  macro, inspect the invocation and emitted items in their containing module.
  Follow `include!`, cfg-dispatch macros and attribute macros to the source or
  inspect the expansion for the affected configuration. Check generated
  declarations against the surrounding groups and keep test modules last.
- **§R20 indirect test cfg.** When a changed module uses `cfg_attr` or indirect
  predicates to compile only under `test`, verify tests-last placement in its
  enclosing scope. Check the physical position of the declaration, including
  when its body is in a separate file.
- **§R23 literals outside Clippy's coverage.** For added or changed named-field
  literals that build a cross-crate type, come from a macro, or compile outside
  the linted target/feature matrix, resolve the actual type through the imports
  or re-exports. Compare the written or emitted field list, including shorthand
  fields, with the struct declaration in that configuration. When reordering
  effectful initializers, bind the values first to preserve evaluation order.
  Clippy checks compiled local structs in the existing root Linux and desktop
  Linux/macOS lint jobs.

[Clippy module order](https://github.com/rust-lang/rust/blob/1.98.0/src/tools/clippy/clippy_lints/src/arbitrary_source_item_ordering.rs),
[tests-last lint](https://github.com/rust-lang/rust/blob/1.98.0/src/tools/clippy/clippy_lints/src/items_after_test_module.rs),
[struct literal lint](https://github.com/rust-lang/rust/blob/1.98.0/src/tools/clippy/clippy_lints/src/inconsistent_struct_constructor.rs).

## A worker's routed tool calls get their own path in `Run`

**Recorded:** 2026-11 · [ORB-28393]
**Code anchors:** `crates/orbit-runtime/src/run/transport.rs`, `crates/orbit-runtime/src/stored/task_access.rs`, `crates/orbit-runtime/src/base/role.rs`

### Context

A worker's task reads [ORB-28391] and the refusal of its local coordination writes [ORB-28392] are chosen when `Stored` is built. That left open whether what a worker sends its owner fits the same task access ([3_vision.md](3_vision.md#1-open-questions), question 19). The code says it does not:

- The task access answers a read from one of two paths that return the same type: the local store or the owner's projection. A routed call is a whole tool call. It carries the caller's session, with its capabilities and whether the host itself is calling, and is checked for both before it leaves.
- Routed calls are not only task writes. They reach notices, search, friction, auto-tasks and review artifacts.
- The local half of each routed write is a use case in `orbit-core`, with lifecycle rules and audit. `Stored` could run it only by calling back into the application, which is the port this restructure empties.
- A worker's local coordination write is refused, not redirected.

### Decision

Routed tool calls do not go through the task access. `Run::claim_owner()` returns a claimed worker's `ClaimOwner`, which holds the claim's binding and sends the calls (`call`, `host_call`). Each remaining read of the worker invocation outside the runtime became one of three:

- `Stored::task().held_by_owner()`, where the check chose how a task record is read or whether a local one is written.
- `Run::claim_owner()`, where a worker sends its owner a call or needs its binding.
- `Role::is_worker()`, `claimed_run()` or `claimed_task()`, where a caller needs the fact: a refusal, or work only an owner does.

`RuntimeKernel::worker_invocation` is deleted and `Role::invocation` is private to `orbit-runtime`.

### Consequences

- A flow that sends its owner a call takes `&Run`. One that only reads tasks still takes `&Stored`.
- The engine still reads the invocation through `RuntimeHost::worker_invocation`: 18 calls in `orbit-engine` and the one implementation in `orbit-core`. The engine-host phase removes them, and `OrbitRuntime::worker_invocation` stays until then for that implementation and the drain tests.
- Cost: each site that writes keeps a branch, the owner call for a worker and the local use case otherwise. One write access behind one type would remove the branch, and needs the callback this design deletes.
- Cost: `held_by_owner()` and `is_worker()` are true in the same processes, so which of the two a site asks is a judgement made per site.
>

## Shared source-style checker gates new findings

**Recorded:** 2026-11-26 · [ORB-28985]
**Code anchors:** `scripts/check-source-style.py`, `scripts/check-source-style-diff.py`, `rustfmt.toml`

### Context

[STD-12@4](../../standards/STD-12-source-style/STD-12.md#rust-profile) names what the stable formatter cannot check: blank lines, line width, `use` groups, `rustfmt::skip` reasons, impl adjacency, member order and `super::super`. The shared checker in `constellation-works/rust-cli-template` checks those rules, but it reads every tracked Rust file and has no diff mode. Orbit already has findings for every one of them, so a whole-repository failure would block every change.

### Decision

- The checker is carried unchanged from `constellation-works/rust-cli-template` at `7539c089`: `scripts/check-source-style.py` (sha256 `edf6a6e8…`, 34,783 bytes) and `rustfmt.toml` (sha256 `a72f2301…`), which also sits at `desktop/rustfmt.toml`. The wrapper pins those digests and fails when either changes.
- `scripts/check-source-style-diff.py` runs the checker's `names` (R1) and `style` (R12, R13, R16, R19, R21, R22, R26) modes on this checkout and on the merge base of HEAD and `origin/agent-main`, or of `CI_TEST_BASE`. It fails on each finding this checkout adds. A finding's identity is its file (followed across a rename), its rule, its message and its line text. The merge base's findings with the same identity are recorded debt.
- The checker's rules are per file and per path, and a run over this repository lexes all 4,985 Rust files. So the wrapper runs the checker on a snapshot that keeps every path and `Cargo.toml` but carries text only for the files that differ from the merge base and for untouched files whose text contains `@generated`. Recorded debt in other untouched files cancels out, and the run takes about two seconds.
- A file whose leading comment says `@generated` is exempt, as STD-12 allows. The checker enforces it, and the wrapper inherits it on both sides. An empty placeholder would drop that exemption: the base would report `R1` for a generated-only directory, and the head's `R1` for its first handwritten source would cancel against that base finding as recorded debt. The wrapper carries generated text so that case stays a new finding.
- The Rust format gate is `cargo fmt --all -- --check` at the root and `cargo fmt --check` in `desktop/`. Both read the same stable profile, `edition = "2024"` and `max_width = 100`, the rustfmt default that STD-12 R11 fixes.
- `scripts/test-check-source-style-diff.py` seeds R1, R12, R22 and R26 violations in scratch repositories and expects the gate to fail, then expects the corrected fixtures to pass. It also covers a change inside recorded debt, a rename, a new file in a recorded directory, `@generated`, a pin mismatch and an unresolved base.
- `ci-guardrails.sh` runs the gate beside the Clippy source-order checker. `desktop.yml` also runs when `rustfmt.toml` changes.

### Scoped deviation: recorded debt

This change adds the gates for `STD-12@4 §R1, §R12, §R13, §R16, §R21, §R22 and §R26` and fixes none of the debt they find. At the merge base `d06374ced`, the checker reports these breaches across the 4,985 tracked Rust files:

| Rule | Breaches |
| --- | ---: |
| R1 | 1,809 |
| R12 | 8,181 |
| R13 | 3,706 |
| R16 | 251 |
| R21 | 605 |
| R22 | 1,886 |
| R26 | 2,585 |

No R19 breach exists. A change that adds one of these findings fails the gate, including on a recorded line that it rewrites. A finding leaves the inventory when its file is changed for another reason and the finding is fixed there.

### Remaining gates

| STD-12 rule | Gate now | Outstanding |
| --- | --- | --- |
| R1 | the wrapper's `names` mode | 1,809 plural names |
| R2, R14 | Clippy `min_ident_chars`, desktop only | the root does not enable it, and `allowed-idents-below-min-chars` stays empty |
| R3, R11 | `cargo fmt --check` in both workspaces | none |
| R4, R12 | the wrapper | 8,181 breaches |
| R5, R13 | the wrapper | 3,706 breaches |
| R15 | none | `clippy::wildcard_imports` and `clippy::enum_glob_use` are enabled in neither workspace |
| R16 | the wrapper | 251 breaches |
| R18 | `missing_docs` at warn in both workspaces | presence only, and 142 files in 15 crates carry `allow(missing_docs)`; the content is review |
| R19 | the wrapper | none |
| R20, R23 | Clippy `arbitrary_source_item_ordering` and `inconsistent_struct_constructor`, plus the baseline in `check-rust-source-order.py` ([ORB-28969]) | coverage gaps tracked by [ORB-29039] |
| R21, R22 | the wrapper | 605 and 1,886 breaches |
| R26 | the wrapper | 2,585 breaches |
| R6–R10, R17, R24, R25 | review only | none |

## Task References

- [ORB-28318] — carried the proposal to Daniel's approval.
- [ORB-27886] — recorded the STD-02 deviations this restructure answers.
- [ORB-28969] — adopted the STD-12 declaration and literal order gate with a shrinking root baseline.
- [ORB-29039] — added the written module-order guard and review rules for remaining lint gaps.
- [ORB-28985] — adopted the shared source-style checker as a differential gate and recorded its debt and remaining gates.
- [ORB-15932] — set the noun-by-noun order the conversions follow.
- [ORB-28391] — chose a worker's task reads when `Stored` is built.
- [ORB-28392] — chose coordination writes from the role.
- [ORB-28393] — folded the remaining worker-role checks and decided where routed tool calls belong.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
