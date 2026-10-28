//! Settle a task to `done` when its delivery PR merged outside Orbit.
//!
//! An operator who merges a task's delivery PR on the forge lands the task's
//! work, but no Orbit run observes that merge, so the task stayed where it
//! was (usually `blocked`). Walking it through the lifecycle by hand meant a
//! `blocked → in-progress` write that the footprint guard on starting work
//! refuses while another task's claim overlaps it, although nothing will
//! start. This is the reverse of [`pr_closure`](super::pr_closure): there a
//! terminal task closes its PRs, here a task follows its merged PR.
//!
//! A task settles when all of these hold:
//! - it is `blocked`, `in-progress` or `review`;
//! - no run or unsettled claim of its own is live, and no live `run auto`
//!   leaf carries it: that run lands or fails the task itself;
//! - one of its delivery PRs is merged into the task's target branch;
//! - it owes no acceptance criterion after deploy that an operator has not
//!   recorded met.
//!
//! One unsettled claim does not stand in the way: a `handed_off` claim whose
//! completion authority an operator revoked. Nothing will land that handoff,
//! and the revocation cannot be undone, so its task would otherwise stay in
//! `review` for good. It settles when no landing attempt is dispatched for
//! the handoff, no merge intent is unresolved, and the merged PR is exactly
//! the handoff's candidate PR, merged into its landing branch. Every other
//! claim still refuses: a `claimed`, `running` or `repair_pending` one, and a
//! handoff that awaits approval or is authorized, which approval or the
//! owner's landing job completes.
//!
//! A delivery PR is matched as the closure matches it: its head branch is
//! `orbit/<TASK-ID>-…`, its body names the task, and it was authored by a
//! delivery identity (`pr.delivery_authors`, else the login this machine's
//! forge credentials act as), or it is the delivery PR of a follower handoff
//! this owner accepted, or a PR preserved with a released or failed claim's
//! candidate. A claim PR targets its accepted handoff's landing branch, or
//! the landing branch in its original admission receipt. Other PRs target
//! the base branch the task's delivery run was submitted against, else the
//! workspace base branch.
//!
//! The write is Orbit's own: `done` with a landing note naming the PR and its
//! merge commit, recorded in history as [`super::lifecycle::EXTERNAL_MERGE_SETTLED_EVENT`]. It
//! starts no execution, so no footprint guard applies to it. A task with an
//! execution claim of its own is still refused by the store, except through
//! the claim journal: a revoked handoff's settlement is one owner store write
//! that ends the claim `revoked` and moves the task `review → done` together,
//! so a crash cannot leave one without the other. It grants no completion
//! authorization, records no merge intent or landing attempt, and leaves the
//! revoked landing request revoked.
//!
//! An operator-recovered claim has already ended as `revoked`. When its
//! refused handoff recorded no PR, the explicit rescued-delivery route accepts
//! the claim, PR number and operator-reported original head. It verifies the
//! original admission and the merged PR's task identity, author and landing
//! branch, then settles the task under its write lock, retaining the ended
//! claim and recording both heads. See [`crate::OrbitRuntime::settle_recovered_claim_merge`].
//!
//! Entry points run it. Each `orbit run auto` admission pass settles from
//! one listing of the most recently merged PRs, and reads the forge only when
//! a candidate task has a PR to match. Each scheduler clock tick
//! (`orbit clock tick` / `orbit sweep`) runs the same owner pass for every
//! registered, active owner workspace, so a merged task settles with no drain
//! running. `orbit task settle-merged <id>` reads the task's own PRs one by
//! one, so a merge older than that listing settles too; the dashboard's
//! batch settlement runs that per-task read over every candidate and
//! reports each skip instead of refusing the batch.

mod candidate;
mod entry;
mod handoff;
mod model;
mod persistence;
mod refusal;
mod rescued;

use crate::runtime::task_pr_forge::MergedPullRequest;
use candidate::{RevokedHandoff, names_delivery};
pub use model::ExternalMergeBatchSettlement;
pub use model::ExternalMergeSettlement;
pub use model::ExternalMergeSkippedTask;
use model::SETTLEABLE_STATUSES;
pub(crate) use persistence::settled_merge_commit;
use refusal::refused;
