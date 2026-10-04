//! Activity dispatch and the engine's deterministic actions: v2 agent and
//! local-shell dispatch, name resolution, worktree lifecycle, PR landing, the
//! shipped PR pipeline's review rework loop and history notes.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod commit_verifier;
mod history_note;
mod pr_landing;
mod review_rework;
mod v2_cli_agent;
mod v2_local_shell;
mod v2_name_resolution;
mod v2_runtime;
mod v2_worktree_lifecycle;
