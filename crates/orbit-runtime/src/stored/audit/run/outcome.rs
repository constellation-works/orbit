//! Recorded ending of one activity step, parsed once from the audit trail.
//!
//! The spelling stays the recorded text (`as_str`). `state` on a step is the
//! display form: a denial is shown as failed, and every other outcome uses
//! its own spelling.

use orbit_types::workflow::JobRunState;

/// How an activity step ended in the v2 audit trail.
///
/// Known spellings are the ones readers branch on. Anything else is kept
/// verbatim in [`AuditStepOutcome::Other`] so a later reader still sees the
/// recorded text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditStepOutcome {
    /// Recorded `success`.
    Success,
    /// Recorded `succeeded`.
    Succeeded,
    /// Recorded `finished`, also the default when a finished step omits the field.
    Finished,
    /// Recorded `failed`.
    Failed,
    /// Recorded `error`.
    Error,
    /// Recorded `denied`. Display state is `failed`.
    Denied,
    /// Recorded `timeout`.
    Timeout,
    /// Recorded `skipped`.
    Skipped,
    /// Recorded `cancelled`.
    Cancelled,
    /// Recorded `interrupted`.
    Interrupted,
    /// Recorded `held`.
    Held,
    /// Recorded `retrying`.
    Retrying,
    /// Recorded `pending`.
    Pending,
    /// Recorded `running`.
    Running,
    /// A spelling this projection does not name, kept as recorded.
    Other(String),
}

impl AuditStepOutcome {
    /// Parse one recorded outcome spelling. Unrecognised text is [`Self::Other`].
    pub(crate) fn from_recorded(spelling: &str) -> Self {
        match spelling {
            "success" => Self::Success,
            "succeeded" => Self::Succeeded,
            "finished" => Self::Finished,
            "failed" => Self::Failed,
            "error" => Self::Error,
            "denied" => Self::Denied,
            "timeout" => Self::Timeout,
            "skipped" => Self::Skipped,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            "held" => Self::Held,
            "retrying" => Self::Retrying,
            "pending" => Self::Pending,
            "running" => Self::Running,
            other => Self::Other(other.to_string()),
        }
    }

    /// The exact spelling recorded on the audit row.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Success => "success",
            Self::Succeeded => "succeeded",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Error => "error",
            Self::Denied => "denied",
            Self::Timeout => "timeout",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Held => "held",
            Self::Retrying => "retrying",
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Other(spelling) => spelling,
        }
    }

    /// The state text stored beside this outcome.
    ///
    /// A denial is shown as `failed`. Every other outcome uses its own spelling.
    pub fn display_state(&self) -> &str {
        match self {
            Self::Denied => "failed",
            other => other.as_str(),
        }
    }

    /// Whether the display state is `error`, `failed`, `timeout` or
    /// `interrupted`. `cancelled` counts only when the caller includes it.
    ///
    /// [`Self::Denied`] is included because its display state is `failed`,
    /// which is the text the state-string readers matched.
    pub fn is_failure_like(&self, include_cancelled: bool) -> bool {
        matches!(
            self,
            Self::Error | Self::Failed | Self::Denied | Self::Timeout | Self::Interrupted
        ) || (include_cancelled && matches!(self, Self::Cancelled))
    }

    /// The persisted job-run step state for a recorded outcome.
    ///
    /// A missing outcome and an unrecognised spelling both warn and persist
    /// as failed, matching the previous string mapping.
    pub fn job_run_state(outcome: Option<&Self>) -> JobRunState {
        match outcome {
            Some(Self::Success | Self::Succeeded | Self::Finished) => JobRunState::Success,
            Some(Self::Failed | Self::Error | Self::Denied) => JobRunState::Failed,
            Some(Self::Timeout) => JobRunState::Timeout,
            Some(Self::Skipped) => JobRunState::Skipped,
            Some(Self::Cancelled) => JobRunState::Cancelled,
            Some(Self::Interrupted) => JobRunState::Interrupted,
            Some(Self::Held) => JobRunState::Held,
            Some(Self::Retrying) => JobRunState::Retrying,
            Some(Self::Pending) => JobRunState::Pending,
            Some(Self::Running) => JobRunState::Running,
            Some(Self::Other(spelling)) => unrecognized_audit_outcome(spelling),
            None => unrecognized_audit_outcome("<missing>"),
        }
    }
}

fn unrecognized_audit_outcome(spelling: &str) -> JobRunState {
    tracing::warn!(
        outcome = spelling,
        "unrecognized audit step outcome persisted as failed",
    );
    JobRunState::Failed
}
