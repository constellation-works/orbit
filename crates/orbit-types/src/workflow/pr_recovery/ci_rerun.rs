//! Unchanged-head CI retries, reserved before external mutation.
use crate::task::TaskHistoryEntry;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable reservation, including unknown provider outcomes.
pub const CI_RERUN_RESERVED_EVENT: &str = "ci_check_rerun_reserved";

/// The independently classified root of a workflow-wide failed-jobs retry.
/// `classification` is `transient`, `flaky` or `unattributed`: a failure
/// whose cause no evidence established yet, rerun once per head so its
/// [`CiFailureClass`](super::CiFailureClass) can be told apart.
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

/// Owner-configured budget for logged infrastructure/transient failures.
/// Conclusion-only cancellations/timeouts and flaky failures retain one rerun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CiInfrastructureRetryPolicy {
    pub max_attempts: u32,
    pub backoff_seconds: u64,
}

impl Default for CiInfrastructureRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_seconds: 60,
        }
    }
}

/// A logged transient set can use the infrastructure budget. A candidate or
/// flaky sibling, or a conclusion-only stop, cannot expand its authorization.
pub fn ci_rerun_is_infrastructure(plan: &CiRerunPlan) -> bool {
    !plan.roots.is_empty()
        && plan.roots.iter().all(|root| {
            root.classification == "transient" && !root.signature.starts_with("concluded ")
        })
}

/// Admission is atomic under the owning task/claim boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiRerunAdmission {
    Reserved,
    /// A durable prior reservation imposes this absolute not-before.
    Backoff {
        not_before_ms: u64,
    },
    /// No source repair is authorized by exhaustion of infrastructure retries.
    InfrastructureExhausted {
        attempts: u32,
    },
    Observe(Box<CiRerunPlan>),
    Refused(String),
}

/// Legacy one-rerun admission for consumers without infrastructure policy.
pub fn decide_ci_rerun(history: &[TaskHistoryEntry], plan: &CiRerunPlan) -> CiRerunAdmission {
    decide_ci_rerun_with_policy(
        history,
        plan,
        CiInfrastructureRetryPolicy {
            max_attempts: 1,
            backoff_seconds: 0,
        },
        u64::MAX,
    )
}

/// Decide under the owning task/claim lock; unknown sends consume a reservation.
/// Only a newly authenticated provider attempt can spend the next reservation.
pub fn decide_ci_rerun_with_policy(
    history: &[TaskHistoryEntry],
    plan: &CiRerunPlan,
    policy: CiInfrastructureRetryPolicy,
    now_ms: u64,
) -> CiRerunAdmission {
    if plan.schema_version != 1
        || plan.roots.is_empty()
        || plan.failed_jobs.is_empty()
        || plan.roots.iter().any(|root| {
            !matches!(
                root.classification.as_str(),
                "transient" | "flaky" | "unattributed"
            )
        })
    {
        return CiRerunAdmission::Refused("incomplete CI retry authorization".into());
    }
    let mut reserved: Vec<(CiRerunPlan, u64)> = Vec::new();
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
                if let Some((existing, _)) = reserved.iter().find(|(existing, _)| {
                    existing.run_id == prior.run_id && existing.run_attempt == prior.run_attempt
                }) {
                    if existing != &prior {
                        return CiRerunAdmission::Refused(
                            "conflicting CI retry reservations for this attempt".into(),
                        );
                    }
                } else {
                    reserved.push((
                        prior,
                        u64::try_from(entry.at.timestamp_millis()).unwrap_or(0),
                    ));
                }
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
    if let Some((prior, _)) = reserved
        .iter()
        .find(|(prior, _)| prior.run_id == plan.run_id && plan.run_attempt <= prior.run_attempt)
    {
        return CiRerunAdmission::Observe(Box::new(prior.clone()));
    }
    if let Some((prior, _)) = reserved.last() {
        if !ci_rerun_is_infrastructure(plan) {
            // Flaky and conclusion-only failures retain the shared one-rerun
            // contract: observe its reservation even on a later attempt.
            return CiRerunAdmission::Observe(Box::new(prior.clone()));
        }
        let attempts = u32::try_from(reserved.len()).unwrap_or(u32::MAX);
        if attempts >= policy.max_attempts {
            return CiRerunAdmission::InfrastructureExhausted { attempts };
        }
        let last_ms = reserved.iter().map(|(_, at)| *at).max().unwrap_or(0);
        let delay_ms = policy
            .backoff_seconds
            .saturating_mul(1000)
            .saturating_mul(1_u64 << attempts.saturating_sub(1).min(20));
        let not_before_ms = last_ms.saturating_add(delay_ms);
        if now_ms < not_before_ms {
            return CiRerunAdmission::Backoff { not_before_ms };
        }
    } else if ci_rerun_is_infrastructure(plan) && policy.max_attempts == 0 {
        return CiRerunAdmission::InfrastructureExhausted { attempts: 0 };
    }
    if legacy_spent && reserved.is_empty() {
        return CiRerunAdmission::Refused(
            "the shared unchanged-head CI retry budget is spent".into(),
        );
    }
    CiRerunAdmission::Reserved
}
