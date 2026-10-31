use orbit_types::workflow::ReviewCertificate;
use serde_json::Value;

pub(super) enum Settled {
    Passed(Value),
    AwaitingEvidence(Box<orbit_types::workflow::ReviewEvidenceHold>),
    Blocked(Box<ReviewCertificate>),
    /// Every failed required check fails the same way on the pinned base,
    /// and nothing else keeps the review from passing.
    BaselineRed(Box<ReviewCertificate>),
    /// The reviewer exited cleanly leaving only its initial provisional
    /// report [ORB-15130]. The attempt was released, so it is not a verdict
    /// and the candidate's review stays unspent.
    Abandoned,
}
