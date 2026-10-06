//! Trusted claim execution and owner-side handoff landing types.

/// Trusted execution facts for one claimed distributed leaf [ORB-12616].
///
/// The runtime resolves every field from its own process worker binding and
/// the durable pull admission that created this run. Nothing here may come
/// from job input, activity payload or environment: an activity compares a
/// payload against this context, it never adopts one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimExecutionContext {
    /// Original owner-admitted selectors from the local durable receipt.
    pub footprint: Vec<String>,
    pub workspace_id: String,
    pub task_id: String,
    pub claim_id: String,
    /// Trusted execution machine, as the owner recorded it on the claim.
    pub machine_id: String,
    /// The one leaf run bound to this claim.
    pub run_id: String,
    /// Owner-resolved ship mode: `local` or `pr`.
    pub ship_mode: String,
    pub base_branch: String,
    pub landing_branch: String,
    /// Commands the owner requires this candidate to pass. Empty means no
    /// required check: the activity runs nothing and records that it did not.
    pub required_commands: Vec<String>,
}

/// What the owner store knows about one authorized handoff, handed to the
/// landing activity so it can observe the real world and decide [ORB-12499].
///
/// Everything here is owner-held state. The activity may not treat any of it as
/// proof that a merge happened; it exists so the activity knows exactly which
/// candidate to look for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffLandingContext {
    pub handoff_id: String,
    pub task_id: String,
    pub claim_id: String,
    /// The accepted candidate: repository, branches, candidate/base revisions
    /// and delivery variant.
    pub candidate: orbit_types::workflow::handoff::HandoffCandidate,
    /// An external merge request this owner sent but never confirmed. It must
    /// be reconciled against real state before anything else happens.
    pub unresolved_merge_intent: Option<String>,
    /// The owner checkout the landing runs against. Never a follower path.
    pub workspace_path: std::path::PathBuf,
}

/// One durable step of a landing attempt, recorded by the owner store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffLandingStep {
    /// Persisted before the external merge call, so a lost reply is uncertainty
    /// the next attempt must reconcile rather than silently retry.
    PublishIntent { intent_id: String },
    /// The external state was read back: `merged` says what it actually shows.
    ResolveIntent { intent_id: String, merged: bool },
    /// Verified merge evidence permits the guarded `review -> done` transition.
    Complete,
    /// Durable evidence for a landing that must not proceed.
    Stop,
}

/// A landing transition with the owner observation that justifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffLandingUpdate {
    pub handoff_id: String,
    pub step: HandoffLandingStep,
    /// Read from the provider and the owner checkout by this activity, never
    /// copied from the worker's handoff payload. `None` where no candidate
    /// observation was possible, which the host refuses for any step that
    /// records a landing authority decision.
    pub observed: Option<orbit_types::workflow::handoff::HandoffCandidate>,
    /// What was observed, in the operator's words, recorded durably.
    pub evidence: String,
}
