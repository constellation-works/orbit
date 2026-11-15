---
id: STD-02-rust-architecture-and-errors
title: Rust architecture and errors — layering, visibility, typed errors, failure semantics, idiom and naming, logging, and test layout for constellation Rust projects
summary: Normative structure and failure rules for any constellation Rust repo (workspace or single-crate CLI) — dependency direction, crate splits, visibility, one definition per rule, typed thiserror errors translated at the boundary, errors that name verified causes, exhaustive matches, load-time config validation, unknown-never-reads-as-success, fail-closed vs fail-open, batch isolation, preconditions before side effects, no boolean mode parameters, Rust API naming, borrow-general signatures, reasoned and expiring lint suppressions, lossless persisted shapes, tracing, dead code, test layout — plus the copyable lint, dependency-direction and cargo-deny gates. Read before creating or restructuring a Rust project or designing how it fails.
status: active
tags: [standard, rust, architecture, errors, failure-semantics, idioms, naming, testing, lints]
version: 4
created: 2026-09-26
updated: 2026-10-28
last_validated: 2026-09-26
related: [STD-01-cli-surface, STD-03-concurrency-and-process-safety, STD-04-testing-and-verification, STD-05-security-boundaries]
---

# Rust architecture and errors

How a constellation Rust project is layered, what it exposes, how it fails, and where its
tests live. Orbit (`codebases/orbit`) is the worked example; the rules are written for a
repo that is not Orbit, including a small one. Several rules (R24–R34, and parts of R10 and
R16) are language-neutral principles about failure; they are stated portably, with the Rust
specifics as notes.

Neighbouring standards own the rest, and their rules are not repeated here:
- Concurrency, async, locking, channels, durable writes and subprocesses: STD-03
  (`STD-03-concurrency-and-process-safety`).
- The CLI's user-facing surface (flags, output streams, exit codes, how an error is
  presented, never silently dropping caller input): STD-01 (`STD-01-cli-surface`).
- Test strategy and verification beyond the layout rules R19–R21: STD-04
  (`STD-04-testing-and-verification`).
- Trust boundaries, authority and secrets: STD-05 (`STD-05-security-boundaries`).

This standard is a directory. This file holds the binding text: the rules, where they
apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule,
[`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked
examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

**Scaling.** Every rule applies to a multi-crate workspace and to a single-crate CLI. Where a
rule says *layer* or *unit*, read **crate** in a workspace and **top-level module** (for
example `src/domain/`, `src/app/`, `src/cli/`) in a single crate. A single crate expresses
dependency direction as module-import rules instead of `Cargo.toml` edges; the rules are the
same.

### Layering and structure

- **R1.** *Declared layer order.* Code in a lower layer MUST NOT import from a higher one,
  against an ordered layer model written in the project's architecture doc — foundation
  (contract types, shared mechanisms) → domain → composition → surfaces.
- **R2.** *Domain, composition, surfaces are separate.* Domain units own their data and
  logic; exactly one composition unit joins configuration, storage construction and domain
  services; surfaces (CLI parsing, HTTP/MCP handlers, rendering) MUST only parse input, call
  composition, and render the result.
- **R3.** *Resolved inputs flow down.* Domain and runtime code MUST receive resolved
  configuration and filesystem roots as values from composition, never load config or
  discover the cwd or `$HOME` itself.
- **R4.** *Mechanisms never depend on features.* Kernel units that expose reusable
  mechanisms (process, filesystem, codec, sandbox, observability helpers) MUST NOT depend on
  any domain, composition or surface unit.
- **R5.** *Contract types are I/O-free.* The foundation unit holding shared contract types
  (serde DTOs, IDs, pure constructors, narrow errors) MUST perform no I/O — no `std::fs`,
  `std::net`, `std::process`, database or HTTP — and depend on no crate that does.
- **R6.** *Direction is machine-checked.* Dependency direction MUST be enforced by a CI
  check, and a new edge changes that check and the architecture doc in the same change.
- **R7.** *Crates follow build boundaries, not taxonomy.* A new crate MUST be justified by a
  build or dependency need — compile-graph isolation, an independently consumed artifact, or
  a dependency edge that must be enforceable — not by conceptual category. A new CLI SHOULD
  start as two crates, because the domain/surface edge is exactly such an enforceable edge:
  a domain library crate that depends on no argument parser, terminal, environment or
  log-subscriber crate, and a CLI crate that holds parsing, composition, rendering and
  `main`. Any other project starts as one crate. Split further only when another such need
  appears.
- **R24.** *One definition per rule, enforced at the chokepoint.* Every validation,
  authorization and classification rule MUST have exactly one implementation, enforced in
  the shared path that every entry point crosses — CLI, HTTP or MCP handler, dashboard,
  scheduler, and any adapter added later. In practice that is the composition or domain
  operation that R2 routes surfaces through. A surface MUST NOT re-implement, pre-empt or
  bypass the rule, and two code paths that need the same rule call one function rather than
  spelling it twice.
- **R25.** *Obligations live in deterministic code.* Where a system hands work to an agent,
  a script or a person following instructions, an obligation the system later relies on (a
  record written, a field set, a check run) SHOULD be performed or verified by deterministic
  code, not left to prompt text, a README step or convention. Instructions may ask for it;
  code produces or checks it before anything depends on it.

### Visibility and dependencies

- **R8.** *Private by default.* Items MUST default to private or `pub(crate)`; `pub` is
  reserved for the deliberate cross-crate or public API, and the crate root re-exports only
  what a consumer actually needs.
- **R9.** *Workspace-declared dependencies.* In a workspace, every third-party dependency
  used by more than one member MUST be declared once in `[workspace.dependencies]` and
  referenced as `dep.workspace = true` (package `version`, `edition`, `rust-version` and
  `license` inherited the same way); a single crate pins `rust-version` in `[package]`.

### Idiom and naming

These rules bind new and changed code. An existing signature or name is brought into line
when the change touches it for another reason, not in a sweep of its own.

- **R36.** *No boolean mode parameters.* A function or method callable outside its own
  module MUST NOT take a `bool` that selects between behaviours (`render(true)`,
  `sync(path, false)`). It takes a two-variant enum named for the choice
  (`Verbosity::Summary | Verbosity::Full`), or the operation becomes two functions. A `bool`
  that is plain data — a setter's value, a field of an options struct, a value carried into
  a persisted or wire shape — is not a mode and is allowed. A private helper whose callers
  sit beside it MAY take a mode `bool`.
- **R37.** *Rust API naming.* Names SHOULD follow the Rust API Guidelines naming rules.
  Getters drop `get_` (`fn name(&self)`, `fn name_mut(&mut self)`). Conversion prefixes
  match the real cost and ownership: `as_` is a free borrow-to-borrow view, `to_` is costly
  or borrow-to-owned, and `into_` consumes `self`. Constructors are `new` and `with_*`; a
  fallible one is `try_new`, `TryFrom` or `FromStr`. A type that has `len()` has
  `is_empty()`. A type is named for what it is, not with a `Manager`, `Service`, `Helper`
  or `Util` suffix, and an abbreviation an outside reader would not know is spelled out.
- **R38.** *Signatures borrow the most general type and own only what they keep.* A
  parameter that is only read SHOULD take the most general borrowed form (`&str`, `&[T]`,
  `&Path`, `&T`, `Option<&T>`), never `&String`, `&Vec<T>`, `&PathBuf`, `&Box<T>` or
  `&Option<T>`. A function that stores or consumes an argument takes it by value rather
  than borrowing it and cloning inside. `Rc`, `Arc`, `RefCell` and `Mutex` stay out of
  signatures shared across modules unless shared ownership is the contract. A `clone()`
  whose only purpose is to end a borrow is replaced by restructuring (a narrower borrow,
  `mem::take`, `Option::take`, splitting a struct so disjoint fields borrow separately); a
  deliberate clone of non-trivial data on a hot path says so in a comment. Conversions
  implement `From`, `TryFrom` or `FromStr`, never `Into` directly or an inherent
  `from_str`. `Deref` is implemented only by smart pointers and owning collections, never
  to imitate inheritance. Absence is `Option`, never a sentinel such as `""`, `-1` or `0`
  (R29).

### Errors and domain types

- **R10.** *Typed errors per unit; classify from structure.* Each crate (single crate: each
  layer module whose callers branch on its failures) MUST define its own
  `thiserror`-derived error enum with variants callers can match; library code never
  returns `String`, `Box<dyn Error>` or `anyhow::Error`. Any code that classifies a failure,
  its own or a child process's, MUST decide from structured data — an error variant, an exit
  status, a supervisor's verdict, an `io::ErrorKind` or errno value — and MUST NOT match
  substrings of a message, stderr, a URL or an errno's text.
- **R11.** *One surface error type.* The project MUST have one uniform error type at its
  surface boundary (Orbit: `OrbitError`), kept small by boxing large variant payloads. When
  that type is part of a public API it MUST be `#[non_exhaustive]`. In a two-crate CLI
  (R7) the surface error type is crate-private in the CLI crate, so `#[non_exhaustive]` has
  nothing to protect there; the domain library's public error enum (R10) carries it instead.
- **R12.** *Translate at the boundary, once.* A typed error that crosses into the surface
  error type MUST be converted by one translator and applied as `?` or
  `.map_err(translator)?`; call sites never map variants ad hoc, and code inside a unit
  propagates the typed error unchanged. When the owning unit can depend on the surface
  type (Orbit: `OrbitError` lives in the foundation crate `orbit-common`), the translator is
  defined next to the error in its owning unit. When the surface type lives in a crate that
  depends on the owning unit, as a CLI crate over a domain library does, the owning unit
  cannot name it; there the one translator is a single `#[from]` variant or `From` impl on
  the surface type, in the surface crate.
- **R13.** *No panics on fallible paths.* Production code MUST propagate failures with `?`
  instead of `unwrap()`, `expect()`, `panic!`, `todo!` or `unreachable!`; tests are exempt,
  and a genuine invariant `expect` carries
  `#[expect(clippy::expect_used, reason = "<the invariant>")]` on the narrowest item (R35).
- **R14.** *Newtypes with validating constructors.* A primitive that carries a protocol
  contract (IDs, refs, versions, hashes, selectors) that crosses a function boundary SHOULD
  be a newtype with a private inner value and a fallible constructor (`new`/`FromStr`/
  `TryFrom`, plus `#[serde(try_from = …)]` when deserialized), not a bare `String` checked
  by an `is_valid_x(&str)` helper.
- **R26.** *Errors state verified causes, resolved inputs and a runnable remedy.* An error
  MUST assert only causes the code actually checked, and each failure mode MUST get its own
  error (variant and message), never a shared string that fits several. The message MUST
  carry the resolved inputs that decided the outcome: the path, scope, config layer or
  effective `PATH`. Any remedy it suggests MUST be safe and runnable from the surface where
  the error appears. How a surface presents the error is STD-01 §R19–R21.
- **R27.** *Exhaustive matches on domain enums.* Code that branches on a domain enum the
  project owns (a status, outcome, kind or class) MUST name every variant, with no wildcard
  or default arm, so that adding a variant fails the build at every decision it affects.
  *Rust:* no `_ =>` or catch-all binding arm on an in-workspace enum; write a predicate as
  an exhaustive `match` rather than `matches!` whenever a new variant's answer is not
  obviously "no". A foreign `#[non_exhaustive]` enum still needs its wildcard.
  *Elsewhere:* TypeScript's `never` check, Python's `typing.assert_never`.
- **R28.** *Configuration is validated at load.* Configuration and other operator-authored
  input MUST be validated when it is loaded: references resolved, numeric values checked
  against their bounds, and narrowing conversions checked (`try_from`, never `as`). A bad
  value is refused at load, naming the key, not discovered or silently wrapped at use time.
  Load and use go through the same validation function (R24).

### Failure semantics

- **R29.** *Unknown is never zero, empty, success or healthy.* When a value or a check
  cannot be obtained, the result MUST say so explicitly — `None`/`null` with a reason,
  `unavailable`, `outcome_unknown` — and MUST NOT read as 0, an empty list, "healthy",
  success or exit 0. "Could not run" and "ran and failed" are distinct outcomes. A health or
  readiness check MUST probe the real effective state (the timer is actually scheduled, the
  helper actually executes, the disk is actually writable), never only a configuration
  flag or a manager command's exit status.
- **R30.** *Distinct failure classes get distinct terminal states.* A wait, a dead end, an
  infrastructure failure and a verdict are different outcomes and MUST NOT share a terminal
  state, status or error variant. A caller can always tell "could not start" from "ran and
  was rejected", and "not yet" from "never".
- **R31.** *Fail closed on integrity, fail open on side channels, and say which.* When code
  cannot verify an authorization, sandbox, integrity or ownership fact, it MUST refuse. When
  a best-effort side channel fails (telemetry, metrics, cache refresh, cosmetic asset
  reconciliation), that failure MUST NOT fail the primary operation, and it is still
  recorded (R32). Each check MUST declare its class, in its type or a comment at the
  decision, so a reviewer can tell a deliberate fail-open from a missing error path. The
  security boundaries themselves are STD-05.
- **R32.** *One bad item is isolated, reported and counted.* In a batch, scan, listing or
  aggregate, a failing item MUST be isolated and reported with its identity and cause while
  the remaining items proceed. It MUST NOT abort the whole operation, and it MUST NOT be
  folded into a success summary or total: a run over N items with k failures reports k
  failures. An operation declared all-or-nothing instead refuses as a whole before its first
  side effect (R34).
- **R33.** *Recovery paths accept the states they exist to fix.* Resume, repair, recovery
  and cleanup paths MUST accept the partial, stale or half-applied states they were written
  for, and complete or undo them. They MUST NOT apply the forward path's strict
  preconditions and refuse. A state the path cannot identify with certainty is still
  refused, never guessed (STD-03 §R9).
- **R34.** *Preconditions before the first side effect.* An operation MUST validate every
  input and prerequisite — ids, flags, executables, capabilities, registered actions —
  before its first durable side effect, so a rejected request leaves nothing behind. A
  dry-run or preflight MUST run the same checks, in the same order, through the same code
  as the real run.

### Logging, contracts, and size

- **R15.** *`tracing`, not print.* Diagnostics MUST go through `tracing`. Only the CLI's
  single output layer, which owns stdout/stderr as the command's result channel (STD-01),
  may write to the standard streams: `print!`, `eprint!` and `dbg!` are denied everywhere,
  and outside that layer no code names `io::stdout()` or `io::stderr()` either. The one
  place that installs the log subscriber may hand it `io::stderr`; any other exception (an
  async-signal-safe write in a signal handler, say) is listed in the stream guard with its
  reason.
- **R16.** *Persisted and wire shapes are contracts.* A serde shape that is written to disk,
  a database, or another process MUST change only compatibly — new fields defaulted, a
  separate `*Document` wire type when the domain shape diverges, and a removed field or key
  warned about and ignored (or migrated) rather than silently rejected. Round trips MUST be
  lossless: a value that was written reads back as the same value, and neither a writer nor
  a deserializer fabricates a value it does not know. A missing timestamp stays absent
  (`Option`, never `#[serde(default = "Utc::now")]`), and an ambiguous record keeps its
  ambiguity instead of a guessed default.
- **R17.** *Fewest moving parts.* Dead code MUST be deleted together with its docs, tests,
  config keys and design notes; compatibility shims are kept only for an external contract
  or a persisted format (R16), and a retired path is guarded so it does not grow back.
- **R18.** *File size is a heuristic.* A source file past about 800 lines SHOULD be checked
  for multiple responsibilities and split along them (not into `part1`/`part2`); closely
  related functionality MAY stay in one file when a split would only scatter it.

### Tests

- **R19.** *Unit tests in a sibling `tests/` directory.* Unit tests for the files of a
  module MUST live in `<module>/tests/<source_file>.rs` (top-level files: `src/tests/`),
  declared once by the parent's `#[cfg(test)] mod tests;`, so they reach only
  `pub`/`pub(crate)`/`pub(super)` items; visibility is never widened solely for a test.
- **R20.** *Crate-root `tests/` is integration only.* The crate-root `tests/` directory MUST
  hold only tests that drive the crate's public API or built binary end to end.
- **R21.** *Tests exercise behaviour.* A test MUST assert what the code guarantees by running
  it; it never text-matches source, assets or UI copy (`include_str!` + `contains()`), and
  never pins operational policy owned by config or prompts (model, schedule, wording) unless
  its assertion message cites the incident it guards.

### Gates

- **R22.** *Shared lint baseline.* Every crate MUST opt into the lint table in
  [Machine checks](checks.md), enforced by CI running `cargo clippy --all-targets --
  -D warnings` so every `warn` lint fails the build.
- **R23.** *Supply-chain gate.* CI MUST run `cargo deny check` against a checked-in
  `deny.toml` that denies yanked crates, open advisories and unknown sources and allow-lists
  licenses, with every exception carrying a reason and a re-review date.
- **R35.** *Lint suppressions state a reason and expire.* A suppression MUST be
  `#[expect(<lint>, reason = "…")]` on the narrowest item that needs it, so the reason is
  reviewable and the compiler reports the suppression once the lint stops firing. `#[allow]`
  is reserved for generated code, macro output and a lint that fires on only some targets
  or `cfg`s (where an `expect` would go unfulfilled on the others), and it still carries
  `reason = "…"`. Test-only relaxations of R13 go in `clippy.toml`
  (`allow-unwrap-in-tests`, `allow-expect-in-tests`) or a reasoned
  `cfg_attr(test, allow(…, reason = "…"))`. Source never contains `#![deny(warnings)]`;
  warnings fail the build only in CI (R22). A suppression without a reason is fixed when
  its item changes. Needs `rust-version` 1.81 or later.

## Applies to

- `rust`: every rule, in every constellation Rust project, whether a multi-crate workspace
  or a single-crate CLI; all gates in [Machine checks](checks.md).
- `universal`: R24–R26, R29–R34 and R36, the classification clause of R10 and the lossless
  clause of R16 are language-neutral and bind any project; R27 and R28 wherever the
  language offers exhaustiveness checking and checked conversions.
- `cli`: R7's two-crate default, R11 and R12's downstream-surface form, and R15's stream
  guard.
- `service`: R29 (health checks), R31 and R32 bind hardest on long-running services.
- `stateful`: R16, R33 and R34 for anything that persists state across runs.

Out of scope, and optional for adopters: Orbit's per-crate stability tiers
(`[package.metadata.orbit] stability`, `codebases/orbit/scripts/check-stability.sh`) and its
product-identity marker (`codebases/orbit/ARCHITECTURE.md#product-identity`) are
Orbit-specific mechanisms. A repo with external crate consumers can adopt a stability
marker; nothing here requires it. Concurrency, async, locking, bounded channels and
subprocess safety are STD-03.

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing
`STD-02@<version> §Rn` (for example `STD-02@2 §R27`) and the reason. It never edits its
vendored copy of this standard.

Known deviations in the exemplar itself, so adopters do not copy them: Orbit has no
production private-inner validated newtype (R14) and validates identities with
`validate_*(&str)` helpers; several Orbit crates declare `tempfile = "3"` directly although
it is in `[workspace.dependencies]` (R9); `missing_docs` is allowed at most crate roots
(R22); Orbit's `satisfies_dependency` is a `matches!` predicate rather than an exhaustive
match (R27); Orbit gates none of the [adoptable lints](checks.md#adoptable-additions-beyond-orbits-baseline);
and Orbit has no stream grep guard (R15): besides its logging subscriber, `orbit-exec`
writes stderr from a signal handler and a debug tee, and `orbit-common` probes stderr for a
terminal. As of v4 (measured 2026-10-28 at `7af1c6e74`), Orbit has 459 `#[allow]`
attributes, none with a reason, and no `#[expect]` (R35); about 58 `pub`/`pub(crate)`
functions take a `bool` parameter, not yet sorted into modes and data (R36); and 20 public
`get_` getters (R37). All are grandfathered until touched.
