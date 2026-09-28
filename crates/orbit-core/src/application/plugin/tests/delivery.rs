//! A plugin-contributed delivery job selected by a task's `delivery:<job>` tag
//! ships through the same gate, in-flight guard and drain as the shipped
//! pipelines (design §4.5).
//!
//! The gate and the auto pipeline run for real in-process. Only
//! `invoke_and_wait` is scripted: it executes the named child job in the same
//! process instead of spawning a detached worker, so every reservation, lock
//! and resolution step between them is the production one.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{
    DispatchError, ResolvedCliExecutor, RuntimeHost, V2AuditWriter, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::task::{TaskComplexity, TaskStatus, TaskType};
use orbit_types::workflow::{CompletionPolicy, JobRunTrigger, ShipMode};
use serde_json::{Value, json};

use super::super::{PluginAddOptions, disable_plugin, disable_plugin_in_workspace, install_plugin};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;
use crate::OrbitRuntime;
use crate::application::SYSTEM_AUDIT_IDENTITY;
use crate::application::job::{JobRunListParams, seed_default_jobs};
use crate::application::task::TaskAddParams;
use crate::bootstrap::activity::seed_default_activities;
use crate::runtime::task::locks::{workspace_orbit_dir, workspace_task_reservation_id};

const NAMESPACE: &str = "research";
const DELIVERY_JOB: &str = "research_investigation";

/// Install a plugin whose one job declares itself a local delivery job, and
/// seed the shipped catalog the gate and auto pipeline resolve from.
fn delivery_fixture() -> (PluginFixture, OrbitRuntime) {
    let fixture = PluginFixture::new();
    seed_default_activities(&fixture.global_root.join("resources/activities"), true)
        .expect("seed shipped activities");
    seed_default_jobs(&fixture.global_root.join("resources/jobs"), true)
        .expect("seed shipped jobs");
    std::fs::write(fixture.repo_root.join("README.md"), "fixture\n").expect("write context");

    // The fixture's routine targets `plugin.job`, so the delivery job keeps
    // that name rather than orphaning the routine.
    let mut plugin = DefinitionPlugin::new(NAMESPACE);
    plugin.job = DELIVERY_JOB.to_string();
    let source = plugin.write(&fixture);
    std::fs::write(
        source.join("definitions/jobs/pipeline.yaml"),
        format!(
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {DELIVERY_JOB}\nspec:\n  \
             state: enabled\n  task_delivery:\n    modes: [local]\n  steps:\n    \
             - id: investigate\n      target: activity:sleep\n      default_input:\n        \
             seconds: 0\n"
        ),
    )
    .expect("write plugin delivery job");
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 plugin source"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install delivery plugin");
    let runtime = fixture.reopen();
    (fixture, runtime)
}

fn add_task(runtime: &OrbitRuntime, tags: &[&str]) -> String {
    runtime
        .add_task(TaskAddParams {
            title: "Investigate the question".to_string(),
            description: "Fixture research task".to_string(),
            acceptance_criteria: vec!["A research record exists".to_string()],
            plan: "Investigate".to_string(),
            context_files: vec!["README.md".to_string()],
            tags: tags.iter().map(ToString::to_string).collect(),
            complexity: TaskComplexity::Low,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add task")
        .id
}

fn selecting_task(runtime: &OrbitRuntime) -> String {
    add_task(runtime, &[&format!("delivery:{DELIVERY_JOB}")])
}

fn ship(runtime: &OrbitRuntime, task_id: &str, mode: ShipMode) -> Result<String, OrbitError> {
    runtime
        .submit_ship_run(
            mode,
            Some("main"),
            &[task_id.to_string()],
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
            JobRunTrigger::cli(),
        )
        .map(|submitted| submitted.run_id)
}

fn persisted_run_ids(runtime: &OrbitRuntime) -> Vec<String> {
    runtime
        .list_job_runs(JobRunListParams::default())
        .expect("list job runs")
        .into_iter()
        .map(|run| run.run_id)
        .collect()
}

/// Every child dispatch the pipelines under test asked for.
#[derive(Debug, Clone)]
struct Dispatch {
    job_name: String,
    run_input: Value,
    succeeded: bool,
}

/// Executes each `invoke_and_wait` child in-process. The shipped leaves are
/// recorded without being run: their own behavior is covered elsewhere, and
/// what these tests observe is which job the gate chose.
struct InProcessChildren<'a> {
    runtime: &'a OrbitRuntime,
    repo_root: &'a Path,
    dispatches: Mutex<Vec<Dispatch>>,
    reservations: Mutex<Vec<Value>>,
}

impl<'a> InProcessChildren<'a> {
    fn new(runtime: &'a OrbitRuntime, repo_root: &'a Path) -> Self {
        Self {
            runtime,
            repo_root,
            dispatches: Mutex::new(Vec::new()),
            reservations: Mutex::new(Vec::new()),
        }
    }

    fn execute(&self, job_name: &str, input: Value, run_id: &str) -> Result<bool, DispatchError> {
        let (_path, mut job) = self
            .runtime
            .load_v2_job_asset_by_name(job_name)
            .map_err(|error| DispatchError::JobExecution(format!("load {job_name}: {error}")))?;
        let catalog = self
            .runtime
            .v2_activity_catalog()
            .map_err(|error| DispatchError::JobExecution(error.to_string()))?;
        resolve_job_catalog_refs_for_execution(&mut job, &catalog)
            .map_err(|error| DispatchError::JobExecution(error.to_string()))?;
        let writer = V2AuditWriter::with_disk_sinks(
            &self.runtime.paths().audit_dir,
            self.runtime
                .v2_audit_store()
                .map_err(|error| DispatchError::JobExecution(error.to_string()))?,
            self.runtime
                .workspace_id()
                .map_err(|error| DispatchError::JobExecution(error.to_string()))?,
            run_id,
            SYSTEM_AUDIT_IDENTITY,
            Some(self.repo_root),
        )
        .map_err(|error| DispatchError::JobExecution(error.to_string()))?;
        Ok(execute_job_with_resume(&job, input, run_id, writer, self, None)?.success)
    }

    fn run_child(&self, input: &Value) -> Result<Value, DispatchError> {
        let job_name = input["job_name"].as_str().unwrap_or_default().to_string();
        let run_input = input["run_input"].clone();
        let run_id = format!("jrun-delivery-child-{}", self.dispatches().len());
        let succeeded = if matches!(
            job_name.as_str(),
            "task_local_pipeline" | "task_pr_pipeline"
        ) {
            true
        } else {
            self.execute(&job_name, run_input.clone(), &run_id)?
        };
        self.dispatches
            .lock()
            .expect("dispatch log")
            .push(Dispatch {
                job_name,
                run_input,
                succeeded,
            });
        Ok(json!({
            "run_id": run_id,
            "status": if succeeded { "success" } else { "failed" },
        }))
    }

    fn dispatches(&self) -> Vec<Dispatch> {
        self.dispatches.lock().expect("dispatch log").clone()
    }

    fn reservations(&self) -> Vec<Value> {
        self.reservations.lock().expect("reservation log").clone()
    }
}

impl RuntimeHost for InProcessChildren<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        if action == "invoke_and_wait" {
            return self.run_child(input);
        }
        let output = <OrbitRuntime as RuntimeHost>::run_deterministic(
            self.runtime,
            action,
            config,
            input,
            tool_context,
        )?;
        if action == "reserve_locks" {
            self.reservations
                .lock()
                .expect("reservation log")
                .push(output.clone());
        }
        Ok(output)
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Err(DispatchError::JobExecution(format!(
            "fixture refuses to launch provider '{provider}'"
        )))
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            self.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

fn gate_input(task_id: &str) -> Value {
    json!({
        "task_ids": [task_id],
        "mode": "local",
        "base_branch": "main",
        "base_sync": "local",
        "poll_interval_seconds": 1,
    })
}

#[test]
fn a_task_selecting_a_plugin_delivery_job_ships_through_the_gate_to_that_job() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_task_selecting_a_plugin_delivery_job_ships_through_the_gate_to_that_job",
    ) {
        return;
    }
    let (fixture, runtime) = delivery_fixture();
    let task_id = selecting_task(&runtime);
    let host = InProcessChildren::new(&runtime, &fixture.repo_root);

    let succeeded = host
        .execute(
            "task_gate_pipeline",
            gate_input(&task_id),
            "jrun-delivery-gate",
        )
        .expect("execute gate");

    assert!(succeeded, "the gate delivers through the plugin job");
    let reservations = host.reservations();
    assert_eq!(
        reservations.last().map(|output| &output["reserved"]),
        Some(&json!(true)),
        "the gate reserved the task's context before dispatching: {reservations:?}"
    );
    let dispatches = host.dispatches();
    assert_eq!(dispatches.len(), 1, "one child: {dispatches:?}");
    assert_eq!(dispatches[0].job_name, DELIVERY_JOB);
    assert!(dispatches[0].succeeded, "the plugin job itself ran");
    assert_eq!(dispatches[0].run_input["task_ids"], json!([task_id]));
    assert_eq!(
        active_reservation_count(&runtime),
        0,
        "the gate released its reservation after the child finished"
    );
}

#[test]
fn a_task_without_a_selection_still_ships_through_the_default_pipeline() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_task_without_a_selection_still_ships_through_the_default_pipeline",
    ) {
        return;
    }
    let (fixture, runtime) = delivery_fixture();
    let task_id = add_task(&runtime, &[]);
    let host = InProcessChildren::new(&runtime, &fixture.repo_root);

    assert!(
        host.execute(
            "task_gate_pipeline",
            gate_input(&task_id),
            "jrun-default-gate"
        )
        .expect("execute gate")
    );
    let dispatches = host.dispatches();
    assert_eq!(dispatches.len(), 1, "one child: {dispatches:?}");
    assert_eq!(dispatches[0].job_name, "task_local_pipeline");
}

/// The drain dispatches `task_auto_pipeline` leaves; one that discovers a
/// selecting task hands it to the plugin job through the gate.
#[test]
fn the_auto_pipeline_dispatches_a_selecting_task_to_its_plugin_job() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "the_auto_pipeline_dispatches_a_selecting_task_to_its_plugin_job",
    ) {
        return;
    }
    let (fixture, runtime) = delivery_fixture();
    let task_id = selecting_task(&runtime);
    let host = InProcessChildren::new(&runtime, &fixture.repo_root);

    assert!(
        host.execute(
            "task_auto_pipeline",
            json!({ "mode": "local", "base_branch": "main", "base_sync": "local" }),
            "jrun-delivery-auto",
        )
        .expect("execute auto pipeline")
    );
    let jobs = host
        .dispatches()
        .into_iter()
        .map(|dispatch| (dispatch.job_name, dispatch.run_input["task_ids"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        jobs,
        vec![
            (DELIVERY_JOB.to_string(), json!([task_id])),
            ("task_gate_pipeline".to_string(), json!([task_id])),
        ],
        "the plugin job runs inside the gate the auto pipeline dispatched"
    );
}

#[test]
fn a_second_ship_is_refused_while_the_plugin_delivery_job_is_live() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_second_ship_is_refused_while_the_plugin_delivery_job_is_live",
    ) {
        return;
    }
    let (_fixture, runtime) = delivery_fixture();
    let task_id = selecting_task(&runtime);
    let live = runtime
        .stores()
        .jobs()
        .insert_job_run(
            DELIVERY_JOB,
            1,
            Utc::now(),
            Some(json!({ "task_ids": [task_id] })),
            None,
        )
        .expect("insert live plugin delivery run");
    assert!(!live.state.is_terminal());

    let error = ship(&runtime, &task_id, ShipMode::Local)
        .expect_err("the live plugin run holds the task's delivery slot");

    assert!(
        matches!(
            &error,
            OrbitError::ShipRunInFlight { task_id: guarded, run_id }
                if *guarded == task_id && *run_id == live.run_id
        ),
        "expected the in-flight refusal naming the plugin run, got {error:?}"
    );
    assert_eq!(persisted_run_ids(&runtime), vec![live.run_id]);
}

#[test]
fn ship_refuses_a_selection_its_plugin_cannot_serve_and_names_the_plugin() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "ship_refuses_a_selection_its_plugin_cannot_serve_and_names_the_plugin",
    ) {
        return;
    }
    let (fixture, runtime) = delivery_fixture();
    let task_id = selecting_task(&runtime);

    let error = ship(&runtime, &task_id, ShipMode::Pr)
        .expect_err("a local-only delivery job cannot ship in PR mode");
    assert!(
        matches!(&error, OrbitError::InvalidInput(message)
            if message.contains(DELIVERY_JOB) && message.contains("'pr'")),
        "{error:?}"
    );

    disable_plugin_in_workspace(&runtime, NAMESPACE).expect("disable in workspace");
    let runtime = fixture.reopen();
    let error = ship(&runtime, &task_id, ShipMode::Local)
        .expect_err("a workspace-disabled plugin's job cannot deliver");
    assert!(
        matches!(&error, OrbitError::InvalidInput(message)
            if message.contains(&format!("plugin '{NAMESPACE}'"))
                && message.contains("disabled in this workspace")),
        "{error:?}"
    );

    disable_plugin(&runtime, NAMESPACE).expect("disable on host");
    let runtime = fixture.reopen();
    let error = ship(&runtime, &task_id, ShipMode::Local)
        .expect_err("a host-disabled plugin's job cannot deliver");
    assert!(
        matches!(&error, OrbitError::InvalidInput(message)
            if message.contains(&format!("plugin '{NAMESPACE}'"))
                && message.contains("disabled on this host")),
        "{error:?}"
    );

    let uninstalled = add_task(&runtime, &["delivery:nobody_ships_this"]);
    let error = ship(&runtime, &uninstalled, ShipMode::Local)
        .expect_err("a job no plugin ships cannot deliver");
    assert!(
        matches!(&error, OrbitError::InvalidInput(message)
            if message.contains("nobody_ships_this")
                && message.contains("no installed plugin ships")),
        "{error:?}"
    );
    assert!(
        persisted_run_ids(&runtime).is_empty(),
        "no refused ship may persist a run"
    );
}

/// A refused selection must not be dispatched to a gate on every drain pass:
/// the drain withholds it with the refusal, and keeps the rest of the backlog.
#[test]
fn the_drain_withholds_a_task_whose_delivery_plugin_is_disabled() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "the_drain_withholds_a_task_whose_delivery_plugin_is_disabled",
    ) {
        return;
    }
    let (fixture, runtime) = delivery_fixture();
    let selecting = selecting_task(&runtime);
    let listed = list_backlog(&runtime);
    assert_eq!(listed["task_ids"], json!([selecting]));

    disable_plugin(&runtime, NAMESPACE).expect("disable on host");
    let runtime = fixture.reopen();
    let ordinary = add_task(&runtime, &[]);
    let listed = list_backlog(&runtime);

    assert_eq!(listed["task_ids"], json!([ordinary]));
    let excluded = listed["excluded"].as_array().expect("excluded array");
    assert_eq!(excluded.len(), 1, "{excluded:?}");
    assert_eq!(excluded[0]["id"], json!(selecting));
    assert_eq!(excluded[0]["reason"], json!("delivery_job_unavailable"));
    assert!(
        excluded[0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(&format!("plugin '{NAMESPACE}'"))),
        "{excluded:?}"
    );
}

fn active_reservation_count(runtime: &OrbitRuntime) -> usize {
    runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(
            &workspace_orbit_dir(runtime),
            workspace_task_reservation_id(runtime)
                .expect("workspace reservation id")
                .as_deref(),
        )
        .expect("list active reservations")
        .reservations
        .len()
}

fn list_backlog(runtime: &OrbitRuntime) -> Value {
    <OrbitRuntime as RuntimeHost>::run_deterministic(
        runtime,
        "list_backlog_tasks",
        &json!({}),
        &json!({ "mode": "local" }),
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(runtime, None, None, None, None),
    )
    .expect("list backlog")
}
