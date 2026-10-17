//! The `*_to_json` projections the dashboard API serves. A task detail builds
//! on the show record every read surface shares
//! (`orbit_core::application::task::task_show_json`) and adds only the
//! dashboard's own extras. A mutation response builds on `task_record_json`
//! instead, so it does not carry show-only guidance. An audit event builds on
//! the base the CLI shares (`orbit_types::telemetry::audit_event_base_json`).
//! The job-catalog projection lives in `orbit_core::application::job`; the
//! other projections are dashboard-local.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use orbit_common::fs::overlap_index::OverlapIndex;
use orbit_core::application::task::{
    PullRequestLinks, TaskRow, declared_context_files, task_record_json, task_show_json,
    task_status_transition_allowed, task_status_transition_required_field,
};
use orbit_core::runtime::engine::ConfiguredCrewRegistryProjection;
use orbit_core::{AuditEvent, OrbitError, OrbitRuntime, ResolvedCrewProjection, Task, TaskStatus};
use orbit_types::task::{TaskEnvelopeV2, task_base_json};
use orbit_types::telemetry::audit_event_base_json;
use serde_json::{Map, Value, json};

pub(crate) fn audit_event_to_json(event: &AuditEvent) -> Value {
    let actor = event.actor();
    let mut value = audit_event_base_json(event);
    // ORB-10888: the canonical actor beside the raw label. `role` stays
    // byte-for-byte what was recorded; these are derived.
    value["actor"] = json!(actor.id);
    value["actor_kind"] = json!(actor.kind.to_string());
    value["actor_vendor"] = json!(actor.vendor);
    value["actor_family"] = json!(actor.family);
    value["actor_model"] = json!(actor.model);
    value
}

/// The shared base task projection with the dashboard's one change to it:
/// `links` fills the page of a `github-pr` ref recorded without one, so the
/// dashboard can link every delivered pull request.
pub(crate) fn task_to_json(
    task: &Task,
    status_by_id: &BTreeMap<String, TaskStatus>,
    links: &PullRequestLinks,
) -> Map<String, Value> {
    let mut object = task_base_json(task, status_by_id);
    link_pull_requests(&mut object, task, links);
    object
}

fn link_pull_requests(object: &mut Map<String, Value>, task: &Task, links: &PullRequestLinks) {
    let mut external_refs = task.external_refs.clone();
    links.link(&mut external_refs);
    object.insert("external_refs".to_string(), json!(external_refs));
}

/// Tasks an active task-pilot run is preparing. Their readiness gaps are about
/// to be settled by that run, so the dashboard shows them as preparing. The
/// flag is decoration on a read: a job store that cannot be read leaves every
/// task unmarked rather than failing the task read.
fn tasks_in_pilot_preparation(runtime: &OrbitRuntime) -> BTreeSet<String> {
    runtime
        .tasks_in_pilot_preparation()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "task readiness: active pilot preparations unreadable");
            BTreeSet::new()
        })
}

/// Add `readiness.preparing` to a projection that carries readiness.
fn mark_readiness_preparing(value: &mut Value, preparing: bool) {
    if let Some(readiness) = value.get_mut("readiness").and_then(Value::as_object_mut) {
        readiness.insert("preparing".to_string(), Value::Bool(preparing));
    }
}

pub(crate) fn task_to_json_with_sidecars(
    runtime: &OrbitRuntime,
    task: &Task,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Value, OrbitError> {
    let row = runtime.get_task_row(&task.id)?;
    task_row_to_json(runtime, &row, status_by_id)
}

/// A mutation response: [`task_record_json`] plus the dashboard extras. It does
/// not carry the show-only `next_step` and `final_recovery` guidance.
/// `GET /api/tasks/:id` serves [`task_detail_json`] instead. List endpoints
/// serve [`TaskListProjection`].
pub(crate) fn task_row_to_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Value, OrbitError> {
    let object = task_record_json(runtime, row, status_by_id)?;
    with_dashboard_extras(runtime, row, object)
}

/// `GET /api/tasks/:id`: [`task_show_json`] plus the dashboard extras — linked
/// pull requests, the lifecycle transitions with their evidence requirements,
/// run navigability and `readiness.preparing`.
pub(crate) fn task_detail_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Value, OrbitError> {
    let object = task_show_json(runtime, row, status_by_id)?;
    with_dashboard_extras(runtime, row, object)
}

/// Linked pull requests, lifecycle transitions, run navigability and
/// `readiness.preparing`. These are dashboard controls, not part of the shared
/// task record.
fn with_dashboard_extras(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    mut object: Map<String, Value>,
) -> Result<Value, OrbitError> {
    let task = &row.task;
    link_pull_requests(&mut object, task, &PullRequestLinks::new(runtime));
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
        Value::Bool(job_run_is_locally_navigable(
            runtime.automation_machine_identity(),
            task,
        )),
    );
    let mut value = Value::Object(object);
    if value.get("readiness").is_some() {
        let preparing = tasks_in_pilot_preparation(runtime).contains(&task.id);
        mark_readiness_preparing(&mut value, preparing);
    }
    Ok(value)
}

fn job_run_is_locally_navigable(local_machine_id: Option<&str>, task: &Task) -> bool {
    if task.job_run_id.is_none() {
        return false;
    }
    let Some(host) = task.job_run_machine.as_ref() else {
        return true;
    };
    // A recorded host has to *match* to navigate: with no local identity there
    // is nothing to prove the run is here, and an owner-local link would open
    // the wrong thing or nothing. An unrecorded host is handled above.
    local_machine_id.is_some_and(|local| local == host.machine_id)
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
/// their evidence requirement, whether the linked run is navigable on this
/// machine, and a crew resolved purely from the registry built once here —
/// never a store call per row. Navigability uses the machine id captured with
/// the registry and the row's own `job_run_id` / `job_run_machine`.
pub(crate) struct TaskListProjection {
    registry: ConfiguredCrewRegistryProjection,
    local_machine_id: Option<String>,
    /// Read once, on the page's first proposed or backlog row.
    preparing: OnceCell<BTreeSet<String>>,
    /// Reads the origin remote once, on the page's first unlinked PR ref.
    pull_request_links: PullRequestLinks,
}

impl TaskListProjection {
    pub(crate) fn new(runtime: &OrbitRuntime) -> Self {
        Self {
            registry: runtime.configured_crew_registry_projection(),
            local_machine_id: runtime.automation_machine_identity().map(str::to_string),
            preparing: OnceCell::new(),
            pull_request_links: PullRequestLinks::new(runtime),
        }
    }

    /// `runtime` is the one this projection was built from; it is read only
    /// for the pilot preparations behind `readiness.preparing`.
    pub(crate) fn row_to_json(
        &self,
        runtime: &OrbitRuntime,
        row: &TaskRow,
        status_by_id: &BTreeMap<String, TaskStatus>,
    ) -> Result<Value, OrbitError> {
        let task = &row.task;
        let mut object = task_to_json(task, status_by_id, &self.pull_request_links);
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
        // Same decision as the detail projection. The list used to omit the
        // flag, and the dashboard treats a missing value as permission to
        // link `#runs?run_id=` on this host.
        object.insert(
            "job_run_navigable".to_string(),
            Value::Bool(job_run_is_locally_navigable(
                self.local_machine_id.as_deref(),
                task,
            )),
        );
        let mut value = Value::Object(object);
        if value.get("readiness").is_some() {
            let preparing = self
                .preparing
                .get_or_init(|| tasks_in_pilot_preparation(runtime))
                .contains(&task.id);
            mark_readiness_preparing(&mut value, preparing);
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

/// Registry-only crew resolution for summary rows: the task's explicit crew
/// when the registry still configures it, otherwise the configured default,
/// otherwise nothing.
///
/// This is the dashboard-only display rule the shared read contract
/// (`OrbitRuntime::task_crew_read`, which the detail serves) names as its
/// exception: the run-recorded crew needs a job-run read, which is exactly the
/// per-row lookup the list path gives up, so a summary row never reports
/// `crew_unresolved` and shows the default for a crew this host no longer
/// configures. The dashboard reads `resolved_crew` only for a task with no
/// explicit crew, and flags a stale explicit crew itself.
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

pub(crate) fn task_lock_to_json(task: &TaskEnvelopeV2) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "status": task.status.to_string(),
        "job_run_id": task.job_run_id,
        "crew": task.crew,
        "crew_source": task.crew_source,
        "orchestrator": task.orchestrator,
        "context_files": task.context_files,
    })
}

pub(crate) fn task_locks_json(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    let candidates = runtime.task_candidates(
        &orbit_core::application::task::TaskListFilter {
            statuses: Some(vec![
                TaskStatus::Backlog,
                TaskStatus::InProgress,
                TaskStatus::Review,
            ]),
            ..Default::default()
        },
        usize::MAX,
    )?;
    // Reuse admission's declaration expansion and indexed overlap rules. The
    // informational population is deliberately narrower than lock holders:
    // review tasks still appear in by_task, but never cause these highlights.
    let mut contenders = OverlapIndex::new();
    for task in &candidates.items {
        if task.status == TaskStatus::InProgress {
            for selector in
                declared_context_files(&task.context_files, &runtime.paths().repo_root).retained
            {
                contenders.insert(&selector, (task.id.clone(), task.status, selector.clone()));
            }
        }
    }
    let contention: BTreeMap<String, Value> = candidates.items.iter()
        .filter(|task| matches!(task.status, TaskStatus::Backlog | TaskStatus::InProgress))
        .map(|task| {
            let entries: Vec<Value> = task.context_files.iter().map(|selector| {
                let expanded = declared_context_files(std::slice::from_ref(selector), &runtime.paths().repo_root);
                let matches: Vec<Value> = expanded.retained.iter()
                    .flat_map(|requested| contenders.overlapping(requested))
                    .filter(|(_, (id, _, _))| *id != task.id)
                    .map(|(_, (id, status, held))| json!({"id": id, "status": status.to_string(), "selector": held}))
                    .collect();
                json!({"selector": selector, "known": expanded.invalid.is_empty(), "contenders": matches})
            }).collect();
            (task.id.clone(), json!({"status": task.status.to_string(), "entries": entries}))
        }).collect();
    let (tasks, locked_files) = task_locks(candidates.items);
    let json_by_task: Vec<Value> = tasks.iter().map(task_lock_to_json).collect();
    Ok(json!({
        "locked_files": locked_files.iter().cloned().collect::<Vec<_>>(),
        "by_task": json_by_task,
        "total_locked": locked_files.len(),
        "total_tasks": tasks.len(),
        "context_contention": contention,
    }))
}

fn task_locks(mut tasks: Vec<TaskEnvelopeV2>) -> (Vec<TaskEnvelopeV2>, BTreeSet<String>) {
    tasks.retain(|task| matches!(task.status, TaskStatus::InProgress | TaskStatus::Review));

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

    (tasks, locked_files)
}

fn task_lock_status_rank(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Review => 1,
        _ => 2,
    }
}
