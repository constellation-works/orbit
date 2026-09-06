//! Minimal duplication of the `*_to_json` projection helpers that the dashboard
//! API delegates to. These were originally in orbit-cli under command/* but are
//! duplicated here (verbatim logic) so orbit-web compiles in isolation
//! without a dependency on orbit-cli (per ARCHITECTURE layering rules).

use std::collections::{BTreeMap, BTreeSet};

use orbit_core::application::job::JobCatalogEntry;
use orbit_core::{
    AuditEvent, JobRun, OrbitError, OrbitRuntime, ResolvedCrewProjection, Task, TaskStatus,
    resolve_task_dependencies,
};
use orbit_types::task::ArtifactManifestFileV2;
use orbit_types::workflow::{JobV2Step, JobV2StepBody};
use serde_json::{Value, json};

pub(crate) fn audit_event_to_json(event: &AuditEvent) -> Value {
    let actor = event.actor();
    json!({
        "id": event.id,
        "execution_id": event.execution_id,
        "timestamp": event.timestamp.to_rfc3339(),
        "command": event.command,
        "subcommand": event.subcommand,
        "tool_name": event.tool_name,
        "target_type": event.target_type,
        "target_id": event.target_id,
        "role": event.role,
        // ORB-10888: the canonical actor beside the raw label. `role` stays
        // byte-for-byte what was recorded; these are derived.
        "actor": actor.id,
        "actor_kind": actor.kind.to_string(),
        "actor_vendor": actor.vendor,
        "actor_family": actor.family,
        "actor_model": actor.model,
        "status": event.status.to_string(),
        "exit_code": event.exit_code,
        "duration_ms": event.duration_ms,
        "working_directory": event.working_directory,
        "arguments_json": event.arguments_json,
        "stdout_truncated": event.stdout_truncated,
        "stderr_truncated": event.stderr_truncated,
        "error_message": event.error_message,
        "host": event.host,
        "pid": event.pid,
        "session_id": event.session_id,
        "workspace_id": event.workspace_id,
        "caller_machine_id": event.caller_machine_id,
        "caller_host_id": event.caller_host_id,
        "process_machine_id": event.process_machine_id,
        "process_host_id": event.process_host_id,
        "transport": event.transport,
        "trace_id": event.trace_id,
        "caller_ip": event.caller_ip,
        "effective_capabilities": event.effective_capabilities,
        "origin_session_id": event.origin_session_id,
        "mcp_call_id": event.mcp_call_id,
        "lease_id": event.lease_id,
        "task_id": event.task_id,
        "job_run_id": event.job_run_id,
        "activity_id": event.activity_id,
        "step_index": event.step_index,
    })
}

pub(crate) fn job_catalog_to_json_with_last_run(
    job: &JobCatalogEntry,
    last_run: Option<&JobRun>,
) -> Value {
    let mut value = json!({
        "job_id": job.job_id.clone(),
        "kind": job.kind().to_string(),
        "state": job.state().to_string(),
        "default_input": job.spec.default_input,
        "max_active_runs": job.spec.max_active_runs,
        "steps": job.spec.steps.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
        "path": job.path.display().to_string(),
    });
    value["last_run_state"] = last_run
        .map(|r| serde_json::Value::String(r.state.to_string()))
        .unwrap_or(serde_json::Value::Null);
    value["last_run_at"] = last_run
        .and_then(|r| r.finished_at.or(r.started_at).or(Some(r.scheduled_at)))
        .map(|ts| serde_json::Value::String(ts.to_rfc3339()))
        .unwrap_or(serde_json::Value::Null);
    value
}

fn job_v2_step_to_json(step: &JobV2Step) -> Value {
    let mut value = json!({
        "id": step.id.clone(),
        "when": step.when,
        "retry": step.retry,
    });
    match &step.body {
        JobV2StepBody::TargetRef(target) => {
            value["body"] = json!({
                "kind": "target_ref",
                "target": target.target.clone(),
                "default_input": target.default_input,
                "timeout_seconds": target.timeout_seconds,
                "session": target.session,
            });
        }
        JobV2StepBody::Target(target) => {
            value["body"] = json!({
                "kind": "target",
                "default_input": target.default_input,
                "timeout_seconds": target.timeout_seconds,
                "session": target.session,
                "spec": target.spec,
            });
        }
        JobV2StepBody::Parallel { parallel } => {
            value["body"] = json!({
                "kind": "parallel",
                "join": parallel.join,
                "branches": parallel.branches.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
            });
        }
        JobV2StepBody::FanOut { fan_out, fan_in } => {
            value["body"] = json!({
                "kind": "fan_out",
                "items": fan_out.items,
                "max_workers": fan_out.max_workers,
                "worker": job_v2_step_to_json(&fan_out.worker),
                "fan_in": fan_in,
            });
        }
        JobV2StepBody::Loop { loop_ } => {
            value["body"] = json!({
                "kind": "loop",
                "max_iterations": loop_.max_iterations,
                "break_when": loop_.break_when,
                "steps": loop_.steps.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
            });
        }
    }
    value
}

pub(crate) fn task_to_json(task: &Task, status_by_id: &BTreeMap<String, TaskStatus>) -> Value {
    json!({
        "id": task.id,
        "parent_id": task.parent_id(),
        "title": task.title,
        "description": task.description,
        "acceptance_criteria": task.acceptance_criteria,
        "dependencies": task.dependencies(),
        "resolved_dependencies": dependency_labels(task, status_by_id),
        "tags": task.tags,
        "required_tools": task.required_tools,
        "plan": task.plan,
        "execution_summary": task.execution_summary,
        "context_files": task.context_files,
        "created_by": task.created_by,
        "planned_by": task.planned_by,
        "implemented_by": task.implemented_by,
        "status": task.status.to_string(),
        "priority": task.priority.to_string(),
        "complexity": task.complexity.map(|value| value.to_string()),
        "type": task.task_type.to_string(),
        "pr_status": task.pr_status,
        "external_refs": task.external_refs,
        "relations": orbit_types::task::resolve_task_relations(task, status_by_id),
        "source_task_id": task.source_task_id(),
        "job_run_id": task.job_run_id,
        "crew": task.crew,
        "orchestrator": task.orchestrator,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
    })
}

pub(crate) fn task_to_json_with_sidecars(
    runtime: &OrbitRuntime,
    task: &Task,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Value, OrbitError> {
    let row = runtime.get_task_row(&task.id)?;
    task_row_to_json(runtime, &row, status_by_id)
}

pub(crate) fn task_row_to_json(
    runtime: &OrbitRuntime,
    row: &orbit_core::application::task::TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Value, OrbitError> {
    let task = &row.task;
    let mut value = task_to_json(task, status_by_id);
    let object = value.as_object_mut().ok_or_else(|| {
        OrbitError::Execution("task JSON projection did not produce an object".to_string())
    })?;
    object.insert(
        "comments".to_string(),
        serde_json::to_value(&row.comments).map_err(|e| OrbitError::Io(e.to_string()))?,
    );
    object.insert(
        "history".to_string(),
        serde_json::to_value(&row.history).map_err(|e| OrbitError::Io(e.to_string()))?,
    );
    object.insert(
        "artifacts".to_string(),
        task_artifact_manifest_to_json(&row.artifacts),
    );
    if let Some(projection) = dashboard_resolved_crew_projection(runtime, task)? {
        object.insert("resolved_crew".to_string(), Value::String(projection.name));
        object.insert("crew_model".to_string(), Value::String(projection.model));
    }
    Ok(value)
}

fn dashboard_resolved_crew_projection(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<Option<ResolvedCrewProjection>, OrbitError> {
    if task_has_stale_explicit_crew(runtime, task) {
        let crew = runtime.resolve_crew_for_task(None, None)?;
        return Ok(Some(ResolvedCrewProjection {
            name: crew.name,
            model: crew.assignment.model,
        }));
    }
    runtime.resolved_crew_projection(task)
}

fn task_has_stale_explicit_crew(runtime: &OrbitRuntime, task: &Task) -> bool {
    let Some(stored_crew) = task
        .crew
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    !runtime
        .configured_crew_registry_projection()
        .crews
        .iter()
        .any(|crew| crew.name == stored_crew)
}

pub(crate) fn task_artifact_manifest_to_json(files: &[ArtifactManifestFileV2]) -> Value {
    Value::Array(
        files
            .iter()
            .map(|file| {
                json!({
                    "path": file.path,
                    "media_type": file.media_type,
                    "size_bytes": file.size_bytes,
                    "sha256": file.sha256,
                    "created_by": file.created_by,
                    "created_at": file.created_at.to_rfc3339(),
                })
            })
            .collect(),
    )
}

fn dependency_labels(task: &Task, status_by_id: &BTreeMap<String, TaskStatus>) -> Vec<String> {
    resolve_task_dependencies(task, status_by_id)
        .into_iter()
        .map(|dependency| dependency.label())
        .collect()
}

pub(crate) fn task_lock_to_json(task: &Task) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "status": task.status.to_string(),
        "job_run_id": task.job_run_id,
        "crew": task.crew,
        "orchestrator": task.orchestrator,
        "context_files": task.context_files,
    })
}

pub(crate) fn task_locks_json(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    let (tasks, locked_files) = task_locks(runtime)?;
    let json_by_task: Vec<Value> = tasks.iter().map(task_lock_to_json).collect();
    Ok(json!({
        "locked_files": locked_files.iter().cloned().collect::<Vec<_>>(),
        "by_task": json_by_task,
        "total_locked": locked_files.len(),
        "total_tasks": tasks.len(),
    }))
}

fn task_locks(runtime: &OrbitRuntime) -> Result<(Vec<Task>, BTreeSet<String>), OrbitError> {
    let mut tasks: Vec<_> = runtime
        .list_tasks()?
        .into_iter()
        .filter(|task| matches!(task.status, TaskStatus::InProgress | TaskStatus::Review))
        .collect();

    tasks.sort_by_key(|task| {
        (
            task_lock_status_rank(task.status),
            task.created_at,
            task.id.clone(),
        )
    });

    let locked_files: BTreeSet<String> = tasks
        .iter()
        .flat_map(|task| task.context_files.iter().cloned())
        .collect();

    Ok((tasks, locked_files))
}

fn task_lock_status_rank(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Review => 1,
        _ => 2,
    }
}
