use std::fs;

use chrono::Utc;
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
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
    let now = Utc::now();
    let run = JobRun {
        run_id: id.into(),
        job_id: "reliability_fixture".into(),
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
