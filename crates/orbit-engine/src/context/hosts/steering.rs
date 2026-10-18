//! Mid-run steering's promotion gate, as the host reads it
//! (`docs/design/mid-run-steering/2_design.md` §3).

use orbit_types::task::TaskStatus;
use orbit_types::workflow::CommentLogMark;

/// What the promotion gate read for one task of a run, from the durable task
/// and run records alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteeringGateReading {
    /// Steering is off for this run. The host read nothing, and the gate
    /// leaves the pipeline exactly as it runs without the feature.
    Disabled,
    /// Every operator comment past the run's watermark reached one of its
    /// prompts in full.
    Clear,
    /// These operator comments, oldest first, reached none of the run's
    /// prompts in full.
    Outstanding(Vec<CommentLogMark>),
    /// The task refuses implementer writes. Terminal handling wins over the
    /// steering `outstanding` names, which may be empty.
    Terminal {
        status: TaskStatus,
        outstanding: Vec<CommentLogMark>,
    },
}

/// The host's answer to a request for one more steering round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteeringRoundAdmission {
    /// Recorded durably on the run before answering: this is round `round`
    /// of `max_rounds`, spent whatever its dispatch does.
    Admitted { round: u8, max_rounds: u8 },
    /// No round runs: the run already spent all `max_rounds`.
    Exhausted { rounds: u8, max_rounds: u8 },
    /// No round runs: the run's last round was admitted for exactly these
    /// comments and none of them reached a prompt, so another would observe
    /// nothing new.
    Unobservable { rounds: u8, max_rounds: u8 },
}
