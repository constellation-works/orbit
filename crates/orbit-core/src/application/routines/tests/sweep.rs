use crate::application::routines::clock::{ClockSettings, save_clock_settings};
use crate::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use crate::application::routines::sweep::{
    SweepOptions, configured_sweep_options, refresh_discovered_token_scoreboards,
    run_sweep_at_with_providers,
};
use crate::application::routines::validation::{
    RoutineHostIdentity, RoutinePlacementProjection, RoutinePlacementProvider,
};
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::InvocationInsertParams;
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use std::path::Path;
struct MustNotLoad;

impl RoutinePlacementProvider for MustNotLoad {
    fn load_routine_placement(&self) -> Result<RoutinePlacementProjection, OrbitError> {
        panic!("placement provider ran before the busy sweep lock returned")
    }
}

impl RoutineWorkspaceProvider for MustNotLoad {
    fn discover_workspaces(&self, _global_root: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        panic!("workspace provider ran before the busy sweep lock returned")
    }
}

#[test]
fn busy_lock_returns_before_remote_providers_are_loaded() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let state = global.join("state");
    let _held = orbit_store::try_acquire_routine_sweep_lock(&state)
        .expect("lock")
        .expect("first lock");

    let outcome = run_sweep_at_with_providers(
        &global,
        SweepOptions::default(),
        RoutineHostIdentity {
            machine_id: "hm_local".to_string(),
            host_id: "local".to_string(),
        },
        &MustNotLoad,
        &MustNotLoad,
    )
    .expect("busy outcome");

    assert!(outcome.lock_busy);
    assert_eq!(outcome.machine_id, "hm_local");
    assert_eq!(outcome.host_id, "local");
}

#[test]
fn production_sweep_options_follow_the_host_clock_cadence() {
    let root = tempfile::tempdir().expect("root");

    let default_options = configured_sweep_options(root.path(), SweepOptions::default())
        .expect("default clock settings");
    assert_eq!(default_options.sweep_cadence_seconds, 60);

    save_clock_settings(
        root.path(),
        ClockSettings {
            cadence_seconds: 300,
        },
    )
    .expect("configured clock settings");
    let configured_options = configured_sweep_options(root.path(), SweepOptions::default())
        .expect("configured clock settings");
    assert_eq!(configured_options.sweep_cadence_seconds, 300);
}

#[test]
fn sweep_refreshes_token_scoreboard_for_each_discovered_workspace() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    let runtime = crate::OrbitRuntime::from_roots(&global, &workspace_root).expect("runtime");

    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "jrun-scoreboard".to_string(),
            activity_id: "implement".to_string(),
            agent: "codex".to_string(),
            model: Some("gpt-test".to_string()),
            task_ids: Vec::new(),
            trace: InvocationTrace {
                usage: TokenUsage {
                    input: 10,
                    ..TokenUsage::default()
                },
                ..InvocationTrace::default()
            },
        })
        .expect("persist invocation");

    refresh_discovered_token_scoreboards(&[(
        Workspace {
            id: "ws-scoreboard".to_string(),
            name: "scoreboard".to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "agent-main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        runtime,
    )]);

    let tokens = std::fs::read_to_string(workspace_root.join("state/scoreboard/tokens.json"))
        .expect("scoreboard refreshed during sweep");
    assert!(tokens.contains("codex"), "{tokens}");
}
