//! The claimed leaves a pull drain is still carrying. Job's cancel and stop
//! surfaces report them and the distributed settlement reads them from the
//! drain's admissions, so neither owns the shape [ORB-17937].

use serde::Serialize;

/// A claimed leaf a pull drain is still carrying: launched, not yet
/// terminal. `orbit run cancel` (graceful) waits for these, `--force` stops
/// them, and `orbit run auto --stop` and `orbit run show` list them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DrainClaimedLeaf {
    pub leaf_run_id: String,
    /// The leaf's job: one of the claimed leaf definitions.
    pub job_id: String,
    /// The owner's task the leaf executes.
    pub task_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim; an earlier drain's admission
    /// is carried by the live drain for the same owner.
    pub admitted_by: String,
    /// The leaf run's state (`running`, `pending`, ...).
    pub leaf_state: String,
    /// Where the follower's record of this admission stands.
    pub settlement_phase: String,
    /// Whether the leaf is still implementing or has reached its
    /// before-landing review [ORB-15194].
    pub stage: ClaimedLeafStage,
}

/// What a live claimed leaf is doing with its drain slot [ORB-15194].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimedLeafStage {
    /// Implementing, validating or publishing: the work the slot exists for.
    Implementing,
    /// Past `pr_open`, in the before-landing review of its open pull request
    /// and what follows it. The drain admits a replacement beside it, up to
    /// its concurrency.
    Reviewing,
}

impl ClaimedLeafStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementing => "implementing",
            Self::Reviewing => "reviewing",
        }
    }

    /// The text a stage indicator shows for this stage.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::Implementing => "Implementing",
            Self::Reviewing => "Reviewing",
        }
    }
}

impl DrainClaimedLeaf {
    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} (leaf {}) {} {} settlement={}",
            self.task_id.as_deref().unwrap_or("-"),
            self.leaf_run_id,
            self.leaf_state,
            self.stage.as_str(),
            self.settlement_phase
        )
    }
}
