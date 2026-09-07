use crate::application::routines::clock::{ClockSettings, save_clock_settings};
use crate::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use crate::application::routines::sweep::{
    SweepOptions, configured_sweep_options, run_sweep_at_with_providers,
};
use crate::application::routines::validation::{
    RoutineHostIdentity, RoutinePlacementProjection, RoutinePlacementProvider,
};
use orbit_common::OrbitError;
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
