# Changelog — STD-12 Source style

Version history of [STD-12](STD-12.md). Adopting repos pin the `version:` in its frontmatter as `STD-12@<version>`; a version changes only when a rule is added, removed or changed in meaning.

- **v1 (2026-11-25):** initial publication, R1–R19: the core (R1–R10) and the Rust profile (R11–R19). From scribe standard-18 (singular names) and standard-20 (Rust source readability), both Daniel's, 2026-11-25.
- **v2 (2026-11-26):** R6 names the declarations of a source file's child modules, placed after its header documentation and before its imports. From scribe standard-25 (Daniel: "let's create a standard on declaration order"; the principle was approved in chat, decision-55) (ORB-28385).
  - R20 (new, Rust profile, MUST): a Rust file's `mod` items, then its `use` items, come before its other items; gate `clippy::arbitrary_source_item_ordering`. Adopting v2: the gate and its fixes land in the re-pin change, or the missing gate is a recorded deviation (scribe standard-25, ORB-28385).
- **v3 (2026-11-26):** - R20 (Rust profile) now orders a file's macros, constants, types and functions by the gate's groups, with its test module last, in place of one everything-else group. Adopting v3: the gate and its fixes land in the re-pin change, or the missing gate is a recorded deviation. From scribe standard-26.
  - - Rust profile: a type's impls follow it directly, inherent first, then standard-library trait impls, then the project's (MUST; its gate is built in rust-cli-template). From scribe standard-26.
  - - Rust profile: an inherent impl lists constants and types, then constructors, then with_* methods, then &self, &mut self and self-consuming methods (MUST; its gate is built in rust-cli-template). From scribe standard-26.
  - - Rust profile: a struct literal lists its fields in declaration order, named or shorthand (MUST; gate clippy::inconsistent_struct_constructor). From scribe standard-26.
  - - One subject per file in Core: layout (SHOULD, review-only): a divider comment between sections is the sign that a file should split. From scribe standard-26.
  - - Callers before callees among free functions and private methods (SHOULD, review-only) in Core: layout. From scribe standard-26.
- **v4 (2026-11-26):** New rule, a MUST: `super` reaches one level, `super::super` is not used; text-match gate to be built in the Rust CLI template (scribe standard-27).
