---
id: STD-12-source-style
title: Source style — names, vertical spacing, line width and the formatter gate, as a language-neutral core with one profile per language
summary: Normative rules for how source code reads on the page — singular module and file names, blank lines between items, a line-width limit, named bindings, file order and the formatter as the CI gate plus checks for what it misses — as a core every language follows and a profile per language (Rust first) that names its formatter, lints and checks; follow them in every constellation code repository.
tags: [standard, style, naming, formatting, rust]
version: 4
created_date: 2026-11-25
updated_date: 2026-11-26
last_validated: 2026-11-25
related: [STD-02-rust-architecture-and-errors, STD-09-project-docs]
---

# Source style

How source code reads on the page: what modules and files are named, where blank lines go, how wide a line may be, and which gates keep it that way. A formatter settles most of this, but not all: rustfmt on the stable toolchain inserts no blank line between items and leaves a statement it cannot fit unformatted while `cargo fmt --check` still passes. This standard names what the formatter must settle and adds a check for each thing it misses.

The standard is a **core** and one **profile** per language. The core (the `Core:` clusters) states what every language must settle and binds any code repository. A profile spells the core out for one language: its formatter and configuration, its lints and its check scripts, plus rules only that language needs. Rust is the first profile. A repository applies the core and only the profiles of the languages it contains. A new profile is a new `### <Language> profile` cluster; like any new rule, its rules take the next free numbers, so a cluster is found by its heading, not by a number range.

Neighbouring standards own the rest, and their rules are not repeated here:
- Rust API naming, abbreviations and boolean mode parameters: STD-02 §R36 and §R37. Panics on fallible paths: STD-02 §R13. The shared lint baseline and how a suppression is written: STD-02 §R22 and §R35.
- Markdown and project documentation: STD-09.

This standard is a directory. This file holds the binding text: the rules, where they apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule, [`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked examples, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

A MUST here is machine-checked: [`checks.md`](checks.md) names the CI gate for each. A SHOULD is reviewed.

### Core: names

- **R1.** *Singular names.* A module, source directory or source file MUST be named in the singular, for one of the things it holds or for the role it plays, never for the collection: `app/note/`, `contract/note.rs`, `driver/sqlite/`, `repository/note.rs`, `schema/note.rs`; not `apps/`, `contracts/` or `repository/notes.rs`. A database table follows the same rule (`note`, not `notes`). A name that a toolchain fixes as plural (Cargo's `tests/`, `examples/` and `benches/`) or that belongs to an external interface is exempt. Repository-level and documentation directories (`docs/`, `docs/runbooks/`, `operations/`) are out of scope.
- **R2.** *Bindings are named for what they hold.* A variable, parameter or loop variable MUST NOT be named with a single letter, closure parameters included. A generic type parameter and a lifetime may be a single letter when the letter says what it stands for.

### Core: layout

- **R3.** *The formatter is the gate.* Each profile MUST name one formatter and its checked-in configuration, and CI MUST run it in check mode on the toolchain the project builds with. A layout rule the formatter cannot enforce on that toolchain gets a check script in CI; it is not left to review.
- **R4.** *A blank line between items.* A blank line MUST separate a file's header documentation, its import block, and each top-level item, and MUST separate the members of a type (the methods of an implementation, the functions of a class).
- **R5.** *Line width.* A source line MUST NOT exceed the profile's width. The width is checked on the source as written, not only by the formatter, because a formatter may leave a line it cannot fit unformatted and still pass. A line that cannot be broken (one string literal or URL) is exempt, and the check recognises it.
- **R6.** *File order.* A source file SHOULD read in this order: header documentation, the declarations of its child modules, imports, constants, types, functions, and its tests last.
- **R7.** *Steps a reader can see.* Within a function, a blank line SHOULD separate the logical steps, and an operation SHOULD stay beside the handling of its error, not several lines below it.
- **R24.** *One subject per file.* A source file SHOULD hold one subject, one main type with its small satellites (its error, its options, its builder) or one group of functions on one subject, and be named for it. A file that needs a divider comment between sections SHOULD be split.
- **R25.** *Callers before callees.* Among a file's free functions, and among its private methods, the entry point SHOULD come first and each helper SHOULD follow the function that calls it.

### Core: shape

- **R8.** *A guarantee gets a name.* A step that carries a guarantee its caller relies on SHOULD be its own named function, so the name states the guarantee: for example, creating a missing key where a concurrent creator must win and both callers read the winning key. A step that carries no such guarantee stays inline; this is not a reason to extract a helper every few lines.
- **R9.** *No unnamed tuple of three or more.* A function SHOULD NOT return an unnamed tuple of three or more values; it returns a named type.
- **R10.** *Embedded SQL is laid out and its limits are named.* A SQL statement longer than one short line SHOULD be written across lines with its clauses indented, and a literal limit or size in it (a row cap, a batch size, a timeout) SHOULD be a named constant.

### Rust profile

- **R11.** *rustfmt at width 100.* The Rust formatter is rustfmt, run in CI as `cargo fmt --all --check` on the stable toolchain. `max_width` MUST be 100, rustfmt's default; a checked-in `rustfmt.toml` sets only options that are stable. This is R3 and R5's width for Rust.
- **R12.** *Blank lines are checked by a script.* R4 MUST be enforced by a check script in CI, because rustfmt's `blank_lines_lower_bound` is not available on the stable toolchain. The script fails a file where the module documentation, the `use` block, a top-level item or a method inside an `impl` follows the previous one with no blank line between them. Attributes and doc comments belong to the item they precede.
- **R13.** *Line width is checked by a script.* R5 MUST be enforced by a check script in CI that fails any line of a `.rs` file longer than `max_width`, apart from a line that is one string literal or URL, because rustfmt's `error_on_line_overflow` is not available on the stable toolchain.
- **R14.** *No one-letter bindings.* R2 MUST be enforced by `clippy::min_ident_chars` at warn, with `allowed-idents-below-min-chars = []` in `clippy.toml` so its default list (`i`, `j`, `x`, `y`, `z`, `w`, `n`) is not exempt. The lint does not flag a generic type parameter.
- **R15.** *No glob imports.* A `use` MUST NOT import with `*`, enforced by `clippy::wildcard_imports` and `clippy::enum_glob_use`. The exceptions are the lint's defaults: `use super::*` in a test module and a `pub use` re-export. A prelude module is not an exception for a module the project defines: the project defines no prelude.
- **R16.** *One module per `use`.* A `use` statement MUST import items from one module: braces may group items of that module (`use foo::bar::{Bar, Baz};`) but hold no path (`use foo::{Foo, bar::Bar};` is two statements). rustfmt's `imports_granularity = "Module"` is not available on the stable toolchain, so a check script in CI fails a brace group that holds `::`.
- **R17.** *Types bare, functions through their module.* A type or trait SHOULD be imported and used by its bare name, and a free function SHOULD be called through its module (`store::open(…)`, not `open(…)` after `use store::open`). A type name that collides or misleads when bare (`io::Error` beside the project's `Error`) is used through its module or renamed on import.
- **R18.** *Public items are documented.* Every public item MUST carry a doc comment, enforced by the `missing_docs` lint at warn. A comment SHOULD say what the code does not already say: the contract, the guarantee, the reason, a panic or an unsafe invariant, not a restatement of the signature.
- **R19.** *`#[rustfmt::skip]` only for alignment or generated code.* `#[rustfmt::skip]` MUST be used only for hand-aligned code (a table, a matrix) or generated code, and MUST carry a comment on the line before it saying which. A check script in CI fails a `rustfmt::skip` without that comment.
- **R20.** *Top-level order in a Rust file.* A Rust file MUST read, in this order: its module documentation; its `mod` items (declarations and inline modules); its `use` items (re-exports included); its `macro_rules!` definitions; its constants and statics; its types, each with its impl blocks; its free functions; and its test module last. Order within a group is not checked; rustfmt sorts the `mod` and `use` items. A `macro_rules!` that a child module uses MAY precede that `mod` line, with `#![expect(clippy::arbitrary_source_item_ordering, reason = "...")]` (STD-02 §R35). This is R6 for Rust, and it is a gate.
- **R21.** *A type and its impls are adjacent.* A type MUST be followed by its inherent impl and then its trait impls, standard-library traits before the project's, and no other item sits between them.
- **R22.** *Order inside an inherent impl.* An inherent impl MUST list, in this order: associated constants and types; associated functions with no receiver, constructors first (a constructor returns `Self`); `with_*` methods, whatever their receiver; methods taking `&self`; methods taking `&mut self`; methods that take `self` by value. Within each of these groups, methods private to the module follow the ones visible outside it.
- **R23.** *Struct literal field order.* A struct literal MUST list its fields in the order the struct declares them, whether a field is written in full or as a shorthand.
- **R26.** *`super` reaches one level.* A path MUST NOT contain `super::super`; an item further away is named from `crate::`.

## Applies to

- `universal`: the rules in the `Core:` clusters, in every constellation code repository, whatever its languages.
- `rust`: the rules in the `Rust profile` cluster, in every repository that contains Rust source, on top of the core.

Rules R4, R5 and the Rust profile's checks apply to source the project writes. Generated code that the project checks in is exempt where its file says it is generated.

**Adopting v1.** A new project meets every MUST from its first commit, with the gates in [`checks.md`](checks.md) in its CI. An existing project adds the gates in the change that adopts this standard and fixes what they find there, or records the gates it has not yet added as a deviation (below) until it does. The SHOULD rules bind new and changed code; an existing file is brought into line when a change touches it for another reason.

Left out on purpose: a cap on function length (dense code passes a length cap more easily than readable code, so a cap rewards the habit this standard stops), and a ban on implementation in a module's `mod.rs` (a driver's `mod.rs` holds its open call by design).

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing `STD-12@<version> §Rn` and the reason. It never edits its vendored copy of this standard.
