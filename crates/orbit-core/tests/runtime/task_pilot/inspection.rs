//! Status polling reads cached refs without fetching; the real clock sweep
//! still refreshes them. Git's process trace records actual fetch children.

use std::time::Instant;

use orbit_core::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use orbit_core::application::routines::{
    RoutineMachineIdentity, SweepOptions, routine_statuses_with_providers,
};
use orbit_types::workspace::{Workspace as WorkspaceRecord, WorkspaceStatus};

use super::*;

struct SingleWorkspace(OrbitRuntime);

impl RoutineWorkspaceProvider for SingleWorkspace {
    fn discover_workspaces(
        &self,
        _: &Path,
    ) -> Result<DiscoveredWorkspaces, orbit_core::OrbitError> {
        Ok(DiscoveredWorkspaces {
            entries: vec![(
                WorkspaceRecord {
                    id: self.0.workspace_id()?,
                    name: "inspection-fixture".into(),
                    owner_machine_id: None,
                    git_remote: None,
                    ship_mode: None,
                    base_branch: "main".into(),
                    status: WorkspaceStatus::Active,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                self.0.clone(),
            )],
            ..DiscoveredWorkspaces::default()
        })
    }
}

fn machine() -> RoutineMachineIdentity {
    RoutineMachineIdentity {
        machine_id: "fixture-machine".into(),
        machine_name: "fixture-host".into(),
    }
}

fn automation(workspace: &Workspace, now: chrono::DateTime<Utc>) -> Value {
    let report = routine_statuses_with_providers(
        &workspace.runtime.global_root(),
        machine(),
        &SingleWorkspace(workspace.runtime.clone()),
        now,
    )
    .unwrap();
    assert!(report.load_errors.is_empty(), "{:?}", report.load_errors);
    assert_eq!(report.statuses.len(), 1);
    report.statuses[0].automation.clone().unwrap()
}

fn fetches(trace: &Path) -> usize {
    std::fs::read_to_string(trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"] == "start" && event["argv"][1] == "fetch")
        .count()
}

#[test]
fn routine_status_reads_cached_refs_while_clock_sweep_fetches() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::inspection::routine_status_reads_cached_refs_while_clock_sweep_fetches",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("inspection candidate");
    let routines = workspace.runtime.shared_root().join("routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(
        routines.join("fixture-pilot.yaml"),
        serde_yaml::to_string(&pilot_routine()).unwrap(),
    )
    .unwrap();
    let remote = workspace.root.path().join("origin.git");
    workspace.git(&["clone", "--bare", ".", remote.to_str().unwrap()]);
    workspace.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    workspace.git(&["fetch", "origin", "main"]);
    let head = workspace.object("refs/remotes/origin/main");
    let now = Utc::now();

    let trace = workspace
        .root
        .path()
        .canonicalize()
        .unwrap()
        .join("git-trace.jsonl");
    let _env =
        orbit_common::test_env::scoped([("GIT_TRACE2_EVENT", Some(trace.to_str().unwrap()))]);
    let mut samples = Vec::new();
    let mut cached = None;
    for _ in 0..5 {
        let started = Instant::now();
        let status = automation(&workspace, now);
        samples.push(started.elapsed().as_secs_f64());
        if let Some(expected) = &cached {
            assert_eq!(
                &status, expected,
                "polling preserves every automation field"
            );
        } else {
            cached = Some(status);
        }
    }
    let cached = cached.unwrap();
    let inspection_fetches = fetches(&trace);
    assert_eq!(cached["reason"], "debouncing");
    assert_eq!(
        cached["state"]["members"]["pending"][&task.id]["source"]["commit"],
        head
    );

    // Preparation has always used the local branch when origin cannot be
    // resolved. A never-fetched ref retains that status, without creating it.
    workspace.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    let before_missing = fetches(&trace);
    let missing = automation(&workspace, now);
    let missing_fetches = fetches(&trace) - before_missing;
    assert_eq!(
        missing, cached,
        "an unfetched pilot retains its local-source status"
    );

    let before_tick = fetches(&trace);
    orbit_core::test_support::run_sweep_at(
        &workspace.runtime.global_root(),
        SweepOptions::default(),
        machine(),
        &SingleWorkspace(workspace.runtime.clone()),
        now,
    )
    .unwrap();
    let tick_fetches = fetches(&trace) - before_tick;
    assert_eq!(workspace.object("refs/remotes/origin/main"), head);
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    tracing::info!(
        evidence = %json!({
            "samples_seconds": samples, "p50_seconds": sorted[2],
            "inspection_fetches": inspection_fetches, "missing_ref_fetches": missing_fetches,
            "tick_fetches": tick_fetches, "automation": cached,
        }),
        "routine inspection evidence"
    );
    assert_eq!(
        inspection_fetches, 0,
        "status polling must spawn no git fetch children"
    );
    assert_eq!(
        missing_fetches, 0,
        "an unfetched ref must not make inspection fetch"
    );
    assert!(
        tick_fetches > 0,
        "the non-dry-run clock sweep must still fetch"
    );
}

#[test]
fn inspection_uses_cached_origin_and_preserves_unfetched_source_reasons() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::inspection::inspection_uses_cached_origin_and_preserves_unfetched_source_reasons",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("cached source candidate");
    let local = workspace.object("HEAD");
    workspace.commit_file("README.md", "remote material\n", "advance origin");
    let origin = workspace.object("HEAD");
    let remote = workspace.root.path().join("origin.git");
    workspace.git(&["clone", "--bare", ".", remote.to_str().unwrap()]);
    workspace.git(&["reset", "--hard", &local]);
    workspace.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    workspace.git(&["update-ref", "refs/remotes/origin/main", &origin]);
    let now = Utc::now();
    let inspect = |definition: &RoutineDefinition| {
        orbit_core::application::automation::inspect_routine(&workspace.runtime, definition, now)
            .unwrap()
    };
    let source = |diagnostic: orbit_types::workflow::automation::AutomationDiagnostic| {
        diagnostic.state.unwrap().members.unwrap().pending[&task.id]
            .source
            .commit
            .clone()
    };
    assert_eq!(source(inspect(&pilot_routine())), origin);
    workspace.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    let unfetched = inspect(&pilot_routine());
    assert_eq!(unfetched.reason, "debouncing");
    assert_eq!(source(unfetched), local);
    let delivery: RoutineDefinition = serde_json::from_value(json!({
        "schemaVersion": 1, "name": "fixture-delivery", "enabled": true,
        "target": "job:task_pilot_pipeline",
        "trigger": {"deliveries_landed": {
            "owner_machine": "fixture-machine", "branch": "main", "threshold": 1,
            "max_wait_minutes": 10, "coverage": "landed_code_review_v1",
        }},
    }))
    .unwrap();
    assert_eq!(inspect(&delivery).reason, "awaiting_baseline");
    assert!(
        workspace
            .git(&["for-each-ref", "refs/remotes/origin/main"])
            .is_empty(),
        "inspection must leave a never-fetched ref absent"
    );
}
