# orbit-cli

Clap entry point. Subcommand files hold only `Args`/`Subcommand` definitions, one `impl Execute` calling the owning domain crate, and output projection. Registry lookups, file I/O, audit decisions, and state mutation belong in the domain crate.

- **Directory ⟺ one command.** A subdirectory under `command/` is `orbit <name>`, never a grouping folder; the source tree stays flat regardless of `--help` sections. Single `.rs` is fine for any command that fits in one file.
- In a command directory: `command.rs` holds the `XxxCommand` struct, `XxxSubcommand` enum, and their `Execute` impls; each subcommand body is its own `<subcommand>.rs`; shared helpers go in `support.rs`; `mod.rs` is declarations and re-exports only — no clap derives.
- `--help` grouping comes from a hand-rolled `help_template` in `command/mod.rs`. Adding a top-level command: add the `Commands` variant in template order, add the row to the template section, and add exactly one exhaustive arm in `command/operation.rs` (dispatch, runtime need, audit metadata, error policy together — no default arm, no duplicate policy matches elsewhere).
- Registry-derived commands (`friction`): `command/operation_args.rs` builds the tree from `orbit-common`'s operation registry. Adding a verb is a registry entry plus a Core handler — nothing under `command/` changes. Before migrating another noun, freeze its `--help` fixtures per [`operations-as-data/references/cookbook.md`](../../docs/design/operations-as-data/references/cookbook.md) Step 0. Don't half-migrate a command.
