//! Activity dispatch and the engine's deterministic actions: v2 agent and
//! local-shell dispatch, provider capacity, limits and usage windows, name
//! resolution, worktree lifecycle, PR and handoff landing, the shipped PR
//! pipeline's before-PR review fixes, its before-landing review, completion
//! re-review rounds and candidate resume, a re-claim's reuse of its earlier
//! claim's pull request, and history notes.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod before_landing_review;
mod candidate_resume;
mod claimed_candidate_paths;
#[cfg(unix)]
mod claimed_pr_reuse;
mod commit_verifier;
mod completion_review;
#[cfg(unix)]
mod dependabot_collect;
mod final_recovery;
#[cfg(unix)]
mod forge_hold;
mod git_fixture;
mod handoff_landing;
mod history_note;
mod pr_landing;
#[cfg(unix)]
mod provider_capacity;
#[cfg(unix)]
mod provider_limit;
#[cfg(unix)]
mod provider_usage_window;
mod recovery_evidence;
mod review_fixes;
#[cfg(unix)]
mod reviewer_wall_clock;
#[cfg(unix)]
mod source_inspection;
mod v2_cli_agent;
#[cfg(unix)]
mod v2_cli_sandbox;
mod v2_local_shell;
mod v2_name_resolution;
mod v2_runtime;
mod v2_worktree_lifecycle;
