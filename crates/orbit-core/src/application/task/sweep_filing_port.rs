//! What task-pilot apply needs from CI-sweep admission policy [ORB-17936].
//!
//! A CI-sweep pilot admits the one task the sweep filed only when the
//! assessment correlates with the sweep's source evidence, and settles a
//! filed task another pilot already prepared as superseded. `sweep_filing`
//! owns that policy, and it also files tasks, so a direct call from here
//! would close a cycle. This port is the way in: `sweep_filing` implements
//! it on [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime
//! binds it.

use orbit_engine::DispatchError;
use orbit_types::task::Task;
use serde::Serialize;
use serde_json::Value;

pub(crate) use super::ci_sweep_filing::{CiSweepFilingEvidence, CiSweepFilingSource};

/// Whether a pilot's admission authority permits promotion.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromotionDecision {
    Promote,
    Withhold,
}

/// Closure fields appended after admission and approval, in persisted order.
#[derive(Default, Serialize)]
pub(crate) struct AdmissionClosure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) closed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) covering_commits: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) not_closed_reason: Option<String>,
}

/// A CI-sweep admission record, with fields in their persisted insertion order.
#[derive(Serialize)]
pub(crate) struct CiSweepAdmissionDecision {
    pub(crate) task_id: String,
    pub(crate) decision: PromotionDecision,
    pub(crate) classification: String,
    pub(crate) promotion_authorized: bool,
    pub(crate) failure_key: String,
    pub(crate) source: Value,
    pub(crate) evidence: Value,
    #[serde(flatten)]
    pub(crate) closure: AdmissionClosure,
}

/// What CI-sweep admission decided for a validated assessment.
pub(crate) enum CiSweepAdmission {
    /// The admission decision apply records.
    Decision(Box<CiSweepAdmissionDecision>),
    /// The task outcome of a filing another admission already met.
    Superseded(Value),
}

/// The CI-sweep admission policy task-pilot apply calls, implemented by
/// `sweep_filing`.
pub(in crate::application) trait TaskSweepFilingPort {
    /// Admit the task `filing` names from its validated `assessment` and
    /// `selectors`, or refuse it, correlating both with the sweep's source
    /// evidence.
    #[allow(clippy::too_many_arguments)]
    fn ci_sweep_admission(
        &self,
        action: &str,
        task_id: &str,
        task: &Task,
        assessment: &Value,
        selectors: &[String],
        filing: &CiSweepFilingEvidence,
        promotion_authorized: bool,
        dependencies: &[String],
        is_done: &dyn Fn(&str) -> bool,
    ) -> Result<CiSweepAdmission, DispatchError>;

    /// The superseded outcome for CI-sweep task `task_id` when `prepared`
    /// reports it excluded only because another active pilot prepared it.
    fn ci_sweep_piloted_elsewhere(
        &self,
        action: &str,
        task_id: &str,
        prepared: &Value,
    ) -> Result<Option<Value>, DispatchError>;
}
