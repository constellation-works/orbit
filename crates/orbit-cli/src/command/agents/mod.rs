//! The agent scorecard CLI, derived from the agents operation registry.
//!
//! The verbs call the read-only `orbit.agents.*` tools, so `--json` is the
//! payload the dashboard's Agents view reads. What is left here is the text
//! rendering: one row per crew and model for `scorecard`, one row's terms
//! with the tasks behind every count for `show`, and one row per recorded
//! required-check failure for `ci-failures`.
//!
//! `vision-probe` is hand-wired: it launches one canary run per crew and
//! prints whether each model returned the code. `rank` is hand-wired too:
//! the model ranking is a CLI and dashboard read, and `rank calibrate`
//! writes the workspace's noise-scale seasons.

mod ci_failures;
mod command;
mod rank;
mod scorecard;
mod show;
mod support;
mod vision_probe;

pub use command::{AgentsCommand, AgentsSubcommand};
