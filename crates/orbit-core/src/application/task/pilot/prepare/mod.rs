//! Task-pilot preparation: discover and partition the tasks a run assesses.

mod exclusion;
mod repilot;
mod run;

#[cfg(test)]
pub(super) use run::hydration_test_hook;
pub(crate) use run::prepare;
