use std::fs;

use chrono::{DateTime, Duration, Utc};
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use orbit_types::workflow::{PipelineState, RunRootCause};
use orbit_types::workspace::WorkspaceCheckout;
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const OTHER: &str = "ws_http_reliability";

fn add_workspace(fixture: &Fixture) -> OrbitRuntime {
    let path = orbit_registry::workspace_registry::registry_path_for(&fixture.global);
    let mut registry = orbit_registry::workspace_registry::load_registry_from(&path).unwrap();
    let mut workspace = registry.workspaces[0].clone();
    workspace.id = OTHER.into();
    workspace.name = "reliability".into();
    let repo = fixture.path("reliability");
    let work = repo.join(".orbit");
    fs::create_dir_all(&work).unwrap();
    fs::write(
        work.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: {OTHER}\n"),
    )
    .unwrap();
    let checkout = WorkspaceCheckout::owner(OTHER.into(), repo, work);
    registry.workspaces.push(workspace.clone());
    registry.checkouts.push(checkout.clone());
    orbit_registry::workspace_registry::save_registry_to(&registry, &path).unwrap();
    orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
        &fixture.global,
        &workspace,
        &checkout,
    )
    .unwrap()
}

fn seed_run(runtime: &OrbitRuntime, id: &str, state: JobRunState) {
    seed_job_run(runtime, id, "reliability_fixture", state);
}

fn seed_job_run(runtime: &OrbitRuntime, id: &str, job_id: &str, state: JobRunState) {
    let now = Utc::now();
    let run = JobRun {
        run_id: id.into(),
        job_id: job_id.into(),
        attempt: 1,
        state,
        scheduled_at: now,
        created_at: now,
        started_at: (state != JobRunState::Pending).then_some(now),
        finished_at: state.is_terminal().then_some(now),
        duration_ms: None,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
        executed_on: None,
    };
    let workspace_id = runtime.workspace_id().unwrap();
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&workspace_id, &run, None)
        .unwrap();
}

fn set_created_at(runtime: &OrbitRuntime, id: &str, at: DateTime<Utc>) {
    let workspace_id = runtime.workspace_id().unwrap();
    runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET created_at = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
                    (at.to_rfc3339(), workspace_id.as_str(), id),
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))
        })
        .unwrap();
}

/// Records the leaf a wrapper failed because of, the same `root_cause` a
/// `pipeline_success_guard` parent stores.
fn record_echo(runtime: &OrbitRuntime, id: &str, job_id: &str, leaf_run_id: &str) {
    let mut state = PipelineState::new(id.to_string(), job_id.to_string(), json!({}));
    state.root_cause = Some(RunRootCause {
        leaf_run_id: leaf_run_id.to_string(),
        task_id: Some("ORB-16400".into()),
        step: Some("implement".into()),
        code: Some("validation_failed".into()),
    });
    runtime.write_run_state(id, &state).unwrap();
}

fn set_unknown_state(runtime: &OrbitRuntime, id: &str) {
    let workspace_id = runtime.workspace_id().unwrap();
    runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET state = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
                    ("future_state", workspace_id.as_str(), id),
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))
        })
        .unwrap();
}

fn count(value: &Value, field: &str) -> u64 {
    value[field]
        .as_u64()
        .unwrap_or_else(|| panic!("missing count {field}: {value}"))
}

#[test]
fn fleet_reliability_totals_reconcile_mixed_outcomes_over_http() {
    isolated(
        "reliability::fleet_reliability_totals_reconcile_mixed_outcomes_over_http",
        || {
            let fixture = Fixture::new();
            let other = add_workspace(&fixture);

            for (id, state) in [
                ("success", JobRunState::Success),
                ("failed", JobRunState::Failed),
                ("cancelled", JobRunState::Cancelled),
                ("held", JobRunState::Held),
                ("pending", JobRunState::Pending),
                ("unknown", JobRunState::Pending),
            ] {
                seed_run(&fixture.runtime, id, state);
            }
            set_unknown_state(&fixture.runtime, "unknown");

            seed_run(&other, "skipped", JobRunState::Skipped);
            seed_run(&other, "running", JobRunState::Running);
            seed_run(&other, "held", JobRunState::Held);

            let server = fixture.server(false);
            let response = json_ok(server.get("/api/metrics/reliability?window=24h"));
            let workspaces = response["workspaces"].as_array().unwrap();
            assert_eq!(workspaces.len(), 2, "both active workspaces are included");

            let active_held = workspaces
                .iter()
                .map(|workspace| count(&workspace["job_runs"]["counts"], "held"))
                .sum::<u64>();
            let totals = &response["totals"]["job_runs"]["counts"];
            assert_eq!(count(totals, "held"), active_held);
            assert_eq!(
                totals,
                &json!({
                    "total": 9,
                    "succeeded": 1,
                    "failed": 1,
                    "cascaded": 0,
                    "cancelled": 1,
                    "skipped": 1,
                    "held": 2,
                    "in_flight": 2,
                    "unknown": 1,
                })
            );
            let bucket_sum = [
                "succeeded",
                "failed",
                "cascaded",
                "cancelled",
                "skipped",
                "held",
                "in_flight",
                "unknown",
            ]
            .iter()
            .map(|field| count(totals, field))
            .sum::<u64>();
            assert_eq!(count(totals, "total"), bucket_sum);
        },
    );
}

/// A failed leaf plus the gate and auto wrappers that only echo it is one
/// failure, attributed to the leaf. A wrapper whose leaf is not in this store
/// still counts once, on that wrapper, rather than disappearing.
#[test]
fn fleet_reliability_counts_a_delivery_and_its_wrapper_echoes_once() {
    isolated(
        "reliability::fleet_reliability_counts_a_delivery_and_its_wrapper_echoes_once",
        || {
            let fixture = Fixture::new();
            let runtime = &fixture.runtime;
            let leaf_at = Utc::now() - Duration::hours(5);
            let echo_at = Utc::now() - Duration::minutes(10);

            seed_job_run(runtime, "leaf", "task_pr_pipeline", JobRunState::Failed);
            seed_job_run(runtime, "gate", "task_gate_pipeline", JobRunState::Failed);
            seed_job_run(runtime, "auto", "task_auto_pipeline", JobRunState::Failed);
            seed_job_run(
                runtime,
                "absent-leaf",
                "task_local_pipeline",
                JobRunState::Failed,
            );
            seed_job_run(runtime, "ok", "task_pr_pipeline", JobRunState::Success);
            set_created_at(runtime, "leaf", leaf_at);
            set_created_at(runtime, "ok", leaf_at);
            for id in ["gate", "auto", "absent-leaf"] {
                set_created_at(runtime, id, echo_at);
            }
            record_echo(runtime, "gate", "task_gate_pipeline", "leaf");
            record_echo(runtime, "auto", "task_auto_pipeline", "leaf");
            record_echo(
                runtime,
                "absent-leaf",
                "task_local_pipeline",
                "missing-leaf",
            );

            let server = fixture.server(false);
            let response = json_ok(server.get("/api/metrics/reliability?window=24h"));
            let totals = &response["totals"]["job_runs"]["counts"];
            assert_eq!(
                totals,
                &json!({
                    "total": 5,
                    "succeeded": 1,
                    "failed": 2,
                    "cascaded": 2,
                    "cancelled": 0,
                    "skipped": 0,
                    "held": 0,
                    "in_flight": 0,
                    "unknown": 0,
                })
            );
            let rate = &response["totals"]["job_runs"]["failure_rate"];
            assert_eq!(rate["numerator"], 2);
            assert_eq!(rate["denominator"], 3);
            let label = rate["denominator_label"].as_str().unwrap();
            assert!(
                label.contains("wrapper echoes rolled into the leaf"),
                "denominator must state the roll-up rule: {label}"
            );

            let workspace = response["workspaces"]
                .as_array()
                .unwrap()
                .iter()
                .find(|workspace| {
                    workspace["job_runs"]["by_job"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|job| job["job_id"] == "task_pr_pipeline")
                })
                .expect("the seeded workspace");
            let job = |id: &str| {
                workspace["job_runs"]["by_job"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["job_id"] == id)
                    .unwrap_or_else(|| panic!("missing {id}"))
            };
            let pr = job("task_pr_pipeline");
            let gate = job("task_gate_pipeline");
            let auto = job("task_auto_pipeline");
            let absent = job("task_local_pipeline");
            assert_eq!(
                count(&pr["counts"], "failed"),
                1,
                "the leaf owns the delivery"
            );
            assert_eq!(count(&pr["counts"], "succeeded"), 1);
            assert_eq!(count(&gate["counts"], "failed"), 0);
            assert_eq!(count(&gate["counts"], "cascaded"), 1);
            assert_eq!(count(&auto["counts"], "failed"), 0);
            assert_eq!(count(&auto["counts"], "cascaded"), 1);
            assert_eq!(
                count(&pr["counts"], "failed")
                    + count(&gate["counts"], "failed")
                    + count(&auto["counts"], "failed"),
                1,
                "the leaf and its two wrappers are one failure"
            );
            assert_eq!(
                count(&absent["counts"], "failed"),
                1,
                "a wrapper whose leaf is absent still counts once"
            );
            assert_eq!(count(&absent["counts"], "cascaded"), 0);

            let jobs = workspace["job_runs"]["by_job"].as_array().unwrap();
            for field in ["total", "succeeded", "failed", "cascaded"] {
                let summed = jobs
                    .iter()
                    .map(|job| count(&job["counts"], field))
                    .sum::<u64>();
                assert_eq!(
                    summed,
                    count(&workspace["job_runs"]["counts"], field),
                    "by_job {field} matches the workspace"
                );
            }
            let over_time = workspace["job_runs"]["over_time"].as_array().unwrap();
            for field in ["succeeded", "failed", "cascaded"] {
                let summed = over_time
                    .iter()
                    .map(|bucket| count(&bucket["counts"], field))
                    .sum::<u64>();
                assert_eq!(
                    summed,
                    count(&workspace["job_runs"]["counts"], field),
                    "over_time {field} matches the workspace"
                );
            }
            let bucket = |at: DateTime<Utc>| {
                over_time
                    .iter()
                    .find(|bucket| {
                        let start = bucket["bucket_start"]
                            .as_str()
                            .unwrap()
                            .parse::<DateTime<Utc>>()
                            .unwrap();
                        let end = bucket["bucket_end"]
                            .as_str()
                            .unwrap()
                            .parse::<DateTime<Utc>>()
                            .unwrap();
                        start <= at && at < end
                    })
                    .unwrap_or_else(|| panic!("no bucket holds {at}"))
            };
            let leaf_bucket = bucket(leaf_at);
            let echo_bucket = bucket(echo_at);
            assert_ne!(
                leaf_bucket["bucket_start"], echo_bucket["bucket_start"],
                "the fixture must split the leaf from its echoes"
            );
            assert_eq!(count(&leaf_bucket["counts"], "failed"), 1);
            assert_eq!(count(&leaf_bucket["counts"], "cascaded"), 0);
            assert_eq!(count(&echo_bucket["counts"], "failed"), 1);
            assert_eq!(count(&echo_bucket["counts"], "cascaded"), 2);
            assert_eq!(workspace["job_runs"]["observed_states"]["failed"], 4);
        },
    );
}
