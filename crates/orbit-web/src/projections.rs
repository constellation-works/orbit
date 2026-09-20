//! Minimal duplication of the `*_to_json` projection helpers that the dashboard
//! API delegates to. These were originally in orbit-cli under command/* but are
//! duplicated here (verbatim logic) so orbit-web compiles in isolation
//! without a dependency on orbit-cli (per ARCHITECTURE layering rules).

use std::collections::{BTreeMap, BTreeSet};

use orbit_core::application::job::JobCatalogEntry;
use orbit_core::application::task::{
    TaskRow, task_status_transition_allowed, task_status_transition_required_field,
};
use orbit_core::runtime::engine::ConfiguredCrewRegistryProjection;
use orbit_core::{
    AuditEvent, JobRun, OrbitError, OrbitRuntime, ResolvedCrewProjection, Task, TaskStatus,
    resolve_task_dependencies,
};
use orbit_types::task::{ArtifactManifestFileV2, TaskEnvelopeV2};
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
        // Host-qualified execution provenance [ORB-12516]. A pulled task's run
        // lives in the executing host's own job store, so the run id alone is a
        // dangling reference without the machine that ran it. Absent stays
        // absent: a row recorded before execution provenance existed is
        // *unknown*, never "the owner".
        "job_run_host": task.job_run_host,
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

/// The full task projection: every body, the sidecars, the lifecycle
/// transitions with their evidence requirements, the run-aware crew
/// resolution, and the review gate. Served by `GET /api/tasks/:id` and every
/// mutation response; the list endpoints serve [`TaskListProjection`] instead.
pub(crate) fn task_row_to_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
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
    object.insert(
        "status_transitions".to_string(),
        dashboard_status_transitions(runtime, task)?,
    );
    // ORB-12516: whether `#runs?run_id=` can resolve this task's run *here*.
    // A run recorded against another machine lives in that host's job store, so
    // the detail names the host to inspect instead of linking to nothing. An
    // unrecorded host keeps the historical local link — the provenance line
    // still reads *unknown*, which is what the absent field means.
    object.insert(
        "job_run_navigable".to_string(),
        Value::Bool(job_run_is_locally_navigable(runtime, task)),
    );
    let registry = runtime.configured_crew_registry_projection();
    if let Some(projection) = dashboard_resolved_crew_projection(runtime, &registry, task)? {
        object.insert("resolved_crew".to_string(), Value::String(projection.name));
        object.insert("crew_model".to_string(), Value::String(projection.model));
    }
    if let Some(review) =
        orbit_core::application::review::task_review_projection(runtime, task, &row.artifacts)?
    {
        object.insert("review".to_string(), review);
    }
    Ok(value)
}

fn job_run_is_locally_navigable(runtime: &OrbitRuntime, task: &Task) -> bool {
    if task.job_run_id.is_none() {
        return false;
    }
    let Some(host) = task.job_run_host.as_ref() else {
        return true;
    };
    // A recorded host has to *match* to navigate: with no local identity there
    // is nothing to prove the run is here, and an owner-local link would open
    // the wrong thing or nothing. An unrecorded host is handled above.
    runtime
        .automation_machine_identity()
        .is_some_and(|local| local == host.machine_id)
}

/// Marker the list rows carry so a client can tell a summary from the full
/// projection without probing for absent keys.
pub(crate) const TASK_SUMMARY_PROJECTION: &str = "summary";

/// One request's worth of list-row rendering state.
///
/// DANI-10391: a list page used to render every row through
/// [`task_row_to_json`], so fifty rows meant fifty serialised descriptions,
/// plans, comment and history logs, fifty crew-registry rebuilds, and one to
/// four store lookups per row (job runs behind the `done` evidence check and
/// the run-recorded crew, plus the review ledger). The dashboard reads none of
/// that until a row is expanded, and it fetches `GET /api/tasks/:id` for the
/// expansion. A summary row therefore carries the identifying and sortable
/// fields, counts in place of the bodies, the governed status targets without
/// their evidence requirement, and a crew resolved purely from the registry
/// built once here — never a store call per row.
pub(crate) struct TaskListProjection {
    registry: ConfiguredCrewRegistryProjection,
}

impl TaskListProjection {
    pub(crate) fn new(runtime: &OrbitRuntime) -> Self {
        Self {
            registry: runtime.configured_crew_registry_projection(),
        }
    }

    pub(crate) fn row_to_json(
        &self,
        row: &TaskRow,
        status_by_id: &BTreeMap<String, TaskStatus>,
    ) -> Result<Value, OrbitError> {
        let task = &row.task;
        let mut value = task_to_json(task, status_by_id);
        let object = value.as_object_mut().ok_or_else(|| {
            OrbitError::Execution("task JSON projection did not produce an object".to_string())
        })?;
        for body in TASK_SUMMARY_OMITTED_BODIES {
            object.remove(body);
        }
        object.insert(
            "projection".to_string(),
            Value::String(TASK_SUMMARY_PROJECTION.to_string()),
        );
        object.insert("comment_count".to_string(), json!(row.comments.len()));
        object.insert("history_count".to_string(), json!(row.history.len()));
        object.insert("artifact_count".to_string(), json!(row.artifacts.len()));
        object.insert(
            "status_transitions".to_string(),
            summary_status_transitions(task),
        );
        if let Some(projection) = registry_crew_projection(&self.registry, task) {
            object.insert("resolved_crew".to_string(), Value::String(projection.name));
            object.insert("crew_model".to_string(), Value::String(projection.model));
        }
        Ok(value)
    }
}

/// The prose a summary row leaves to the detail endpoint. Everything else in
/// [`task_to_json`] is a scalar or a short list a list consumer filters on
/// (bridge's `orbit_task_list` reads `dependencies`, `resolved_dependencies`,
/// `context_files`, `parent_id` and `job_run_id` off the list rows).
const TASK_SUMMARY_OMITTED_BODIES: [&str; 4] = [
    "description",
    "plan",
    "execution_summary",
    "acceptance_criteria",
];

const STATUS_ORDER: [TaskStatus; 9] = [
    TaskStatus::InProgress,
    TaskStatus::Review,
    TaskStatus::Blocked,
    TaskStatus::Proposed,
    TaskStatus::Backlog,
    TaskStatus::Someday,
    TaskStatus::Done,
    TaskStatus::Rejected,
    TaskStatus::Archived,
];

fn governed_status_targets(task: &Task) -> impl Iterator<Item = TaskStatus> {
    let current = task.status;
    STATUS_ORDER.into_iter().filter(move |target| {
        *target != current && task_status_transition_allowed(current, *target)
    })
}

fn dashboard_status_transitions(runtime: &OrbitRuntime, task: &Task) -> Result<Value, OrbitError> {
    governed_status_targets(task)
        .map(|target| {
            Ok(json!({
                "status": target.to_string(),
                "required_field": task_status_transition_required_field(runtime, task, target)?,
            }))
        })
        .collect::<Result<Vec<_>, OrbitError>>()
        .map(Value::Array)
}

/// The governed targets alone. `required_field` is deliberately absent rather
/// than `null`: the `done` requirement depends on the task's job run, which the
/// list path no longer reads, so a client that needs the requirement asks the
/// detail endpoint instead of treating "unknown" as "none".
fn summary_status_transitions(task: &Task) -> Value {
    Value::Array(
        governed_status_targets(task)
            .map(|target| json!({ "status": target.to_string() }))
            .collect(),
    )
}

fn dashboard_resolved_crew_projection(
    runtime: &OrbitRuntime,
    registry: &ConfiguredCrewRegistryProjection,
    task: &Task,
) -> Result<Option<ResolvedCrewProjection>, OrbitError> {
    if task_has_stale_explicit_crew(registry, task) {
        let crew = runtime.resolve_crew_for_task(None, None)?;
        return Ok(Some(ResolvedCrewProjection {
            name: crew.name,
            model: crew.assignment.model,
        }));
    }
    runtime.resolved_crew_projection(task)
}

/// Registry-only crew resolution for summary rows: the task's explicit crew
/// when the registry still configures it, otherwise the configured default,
/// otherwise nothing. The run-recorded crew a detail row prefers needs a job-run
/// read, which is exactly the per-row lookup the list path gives up.
fn registry_crew_projection(
    registry: &ConfiguredCrewRegistryProjection,
    task: &Task,
) -> Option<ResolvedCrewProjection> {
    let explicit = explicit_task_crew(task);
    let selected = explicit
        .filter(|name| registry.crews.iter().any(|crew| crew.name == *name))
        .or(registry.default_crew.as_deref())?;
    registry
        .crews
        .iter()
        .find(|crew| crew.name == selected)
        .map(|crew| ResolvedCrewProjection {
            name: crew.name.clone(),
            model: crew.model.clone(),
        })
}

fn explicit_task_crew(task: &Task) -> Option<&str> {
    task.crew
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn task_has_stale_explicit_crew(registry: &ConfiguredCrewRegistryProjection, task: &Task) -> bool {
    let Some(stored_crew) = explicit_task_crew(task) else {
        return false;
    };
    !registry.crews.iter().any(|crew| crew.name == stored_crew)
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

pub(crate) fn task_lock_to_json(task: &TaskEnvelopeV2) -> Value {
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

fn task_locks(
    runtime: &OrbitRuntime,
) -> Result<(Vec<TaskEnvelopeV2>, BTreeSet<String>), OrbitError> {
    let candidates = runtime.task_candidates(
        &orbit_core::application::task::TaskListFilter {
            statuses: Some(vec![TaskStatus::InProgress, TaskStatus::Review]),
            ..Default::default()
        },
        usize::MAX,
    )?;
    let mut tasks = candidates.items;

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
