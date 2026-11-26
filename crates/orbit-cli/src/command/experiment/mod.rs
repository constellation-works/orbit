//! The experiment CLI, derived from the experiment operation registry.
//!
//! Every verb calls its `orbit.experiment.*` tool, so `--json` is the tool's
//! payload. What is left here is the text rendering: a table for `list`, the
//! verdict, health checks and per-arm metric tables for `readout`, and for
//! every other verb the experiment's definition, arms and, for `show`, its
//! event log in order.

mod abandon;
mod assign;
mod clone;
mod command;
mod conclude;
mod create;
mod delete;
mod list;
mod pause;
mod readout;
mod resume;
mod reweight;
mod show;
mod start;
mod support;
mod update;

pub use command::ExperimentCommand;
