use orbit_engine::DispatchError;
use orbit_store::contracts::InvocationInsertParams;
use orbit_types::telemetry::InvocationTrace;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::task_context;

pub(super) fn persist_invocation_trace(
    runtime: &OrbitRuntime,
    job_run_id: &str,
    activity_id: &str,
    provider: &str,
    model: Option<&str>,
    input: &Value,
    trace: &InvocationTrace,
) -> Result<(), DispatchError> {
    let (agent, model) = runtime.invocation_agent_model_identity(
        provider,
        model,
        trace.provider_model.as_deref(),
        job_run_id,
        activity_id,
    );
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: job_run_id.to_string(),
            activity_id: activity_id.to_string(),
            agent: agent.unwrap_or_else(|| provider.to_ascii_lowercase()),
            model,
            task_ids: task_context::associated_task_ids(input),
            trace: trace.clone(),
        })
        .map_err(|error| {
            DispatchError::JobExecution(format!("persist invocation trace: {error}"))
        })?;

    let existing = runtime
        .get_job_run_backend(job_run_id)
        .map_err(|error| {
            DispatchError::JobExecution(format!("read job run for knowledge metrics: {error}"))
        })?
        .and_then(|run| run.knowledge_metrics);
    if let Some(metrics) = crate::metrics::merge_invocation_trace(existing.as_ref(), trace) {
        runtime
            .stores()
            .jobs()
            .record_job_run_knowledge_metrics(job_run_id, metrics)
            .map_err(|error| {
                DispatchError::JobExecution(format!("record job-run knowledge metrics: {error}"))
            })?;
    }

    Ok(())
}
