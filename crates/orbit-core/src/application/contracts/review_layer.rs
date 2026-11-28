//! How run surfaces show the review layer a run captured [ORB-16572]. Job
//! projections and review's own run reads print it.

use orbit_types::workflow::{ReviewAdmission, WORKSPACE_PULL_PIPELINE_JOB};
use serde_json::Value;

/// Scope marker for a pull supervisor's captured local review configuration,
/// which is diagnostic only and never governs claimed leaves.
pub const SUPERVISOR_LOCAL_REVIEW_SCOPE: &str = "supervisor_local";

/// The review layer an admission captured and what decided it, as run
/// surfaces show it [ORB-16572]: `source` is `run-option` when the run's
/// `--review` added it, else the policy layer (`workspace`, `global`,
/// `built-in`) or `claim` for a claimed leaf.
pub(crate) fn review_layer_json(admission: &ReviewAdmission) -> Value {
    serde_json::json!({
        "timing": admission.timing.as_str(),
        "source": admission.timing_source,
    })
}

/// [`review_layer_json`] scoped for `job_id`: a pull supervisor's snapshot
/// reflects only its local configuration and never governs the claims it
/// pulls, so it carries `scope: "supervisor_local"`.
pub(crate) fn review_layer_json_for_run(job_id: &str, admission: &ReviewAdmission) -> Value {
    let mut value = review_layer_json(admission);
    if job_id == WORKSPACE_PULL_PIPELINE_JOB {
        value["scope"] = Value::String(SUPERVISOR_LOCAL_REVIEW_SCOPE.to_string());
    }
    value
}
