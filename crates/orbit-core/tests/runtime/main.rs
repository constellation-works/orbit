//! The composed runtime through its public surface: dispatch admission, the
//! distributed drain, `os:` and `ui` tag routing, retired deterministic stubs, final
//! recovery, repeated rebase recovery, relation auto-close, PR closure on
//! terminal decisions and requeues, settlement from a delivery PR merged
//! outside Orbit, orphaned delivery adoption, plugin inspection, the sandbox
//! opt-out, the agent sandbox mask and cold lexical search hydration.
//!
//! One integration-test binary per area keeps link cost down. Every top-level
//! file here (`<name>.rs`, or a `<name>/mod.rs` directory) is registered by
//! `crates/orbit-core/build.rs`, so a new module needs no edit to this file. A
//! new area still joins this binary rather than a new top-level `tests/*.rs`
//! file (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/admission.rs"]
mod admission_test_support;
#[path = "../support/external_merge_fixture.rs"]
mod external_merge_fixture;
#[path = "../support/git.rs"]
mod git_test_support;
#[path = "../support/task_params.rs"]
mod task_params_test_support;

orbit_common::isolate_test_process!();

/// Keep related permutations in one table with isolated fixture state.
macro_rules! scenario_table {
    ($table:ident, $($row:ident),+ $(,)?) => {
        #[test]
        fn $table() {
            crate::scenarios::run(module_path!(), stringify!($table), &[
                $((stringify!($row), $row as fn() -> _)),+
            ]);
        }
    };
}
pub(crate) use scenario_table;

// Every top-level module in this directory, as written by crates/orbit-core/build.rs.
// It follows `scenario_table`, so each module can use the macro. The file must sit
// beside this one: rustc resolves its `mod` lines from its own directory.
include!(".generated_modules.rs");

// `readiness/` has no `mod.rs`; its one file is mounted here by path.
#[path = "readiness/population.rs"]
mod readiness_population;
