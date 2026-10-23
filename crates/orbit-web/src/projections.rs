//! The `*_to_json` projections the dashboard API serves. A task detail builds
//! on the show record every read surface shares
//! (`orbit_core::application::task::task_show_json`) and adds only the
//! dashboard's own extras, through the shared core enrichment. A mutation
//! response is the core task-write projection in its dashboard full-record
//! profile, so it does not carry show-only guidance. An audit event builds on
//! the base the CLI shares (`orbit_types::telemetry::audit_event_base_json`).
//! The job-catalog projection lives in `orbit_core::application::job`; the
//! list summary and the other projections are dashboard-local.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use orbit_core::application::task::{
    PullRequestLinks, TaskOsReadout, TaskRow, job_run_is_locally_navigable, link_pull_requests,
    mark_readiness_preparing, summary_status_transitions, task_detail_with_dashboard_extras,
    task_pilot_warnings, tasks_in_pilot_preparation,
};
use orbit_core::runtime::engine::ConfiguredCrewRegistryProjection;
use orbit_core::{AuditEvent, OrbitError, OrbitRuntime, ResolvedCrewProjection, Task, TaskStatus};
use orbit_types::task::task_base_json;
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

/// `GET /api/tasks/:id`: the show record plus the dashboard extras — linked
/// pull requests, the lifecycle transitions with their evidence requirements,
/// run navigability and `readiness.preparing`.
pub(crate) fn task_detail_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Value, OrbitError> {
    task_detail_with_dashboard_extras(runtime, row, status_by_id)
}

/// Marker the list rows carry so a client can tell a summary from the full
/// projection without probing for absent keys.
pub(crate) const TASK_SUMMARY_PROJECTION: &str = "summary";

/// One request's worth of list-row rendering state.
///
/// DANI-10391: a list page used to render every row through
/// the full record projection, so fifty rows meant fifty serialised descriptions,
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
    os_readout: Result<TaskOsReadout, String>,
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
            os_readout: TaskOsReadout::new(runtime).map_err(|error| error.to_string()),
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
        object.insert(
            "pilot_warnings".into(),
            json!(task_pilot_warnings(task.status, &row.comments)),
        );
        self.os_readout
            .as_ref()
            .map_err(|error| OrbitError::Execution(error.clone()))?
            .insert_into(task, &mut object);
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
