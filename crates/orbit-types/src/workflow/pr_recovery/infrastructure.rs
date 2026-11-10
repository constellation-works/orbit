//! Durable admission for evidenced workflow infrastructure recovery.
use crate::task::TaskHistoryEntry;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// History event reserved before the single infrastructure mutation.
pub const PR_INFRASTRUCTURE_RESERVED_EVENT: &str = "pr_infrastructure_reserved";

/// History evidence that a candidate workflow failed before creating jobs.
pub const PR_CANDIDATE_WORKFLOW_FAILED_EVENT: &str = "pr_candidate_workflow_failed";

/// Classification retained in completion errors and final-recovery input.
pub const PR_CANDIDATE_WORKFLOW_FAILURE_MARKER: &str = "candidate_workflow_failure:";

/// An effective required context, including its allowed integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredGate {
    pub context: String,
    /// None is the provider's explicit any-integration policy.
    pub app_id: Option<u64>,
}

/// Verified failure of a workflow before it created any jobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrFailedWorkflowRun {
    /// The candidate diff changes this workflow file.
    pub candidate_owned: bool,
    /// Provider diagnosis, when the provider's run view could be read.
    pub provider_message: Option<String>,
}

/// The source of a missing required context. The prerequisite variant retains
/// the original flattened persisted shape, so existing reservations still read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PrInfrastructureFailure {
    Prerequisite {
        workflow_job: String,
        prerequisite: String,
        prerequisite_job_id: u64,
    },
    WorkflowRun {
        failed_workflow_run: PrFailedWorkflowRun,
    },
}

/// Adapter-verified, source-bound evidence for the missing protected check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrInfrastructureEvidence {
    pub repository: String,
    pub repository_id: u64,
    pub pr_number: String,
    pub head_sha: String,
    pub head_branch: String,
    pub base_branch: String,
    pub base_sha: String,
    pub required: Vec<RequiredGate>,
    pub missing: RequiredGate,
    pub app_id: u64,
    pub workflow_id: u64,
    pub workflow_path: String,
    /// Definition binding the missing context. A zero-job failure may use the
    /// base definition because the candidate's workflow cannot be parsed.
    pub workflow_source: String,
    pub workflow_blob: String,
    #[serde(flatten)]
    pub failure: PrInfrastructureFailure,
    pub run_id: u64,
    pub run_attempt: u32,
    pub event: String,
    /// Complete original provider snapshots, retained with the reservation.
    pub policy: Value,
    pub provider: Value,
    pub run: Value,
    pub jobs: Value,
    pub log: String,
}

impl PrInfrastructureEvidence {
    /// The verified zero-job failure, when no prerequisite job existed.
    pub fn failed_workflow_run(&self) -> Option<&PrFailedWorkflowRun> {
        match &self.failure {
            PrInfrastructureFailure::WorkflowRun {
                failed_workflow_run,
            } => Some(failed_workflow_run),
            PrInfrastructureFailure::Prerequisite { .. } => None,
        }
    }
}

/// A cumulative reservation; execution identity does not reset its budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrInfrastructureReservation {
    pub execution: String,
    pub evidence: PrInfrastructureEvidence,
}

/// Decide under the owner's task lock, then mutate only for Reserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrInfrastructureAdmission {
    Reserved(PrInfrastructureReservation),
    Observe(PrInfrastructureReservation),
    Refused(String),
}

/// Read every reservation without ignoring malformed durable entries.
pub fn pr_infrastructure_reservations(
    history: &[TaskHistoryEntry],
) -> Result<Vec<PrInfrastructureReservation>, String> {
    history
        .iter()
        .filter(|entry| entry.event == PR_INFRASTRUCTURE_RESERVED_EVENT)
        .map(|entry| {
            serde_json::from_str(
                entry
                    .note
                    .as_deref()
                    .ok_or("infrastructure reservation has no evidence")?,
            )
            .map_err(|error| format!("unreadable infrastructure reservation: {error}"))
        })
        .collect()
}

/// One infrastructure rerun per repository/PR across heads and executions.
/// Re-entering the same evidence observes it; it never admits a second write.
pub fn decide_pr_infrastructure(
    history: &[TaskHistoryEntry],
    execution: &str,
    evidence: &PrInfrastructureEvidence,
) -> PrInfrastructureAdmission {
    let reservations = match pr_infrastructure_reservations(history) {
        Ok(reservations) => reservations,
        Err(reason) => return PrInfrastructureAdmission::Refused(reason),
    };
    if let Some(previous) = reservations.into_iter().find(|previous| {
        previous.evidence.repository_id == evidence.repository_id
            && previous.evidence.pr_number == evidence.pr_number
    }) {
        if previous.evidence.head_sha == evidence.head_sha
            && previous.evidence.run_id == evidence.run_id
            && previous.evidence.run_attempt == evidence.run_attempt
        {
            return PrInfrastructureAdmission::Observe(previous);
        }
        return PrInfrastructureAdmission::Refused(
            "the cumulative infrastructure rerun budget is spent".into(),
        );
    }
    PrInfrastructureAdmission::Reserved(PrInfrastructureReservation {
        execution: execution.into(),
        evidence: evidence.clone(),
    })
}
