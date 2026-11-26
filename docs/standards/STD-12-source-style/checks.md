# Machine checks — STD-12 Source style

The gate that enforces each rule in [STD-12](STD-12.md), or `review-only`. Ship a change with the gate its rule names, or say it is review-only.

The check scripts are built in `rust-cli-template` first, as `scripts/check-source-style.sh` run by its `make ci`; a project made from the template inherits them. Until a script exists, its rule is a deviation the adopting repo records. No constellation repository carries these scripts at v1.

The Rust lints go in the workspace lint table beside STD-02 §R22's baseline, so `cargo clippy --all-targets -- -D warnings` fails on them:

```toml
[workspace.lints.rust]
missing_docs = "warn"

[workspace.lints.clippy]
min_ident_chars = "warn"
wildcard_imports = "warn"
enum_glob_use = "warn"
```

and in `clippy.toml`:

```toml
allowed-idents-below-min-chars = []
```

| Rule | Gate |
|---|---|
| R1 | A CI step that fails on a plural module, source directory or source file name outside an allow-list of toolchain-fixed names (`tests`, `examples`, `benches`). It reads names, so it cannot tell a plural from a singular ending in `s` (`status`, `address`): the repository keeps those in the same allow-list. |
| R2 | Each profile's lint; Rust: R14. |
| R3 | The profile's formatter in check mode in CI; Rust: R11. |
| R4 | The profile's check script; Rust: R12. |
| R5 | The profile's check script; Rust: R13. |
| R6 | review-only |
| R7 | review-only |
| R8 | review-only |
| R9 | review-only |
| R10 | review-only |
| R11 | `cargo fmt --all --check` in CI on the stable toolchain; `rustfmt.toml` holds no option that stable rustfmt warns is unstable. |
| R12 | `scripts/check-source-style.sh`, blank-line check. To be built in the template. |
| R13 | `scripts/check-source-style.sh`, line-width check against `max_width`. To be built in the template. |
| R14 | `clippy::min_ident_chars` with `allowed-idents-below-min-chars = []`. |
| R15 | `clippy::wildcard_imports` and `clippy::enum_glob_use`, with the lint's default exceptions. |
| R16 | `scripts/check-source-style.sh`, a `use` whose brace group holds `::`. To be built in the template. |
| R17 | review-only |
| R18 | The `missing_docs` lint for presence; the content clause is review-only. |
| R19 | `scripts/check-source-style.sh`, a `rustfmt::skip` without the comment on the line before it. To be built in the template. |
| R20 | `clippy::arbitrary_source_item_ordering` at warn, with `source-item-ordering = ["module"]`, `module-item-order-groupings` (modules: extern_crate, mod, foreign_mod; use: use; macros: macro; constants: static, const; types: ty_alias, enum, struct, union, trait, trait_alias, impl; functions: fn, global_asm) and `module-items-ordered-within-groupings = "none"` in `clippy.toml`, run by `cargo clippy --all-targets -- -D warnings` (STD-02 §R22). |
| R21 | scripts/check-source-style.sh, to be built in the template. |
| R22 | scripts/check-source-style.sh, to be built in the template. |
| R23 | clippy::inconsistent_struct_constructor at warn, with check-inconsistent-struct-field-initializers = true in clippy.toml. |
| R24 | review-only |
| R25 | review-only |
| R26 | `scripts/check-source-style.sh`: a text match for `super::super` in `.rs` files (the script is to be built in the Rust CLI template) |
