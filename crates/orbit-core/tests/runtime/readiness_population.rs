//! Readiness and the drain's backlog snapshot select open tasks from the
//! generated index, so closed history costs them no envelope reads, while
//! the closed tasks their rules still reach stay visible: an epic root's
//! closed descendants, and a closed task an explicit request names.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::workflow::{DrainAdmissionForecast, JobRunState, PipelineState};
use serde_json::{Value, json};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use super::dispatch_admission::PressureProbe;

/// Closed tasks in the workspace: far more than a bounded selection reads.
const CLOSED: usize = 300;

/// Counts `task.yaml` parses: the store opens one span per envelope read.
struct EnvelopeReads(Arc<AtomicU64>);

impl<S: tracing::Subscriber> Layer<S> for EnvelopeReads {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: Context<'_, S>,
    ) {
        if attrs.metadata().name() == "task_envelope_read" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn add(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    parent: Option<&str>,
    tags: &[&str],
    context_files: &[&str],
) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The task is observable.".into()],
            plan: "Fixture plan.".into(),
            complexity: TaskComplexity::Medium,
            parent_id: parent.map(|id| id.parse().unwrap()),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            context_files: context_files
                .iter()
                .map(|file| (*file).to_string())
                .collect(),
            task_type: Some(orbit_core::TaskType::Chore),
            status: Some(status),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

fn readiness(runtime: &OrbitRuntime, task_ids: &[String]) -> Result<Value, orbit_core::OrbitError> {
    runtime.workspace_auto_readiness(task_ids, None, 50, &[])
}

fn entry<'a>(readiness: &'a Value, task: &str) -> &'a Value {
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap_or_else(|| panic!("{task} missing from readiness: {readiness}"))
}

fn readiness_reads_open_work_and_the_closed_tasks_its_rules_reach() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    for index in 0..CLOSED {
        add(
            &runtime,
            &format!("closed {index}"),
            TaskStatus::Done,
            None,
            &[],
            &["file:README.md"],
        );
    }
    let ready = add(
        &runtime,
        "ready",
        TaskStatus::Backlog,
        None,
        &[],
        &["file:src/ready.rs"],
    );
    // An epic root that declared nothing while its (closed) child did: it
    // inherited its whole footprint, so admission withholds it.
    let root_task = add(
        &runtime,
        "epic root",
        TaskStatus::Backlog,
        None,
        &["epic"],
        &[],
    );
    add(
        &runtime,
        "closed child",
        TaskStatus::Done,
        Some(&root_task),
        &[],
        &["file:src/child.rs"],
    );
    let closed = add(
        &runtime,
        "closed explicit",
        TaskStatus::Done,
        None,
        &[],
        &["file:README.md"],
    );
    // Two backlog siblings under a closed parent form one group: a lock
    // conflict on one holds the other, which only the parent links.
    add(
        &runtime,
        "holder",
        TaskStatus::InProgress,
        None,
        &[],
        &["file:src/held.rs"],
    );
    let group = add(
        &runtime,
        "closed parent",
        TaskStatus::Done,
        None,
        &[],
        &["file:README.md"],
    );
    let blocked = add(
        &runtime,
        "blocked",
        TaskStatus::Backlog,
        Some(&group),
        &[],
        &["file:src/held.rs"],
    );
    let sibling = add(
        &runtime,
        "sibling",
        TaskStatus::Backlog,
        Some(&group),
        &[],
        &["file:src/sibling.rs"],
    );
    // One listing records the envelope stamps a later process accepts.
    runtime.list_tasks().unwrap();
    drop(runtime);

    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .unwrap()
        .with_host_resource_probe(PressureProbe::calm());
    let reads = Arc::new(AtomicU64::new(0));
    let subscriber = tracing_subscriber::registry().with(EnvelopeReads(Arc::clone(&reads)));
    let report =
        tracing::subscriber::with_default(subscriber, || readiness(&runtime, &[]).unwrap());
    let reads = reads.load(Ordering::Relaxed);
    assert!(
        reads <= 100,
        "readiness over {CLOSED} closed tasks must read only open work and what its rules reach: read {reads}"
    );
    assert_eq!(report["total"], 4, "{report}");
    assert_eq!(
        entry(&report, &blocked)["reason"],
        "context_lock_conflict",
        "{report}"
    );
    assert_eq!(
        entry(&report, &sibling)["reason"],
        "group_member_conflict",
        "{report}"
    );
    assert_eq!(entry(&report, &ready)["eligible"], true, "{report}");
    let withheld = entry(&report, &root_task);
    assert_eq!(withheld["eligible"], false, "{report}");
    assert_eq!(withheld["reason"], "inherited_only_epic_root", "{report}");

    let backlog = runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert!(
        backlog["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == root_task.as_str()
                && entry["reason"] == "inherited_only_epic_root"),
        "{backlog}"
    );

    let explicit = readiness(&runtime, std::slice::from_ref(&closed)).unwrap();
    let named = entry(&explicit, &closed);
    assert_eq!(named["status"], "done", "{explicit}");
    assert_eq!(named["reason"], "not_backlog", "{explicit}");
    for unknown in ["ORB-999999", "not-a-task"] {
        let error = readiness(&runtime, &[unknown.to_string()]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("was not found in this workspace"),
            "{unknown}: {error}"
        );
    }
}

fn restored_closed_child_retains_inherited_epic_footprint() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let previous = add(
        &runtime,
        "previous parent",
        TaskStatus::Done,
        None,
        &[],
        &[],
    );
    let epic = add(&runtime, "epic", TaskStatus::Backlog, None, &["epic"], &[]);
    let child = add(
        &runtime,
        "closed descendant",
        TaskStatus::Done,
        Some(&previous),
        &[],
        &["file:src/child.rs"],
    );
    runtime.list_tasks().unwrap();
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&global),
    )
    .unwrap();
    let path = registry
        .canonical_task_bundle_path(&runtime.workspace_id().unwrap(), &child)
        .unwrap()
        .join(orbit_types::task::TASK_ENVELOPE_FILE_NAME);
    let mut envelope: orbit_types::task::TaskEnvelopeV2 =
        serde_yaml::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for relation in &mut envelope.relations {
        if relation.relation_type == orbit_types::task::TaskRelationType::ChildOf {
            relation.target = epic.clone();
        }
    }
    let restored = path.with_extension("restored");
    std::fs::write(&restored, serde_yaml::to_string(&envelope).unwrap()).unwrap();
    std::fs::rename(restored, path).unwrap();
    drop(runtime);

    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .unwrap()
        .with_host_resource_probe(PressureProbe::calm());
    let report = readiness(&runtime, std::slice::from_ref(&epic)).unwrap();
    assert_eq!(entry(&report, &epic)["eligible"], false, "{report}");
    assert_eq!(
        entry(&report, &epic)["reason"],
        "inherited_only_epic_root",
        "{report}"
    );
    let backlog = runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert!(
        backlog["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == epic && entry["reason"] == "inherited_only_epic_root"),
        "{backlog}"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    readiness_reads_open_work_and_the_closed_tasks_its_rules_reach_scenarios,
    readiness_reads_open_work_and_the_closed_tasks_its_rules_reach,
    restored_closed_child_retains_inherited_epic_footprint
);

/// A finished drain's retained forecast still names a done task. The admission
/// population omits it, so its title is absent and its status comes from the
/// registry projection (ORB-18957).
fn a_retained_forecast_reports_the_current_status_of_a_done_task() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .unwrap()
        .with_host_resource_probe(PressureProbe::calm());
    let done = add(
        &runtime,
        "finished candidate",
        TaskStatus::Done,
        None,
        &[],
        &["file:README.md"],
    );
    let held = add(
        &runtime,
        "waiting candidate",
        TaskStatus::Backlog,
        None,
        &[],
        &["file:src/held.rs"],
    );
    let now = Utc::now();
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run("workspace_auto_pipeline", 1, now, Some(json!({})), None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, now, std::process::id())
        .unwrap();
    let forecast: DrainAdmissionForecast = serde_json::from_value(json!({
        "mode": "enforce",
        "forecast_at": "2026-10-27T03:59:00Z",
        "admitted": [
            { "task_id": done, "profile_source": "exact" },
            { "task_id": "ORB-UNKNOWN", "profile_source": "none" }
        ],
        "skipped": [{
            "task_id": held,
            "profile_source": "exact",
            "resource": "cpu",
            "minute": 6,
            "percent": 94.0,
            "detail": "cpu 94% at +6m"
        }]
    }))
    .unwrap();
    let mut state = PipelineState::new(
        run.run_id.clone(),
        "workspace_auto_pipeline".into(),
        json!({}),
    );
    state.pull_drain.drain_last_forecast = Some(forecast);
    runtime.write_run_state(&run.run_id, &state).unwrap();
    jobs.finalize_job_run(&run.run_id, JobRunState::Success, now, Some(0))
        .unwrap();

    let report = readiness(&runtime, &[]).unwrap();
    let last = &report["capacity"]["last_admission_forecast"];
    assert_eq!(
        last["drain_live"], false,
        "a finished drain's forecast is not a live window (ORB-18957): {report}"
    );
    assert!(
        report["capacity"].get("admission_forecast").is_none(),
        "a pass that built none has no current forecast (ORB-18957): {report}"
    );
    assert_eq!(
        last["task_statuses"][done.as_str()],
        "done",
        "a done task the forecast still names keeps its current status (ORB-18957): {last}"
    );
    assert_eq!(
        last["task_statuses"][held.as_str()],
        "backlog",
        "a held backlog task reports backlog (ORB-18957): {last}"
    );
    assert!(
        last["task_statuses"].get("ORB-UNKNOWN").is_none(),
        "an unknown forecast id gets no invented status (ORB-18957): {last}"
    );
    assert!(
        last["task_titles"].get(done.as_str()).is_none(),
        "a done task stays out of the admission-population titles (ORB-18957): {last}"
    );
    assert_eq!(
        last["task_titles"][held.as_str()],
        "waiting candidate",
        "a backlog task still carries its title (ORB-18957): {last}"
    );
}

/// A finished drain keeps its built final pass under the current-pass key as
/// well as the retained-forecast key. Readiness must expose both as the same
/// timestamped history and retain the current status of a done task.
fn a_terminal_built_forecast_is_projected_as_current_history() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .unwrap()
        .with_host_resource_probe(PressureProbe::calm());
    let done = add(
        &runtime,
        "finished candidate",
        TaskStatus::Done,
        None,
        &[],
        &["file:README.md"],
    );
    let now = Utc::now();
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run("workspace_auto_pipeline", 1, now, Some(json!({})), None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, now, std::process::id())
        .unwrap();
    let forecast: DrainAdmissionForecast = serde_json::from_value(json!({
        "mode": "enforce",
        "forecast_at": "2026-10-27T03:59:00Z",
        "admitted": [{ "task_id": done, "profile_source": "exact" }]
    }))
    .unwrap();
    let mut state = PipelineState::new(
        run.run_id.clone(),
        "workspace_auto_pipeline".into(),
        json!({}),
    );
    state.pull_drain.record_admission_pass(
        serde_json::from_value(json!({
            "recorded_at": "2026-10-27T03:59:00Z",
            "queued": 0,
            "admission_forecast": forecast
        }))
        .unwrap(),
    );
    runtime.write_run_state(&run.run_id, &state).unwrap();
    jobs.finalize_job_run(&run.run_id, JobRunState::Success, now, Some(0))
        .unwrap();

    let report = readiness(&runtime, &[]).unwrap();
    let current = &report["capacity"]["admission_forecast"];
    assert_eq!(
        current["drain_live"], false,
        "a finished drain's final pass is historical (ORB-19041): {report}"
    );
    assert_eq!(
        current["forecast_at"], "2026-10-27T03:59:00Z",
        "a finished drain's built forecast keeps its timestamp (ORB-19041): {current}"
    );
    assert_eq!(
        current["task_statuses"][done.as_str()],
        "done",
        "a terminal task keeps its current status on the built final pass (ORB-19041): {current}"
    );
    assert_eq!(
        report["capacity"]["last_admission_forecast"], *current,
        "the current final pass and retained forecast are the same observation (ORB-19041): {report}"
    );
}

crate::scenario_table!(
    retained_forecast_names_current_task_status_scenarios,
    a_retained_forecast_reports_the_current_status_of_a_done_task,
    a_terminal_built_forecast_is_projected_as_current_history,
);
