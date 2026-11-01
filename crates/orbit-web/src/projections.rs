//! The `*_to_json` projections the dashboard API serves. A task detail builds
//! on the show record every read surface shares
//! (`orbit_core::application::task::task_show_json`) and adds only the
//! dashboard's own extras, through the shared core enrichment. A mutation
//! response is the core task-write projection in its dashboard full-record
//! profile, so it does not carry show-only guidance. An audit event builds on
//! the base the CLI shares (`orbit_types::telemetry::audit_event_base_json`).
//! The job-catalog projection lives in `orbit_core::application::job`; the
//! list summary and the other projections are dashboard-local. List rows and
//! the detail both carry the same `riskiness` object, estimated from one
//! cached [`TaskRiskinessModel`].

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use orbit_core::application::codebase::{TASK_RISKINESS_ESTIMATOR_VERSION, TaskRiskinessModel};

use orbit_core::application::task::{
    self, PullRequestLinks, TaskOsReadout, TaskRow, job_run_is_locally_navigable,
    link_pull_requests, mark_readiness_preparing, summary_status_transitions,
    task_detail_with_dashboard_extras, task_pilot_warnings, tasks_in_pilot_preparation,
};
use orbit_core::runtime::engine::ConfiguredCrewRegistryProjection;
use orbit_core::{AuditEvent, OrbitError, OrbitRuntime, ResolvedCrewProjection, Task, TaskStatus};
use orbit_types::identity::HumanCrew;
use orbit_types::task::task_base_json;
use orbit_types::telemetry::audit_event_base_json;
use serde_json::{Map, Value, json};

use crate::runtime_memo::{TASK_RISKINESS_STALE_LIMIT, TASK_RISKINESS_TTL};
use crate::state::DashboardState;

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

/// Stamp `human_crew` (`{name, description}`) on a task object whose `crew`
/// names a human crew, and leave it off otherwise. The owning workspace's
/// registry classifies the task here, so a client never has to guess from a
/// crew name: the aggregate view mixes workspaces that configure the same
/// name as an agent crew in one and a human crew in another, and carries no
/// crew registry of its own.
pub(crate) fn stamp_human_crew(object: &mut Map<String, Value>, human: Option<&HumanCrew>) {
    object.remove("human_crew");
    if let Some(human) = human {
        object.insert(
            "human_crew".to_string(),
            json!({ "name": human.name, "description": human.description }),
        );
    }
}

/// [`stamp_human_crew`] for a task object built outside the list projection,
/// classified by `runtime`'s registry from the object's own `crew` value.
pub(crate) fn stamp_human_crew_from_runtime(
    runtime: &OrbitRuntime,
    object: &mut Map<String, Value>,
) {
    let human = runtime.human_crew(object.get("crew").and_then(Value::as_str));
    stamp_human_crew(object, human);
}

/// The riskiness model cached for `runtime`. The first read for a runtime
/// waits for one scan; later reads are served from the cache, stale while one
/// background refresh replaces it. A failed scan is logged and leaves the
/// estimate unavailable rather than failing the task read.
pub(crate) async fn cached_riskiness_model(
    state: &DashboardState,
    runtime: &Arc<OrbitRuntime>,
) -> Option<Arc<TaskRiskinessModel>> {
    let scan_runtime = Arc::clone(runtime);
    match state
        .riskiness_memo()
        .get_or_refresh(
            runtime,
            (),
            TASK_RISKINESS_TTL,
            TASK_RISKINESS_STALE_LIMIT,
            move || scan_runtime.task_riskiness_model(),
        )
        .await
    {
        Ok(served) => Some(served.body),
        Err(error) => {
            tracing::warn!(%error, "task riskiness model unavailable");
            None
        }
    }
}

/// A task's `riskiness` object: the model's estimate, or `unavailable` with
/// a null probability when no model could be built.
pub(crate) fn task_riskiness_json(model: Option<&TaskRiskinessModel>, task: &Task) -> Value {
    let unavailable = || {
        json!({
            "estimator_version": TASK_RISKINESS_ESTIMATOR_VERSION,
            "state": "unavailable",
            "probability": null,
        })
    };
    let Some(model) = model else {
        return unavailable();
    };
    serde_json::to_value(model.estimate(task)).unwrap_or_else(|error| {
        tracing::warn!(task_id = %task.id, %error, "riskiness estimate not serialisable");
        unavailable()
    })
}

/// `GET /api/tasks/:id`: the show record plus the dashboard extras — linked
/// pull requests, the lifecycle transitions with their evidence requirements,
/// run navigability and `readiness.preparing` — and, for a proposed task
/// held by pilot warnings or a blocked task, `escalation_triage`: whether
/// the details panel's Triage action may submit now, and why not. A blocked
/// task, or a task whose latest delivery run is held, also carries
/// `why_stuck`. A task with a linked run carries `job_run_worktree`: the
/// run's recorded branch and worktree path on its execution machine. A task
/// that was piloted, reviewed or recovered carries `handled_by`: those hands
/// in time order, each derived from a durable record. Every task carries
/// `riskiness`, as its list row does, and an active triage/recovery binding
/// carries `active_triage` on both surfaces. A task assigned to a human crew
/// carries `human_crew` (`{name, description}`) on both surfaces and on every
/// dashboard write response.
pub(crate) fn task_detail_json(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
    riskiness: Option<&TaskRiskinessModel>,
) -> Result<Value, OrbitError> {
    let mut value = task_detail_with_dashboard_extras(runtime, row, status_by_id)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "riskiness".to_string(),
            task_riskiness_json(riskiness, &row.task),
        );
        stamp_human_crew_from_runtime(runtime, object);
        if let Some(triage) = runtime.escalation_triage_eligibility_json(row)? {
            object.insert("escalation_triage".to_string(), triage);
        }
        // Detail-only: the stage reads the run and its audit trail, which
        // list rows never pay for.
        if let Some(stage) = runtime.job_run_stage(row)? {
            object.insert("job_run_stage".to_string(), stage);
        }
        if let Some(worktree) = runtime.job_run_worktree(row)? {
            object.insert("job_run_worktree".to_string(), worktree);
        }
        // Detail-only: the piloted, reviewed and recovered hands read pilot
        // runs, the review ledger and recovery invocations.
        let hands = runtime.task_handled_by(row);
        if !hands.is_empty() {
            object.insert("handled_by".to_string(), Value::Array(hands));
        }
        crate::why_stuck::attach_why_stuck(runtime, row, object)?;
    }
    Ok(value)
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
    delivery_attention: BTreeMap<String, Value>,
    /// Read once, on the page's first proposed or backlog row.
    preparing: OnceCell<BTreeSet<String>>,
    active_triage: OnceCell<Result<BTreeMap<String, Value>, String>>,
    /// Reads the origin remote once, on the page's first unlinked PR ref.
    pull_request_links: PullRequestLinks,
    /// The page's riskiness model; `None` when it is unavailable.
    riskiness: Option<Arc<TaskRiskinessModel>>,
}

impl TaskListProjection {
    pub(crate) fn new(runtime: &OrbitRuntime) -> Self {
        Self {
            registry: runtime.configured_crew_registry_projection(),
            os_readout: TaskOsReadout::new(runtime).map_err(|error| error.to_string()),
            local_machine_id: runtime.automation_machine_identity().map(str::to_string),
            delivery_attention: BTreeMap::new(),
            preparing: OnceCell::new(),
            active_triage: OnceCell::new(),
            pull_request_links: PullRequestLinks::new(runtime),
            riskiness: None,
        }
    }

    pub(crate) fn with_riskiness(mut self, model: Option<Arc<TaskRiskinessModel>>) -> Self {
        self.riskiness = model;
        self
    }

    pub(crate) fn with_delivery_attention(mut self, map: BTreeMap<String, Value>) -> Self {
        self.delivery_attention = map;
        self
    }

    /// `runtime` is the one this projection was built from; it is read only
    /// for pilot preparation and active triage/recovery bindings.
    pub(crate) fn row_to_json(
        &self,
        runtime: &OrbitRuntime,
        row: &TaskRow,
        status_by_id: &BTreeMap<String, TaskStatus>,
    ) -> Result<Value, OrbitError> {
        let task = &row.task;
        let mut object = task_to_json(task, status_by_id, &self.pull_request_links);
        object.insert(
            "not_before".into(),
            json!(task.not_before.map(orbit_types::task::format_not_before)),
        );
        if let Some(at) = task.not_before {
            object.insert(
                "scheduled_hold".into(),
                json!(
                    task.status == TaskStatus::Backlog
                        && at > chrono::Utc::now()
                        && !orbit_types::task::schedule_overridden(&row.history, at)
                ),
            );
        }
        object.insert(
            "pilot_warnings".into(),
            json!(task_pilot_warnings(
                task.status,
                &row.comments,
                &task.dependencies(),
                &|id: &str| matches!(
                    runtime.get_task(id),
                    Ok(found) if found.status == TaskStatus::Done
                ),
            )),
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
        let human = explicit_task_crew(task).and_then(|name| {
            self.registry
                .human_crews
                .iter()
                .find(|human| human.name == name)
        });
        stamp_human_crew(&mut object, human);
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
        if let Some(attention) = self.delivery_attention.get(&task.id) {
            object.insert("delivery_attention".to_string(), attention.clone());
        }
        let triage = self
            .active_triage
            .get_or_init(|| {
                task::active_task_triage_map(runtime).map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| OrbitError::Execution(error.clone()))?;
        if let Some(triage) = triage.get(&task.id) {
            object.insert("active_triage".to_string(), triage.clone());
        }
        object.insert(
            "riskiness".to_string(),
            task_riskiness_json(self.riskiness.as_deref(), task),
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
