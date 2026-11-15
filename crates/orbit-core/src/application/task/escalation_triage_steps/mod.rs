//! The escalation triage lane: the pilot-warning holds it reads, the one
//! decision it records on each, and the deterministic support action of
//! `escalation_triage_pipeline`.
//!
//! A `proposed` task whose latest applied task-pilot assessment holds
//! promotion on warning findings ([`super::pilot::held_pilot_warnings`], the predicate
//! every promotion authority shares) waits for an operator to submit triage.
//! The dashboard's Triage action submits this lane on demand; the same
//! submission is available as `orbit job run escalation_triage_pipeline`.
//! `prepare_escalation_triage` draws a crew for it from
//! `workflow.purpose_crews.pilot_warning_triage` (else the hard tier), and
//! the `triage_escalation` agent records one
//! disposition through [`OrbitRuntime::record_pilot_warning_triage`]:
//!
//! - **propose** (decision, the default whenever the operator resolves the
//!   hold by choosing): one comment quoting the holding findings and a
//!   decision request with a short summary, two or three options (each with
//!   a consequence, an optional field rewrite and an optional action) and a
//!   recommendation. One click answers it.
//! - **report** (human-only, limited to actions outside Orbit): one comment
//!   naming the human action, and a request with the short summary, that
//!   action and one "Done" option.
//!
//! Each appends one [`PILOT_WARNING_TRIAGED_EVENT`] marker naming the
//! assessment's operation id, which excludes that assessment from the lane
//! and is what member settlement reads. The lane never approves, promotes,
//! clears a hold or edits any other task field itself: an operator's answer
//! does, through the decision request. A material edit re-pilots the task; a
//! new assessment that still holds is a new member.
//!
//! Blocked tasks are no longer read here: final recovery is their one
//! decider, and the retired `block` subject is refused with a migration
//! message.

mod prepare;
mod proof;

pub(crate) use prepare::prepare;
pub(crate) use proof::TriageInspectionProof;

use orbit_types::task::DecisionOptionInput;

/// History event that marks one pilot assessment's warning hold triaged.
pub const PILOT_WARNING_TRIAGED_EVENT: &str = "pilot_warning_triaged";

/// What the lane asks to record on one held assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PilotWarningDecision {
    /// Decision: the operator chooses between options.
    Propose {
        /// One or two plain sentences naming the decision needed.
        summary: String,
        /// The options, two or three.
        options: Vec<DecisionOptionInput>,
        /// 1-based index of the recommended option.
        recommended: usize,
    },
    /// Human-only: name the action only a human outside Orbit can take.
    Report {
        /// The specific human action.
        human_action: String,
        /// One or two plain sentences for the card; the action stands in
        /// when absent.
        summary: Option<String>,
    },
}

/// A triage decision for one task's held pilot assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotWarningTriageRequest {
    /// The held task.
    pub task_id: String,
    /// The decision.
    pub decision: PilotWarningDecision,
    /// Who asked, named in the comment.
    pub requested_by: String,
}

mod candidacy;
mod recording;
pub(crate) use candidacy::{PilotWarningCandidacy, pilot_warning_candidacy, triaged_at};

use super::pilot::HeldPilotWarnings;
