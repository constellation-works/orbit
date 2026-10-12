//! Typed review-gate refusals the failure handoff decides on [ORB-15714].

/// Why the review gate stopped delivery, for the refusals the failure handoff
/// treats differently from a generic step failure.
///
/// The refusing step carries it beside its message, and the terminal failure
/// handoff receives it as the step's `error_code`, so the handoff decides on
/// the code, never on the message's wording. Each message still leads with
/// the same `code:` for human readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewGateRefusal {
    /// Admission found the held candidate still waiting for named external
    /// evidence; the task stays held, not blocked.
    AwaitingEvidence,
    /// The reviewer exceeded its wall clock; the partial report is retained
    /// and delivery is requeued within a bounded allowance.
    TimeoutIncomplete,
    /// The reviewer exited with only its initial placeholder report; the
    /// attempt was released without a verdict, so the review is unspent.
    Abandoned,
    /// Settlement recorded a verdict that does not approve the candidate.
    Blocked,
}

impl ReviewGateRefusal {
    const ALL: [Self; 4] = [
        Self::AwaitingEvidence,
        Self::TimeoutIncomplete,
        Self::Abandoned,
        Self::Blocked,
    ];

    /// The step `error_code` this refusal is recorded under.
    #[must_use]
    pub const fn error_code(self) -> &'static str {
        match self {
            Self::AwaitingEvidence => "review_awaiting_evidence",
            Self::TimeoutIncomplete => "review_timeout_incomplete",
            Self::Abandoned => "review_abandoned",
            Self::Blocked => "review_gate_blocked",
        }
    }

    /// The refusal a step `error_code` names, if it names one.
    #[must_use]
    pub fn from_error_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.error_code() == code)
    }
}
