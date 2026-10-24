//! How run surfaces show the review layer a run captured [ORB-16572]. Job
//! projections and review's own run reads print it.

use orbit_types::workflow::ReviewAdmission;
use serde_json::Value;

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
