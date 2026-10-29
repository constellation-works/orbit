//! One unchanged-head CI retry, reserved before external mutation.
use crate::task::TaskHistoryEntry;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable reservation, including unknown provider outcomes.
pub const CI_RERUN_RESERVED_EVENT: &str = "ci_check_rerun_reserved";

/// The independently classified root of a workflow-wide failed-jobs retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiRerunRoot {
    pub job_id: u64,
    pub check: String,
    pub classification: String,
    pub signature: String,
}

/// Complete, provider-authenticated action identity and original observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiRerunPlan {
    pub schema_version: u32,
    pub repository: String,
    pub repository_id: u64,
    pub pr_number: String,
    pub head_sha: String,
    pub head_branch: String,
    pub base_branch: String,
    pub accepted_base: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub workflow_id: u64,
    pub roots: Vec<CiRerunRoot>,
    /// Exact failed jobs, including authenticated dependency-only waiters.
    pub failed_jobs: Vec<Value>,
    /// Source blobs, policy, checks, run and complete listing used for admission.
    pub authentication: Value,
}

/// Admission is atomic under the owning task/claim boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiRerunAdmission {
    Reserved,
    Observe(Box<CiRerunPlan>),
    Refused(String),
}

/// Never ignore malformed reservations or reuse a head's shared retry budget.
pub fn decide_ci_rerun(history: &[TaskHistoryEntry], plan: &CiRerunPlan) -> CiRerunAdmission {
    if plan.schema_version != 1 || plan.roots.is_empty() || plan.failed_jobs.is_empty() {
        return CiRerunAdmission::Refused("incomplete CI retry authorization".into());
    }
    let mut reserved: Option<CiRerunPlan> = None;
    let mut legacy_spent = false;
    for entry in history {
        if entry.event == CI_RERUN_RESERVED_EVENT {
            let prior = entry
                .note
                .as_deref()
                .and_then(|note| serde_json::from_str::<CiRerunPlan>(note).ok());
            let Some(prior) = prior else {
                return CiRerunAdmission::Refused("unreadable CI retry reservation".into());
            };
            if prior.schema_version != 1 {
                return CiRerunAdmission::Refused(
                    "unsupported CI retry reservation version".into(),
                );
            }
            if prior.repository_id == plan.repository_id
                && prior.pr_number == plan.pr_number
                && prior.head_sha == plan.head_sha
            {
                if reserved.as_ref().is_some_and(|existing| existing != &prior) {
                    return CiRerunAdmission::Refused(
                        "conflicting CI retry reservations for this head".into(),
                    );
                }
                reserved = Some(prior);
            }
        } else if entry.event == "ci_check_rerun" {
            let prior = entry
                .note
                .as_deref()
                .and_then(|note| serde_json::from_str::<Value>(note).ok());
            let Some(prior) = prior else {
                return CiRerunAdmission::Refused("unreadable legacy CI retry history".into());
            };
            if !prior["head"].as_str().is_some_and(|head| {
                head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) || prior["run_id"].as_u64().is_none()
            {
                return CiRerunAdmission::Refused(
                    "legacy CI retry history omitted its action identity".into(),
                );
            }
            if prior["head"] == plan.head_sha {
                legacy_spent = true;
            }
        }
    }
    if let Some(prior) = reserved {
        return CiRerunAdmission::Observe(Box::new(prior));
    }
    if legacy_spent {
        return CiRerunAdmission::Refused(
            "the shared unchanged-head CI retry budget is spent".into(),
        );
    }
    CiRerunAdmission::Reserved
}
