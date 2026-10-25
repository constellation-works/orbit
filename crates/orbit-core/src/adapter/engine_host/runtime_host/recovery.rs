//! Run-record reads that step and final recovery admission depend on.

use orbit_common::OrbitError;

use crate::OrbitRuntime;

pub(super) fn source_repair_admission(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError> {
    runtime
        .get_job_run_backend(run_id)?
        .and_then(|run| run.input)
        .and_then(|input| input.get(orbit_types::workflow::SOURCE_REPAIR_KEY).cloned())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| OrbitError::JobValidation(format!("source repair admission unreadable: {e}")))
}

pub(super) fn final_recovery_log_tail(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<String>, OrbitError> {
    Ok(runtime
        .read_pipeline_worker_log(run_id)?
        .and_then(|snapshot| snapshot.content))
}
