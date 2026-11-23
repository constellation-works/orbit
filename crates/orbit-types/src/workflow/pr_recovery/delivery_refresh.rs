//! [ORB-26860] Refreshing a delivery pull request's head with its base branch:
//! the forge outcome shared by the resident `refresh` right and the delivery
//! pipeline's base-inherited refresh, and the pipeline's request.

/// The `evidence.cause` of the [`super::PrBaseUpdateReservation`] a delivery
/// records before it refreshes its own pull request to the base tip because
/// its failure is base-inherited.
pub const PR_BASE_INHERITED_REFRESH_CAUSE: &str = "base_inherited_refresh";

/// How a request to refresh a pull request's head with its base branch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestRefreshOutcome {
    /// The forge merged the base branch into the pull request head as one
    /// conflict-free merge commit, starting required checks on the new head.
    Updated {
        /// The new pull request head commit after the merge.
        head_sha: String,
    },
    /// The pull request head already contains the base branch tip; nothing
    /// changed.
    AlreadyUpToDate {
        /// The unchanged pull request head commit.
        head_sha: String,
    },
    /// Merging the base branch into the pull request head would conflict on
    /// these paths; nothing changed.
    Conflicting {
        /// Conflicting file paths, sorted and deduplicated.
        conflicting_paths: Vec<String>,
        /// Diagnostic detail.
        reason: String,
    },
    /// The forge or policy refused the refresh (moved head, unrelated history,
    /// branch protection forbidding merge commits, or caller not permitted);
    /// nothing changed.
    Refused {
        /// The refusal reason.
        reason: String,
    },
}

/// A live delivery run's request to refresh its own open pull request to the
/// tip of its base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRefreshRequest {
    /// The run asking; it must own the tasks.
    pub run_id: String,
    /// The tasks the run delivers through this pull request.
    pub task_ids: Vec<String>,
    /// The pull request number.
    pub pr_number: u64,
    /// The pull request's head branch, as the run pinned it.
    pub head_branch: String,
    /// The pull request's base branch, as the run pinned it.
    pub base_branch: String,
    /// The exact head the run holds; a moved head is refused.
    pub expected_head_sha: String,
}
