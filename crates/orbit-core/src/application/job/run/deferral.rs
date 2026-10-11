//! Runs that stopped without failing because their work was deferred, as
//! Incidents reads them [ORB-15517].
//!
//! A claimed leaf its provider's usage limit stopped ends `held`, and a CI
//! sweep whose only gaps were run-scoped retryable errors ends `success`.
//! Neither leaves a failed run behind, so each records one
//! [`RUN_DEFERRED_SURFACE`] audit row, which the incident classifier lists
//! as `deferred` rather than as a failure.

use orbit_store::contracts::RUN_DEFERRED_SURFACE;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{HeldFailure, JobRun};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::log_best_effort;

impl OrbitRuntime {
    /// Record that `run_id` deferred its work for `reason`, explained by
    /// `message`, with `detail` kept in the row's arguments. Best effort: the
    /// run's outcome stands either way.
    ///
    /// The row's status is `failure` only because Incidents reads non-success
    /// rows; its surface is failure-only, so no tool's failure rate counts it.
    /// Per-run specifics belong in `detail`, so repeated deferrals share one
    /// message and collapse into one incident.
    pub(crate) fn record_run_deferral(
        &self,
        run_id: Option<&str>,
        reason: &str,
        message: &str,
        detail: Value,
    ) {
        let result = self.record_pipeline_audit(
            RUN_DEFERRED_SURFACE,
            run_id,
            None,
            AuditEventStatus::Failure,
            json!({ "run_id": run_id, "reason": reason, "detail": detail }),
            Some(message.to_string()),
        );
        log_best_effort("record run deferral", run_id.unwrap_or_default(), result);
    }

    /// Record the deferral of a run that ended `held` on a provider's usage
    /// limit; every other hold already names its own resumption.
    pub(crate) fn record_held_deferral(
        &self,
        run: &JobRun,
        held: Option<HeldFailure>,
        message: &str,
    ) {
        if let Some(held @ HeldFailure::ProviderLimit) = held {
            self.record_run_deferral(Some(&run.run_id), held.error_code(), message, Value::Null);
        }
    }
}
