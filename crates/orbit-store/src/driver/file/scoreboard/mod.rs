//! File-backed scoreboard implementations.
//!
//! This directory module keeps the scoreboard implementation split into
//! focused files while preserving the existing `file::scoreboard::*` paths.

pub(crate) mod common;
pub(crate) mod pr_scoreboard;
pub(crate) mod scoreboard_summary;

#[cfg(test)]
mod tests;
