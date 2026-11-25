//! A peer scan cut short by the tick deadline must defer scoped routines.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use orbit_core::application::routines::{
    RoutineMachineIdentity, SweepOptions, run_sweep_at_with_providers,
};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::workspace::{Workspace, WorkspaceStatus};

struct SlowPeerScan {
    runtime: OrbitRuntime,
    seen_deadline: Cell<Option<Instant>>,
    seen_routine_names: Cell<Option<BTreeSet<String>>>,
}

impl RoutineWorkspaceProvider for SlowPeerScan {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let workspace = Workspace {
            id: self.runtime.workspace_id()?,
            name: "horizon".into(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces {
            entries: vec![(workspace, self.runtime.clone())],
            ..DiscoveredWorkspaces::default()
        })
    }

    fn discover_name_collision_peers(
        &self,
        _: &Path,
        _: &DiscoveredWorkspaces,
        routine_names: &BTreeSet<String>,
        deadline: Option<Instant>,
    ) -> Result<DiscoveredWorkspaces, OrbitError> {
        // Fault injection: the peer scan returns only after the tick budget,
        // like a slow registry opening candidate checkouts.
        self.seen_deadline.set(deadline);
        self.seen_routine_names.set(Some(routine_names.clone()));
        let deadline = deadline.expect("a tick bounds its peer scan");
        std::thread::sleep(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20),
        );
        Ok(DiscoveredWorkspaces::default())
    }
}

/// [ORB-28162] A peer scan past the public sweep deadline defers its routines.
#[test]
fn a_peer_collision_scan_past_the_deadline_defers_the_workspace() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("create isolated roots");
    let global = root.path().join("global");
    let orbit = root.path().join("repo").join(".orbit");
    std::fs::create_dir_all(&orbit).expect("create workspace root");
    let jobs = global.join("resources/jobs");
    std::fs::create_dir_all(&jobs).expect("create jobs directory");
    std::fs::write(
        jobs.join("fixture_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    )
    .expect("write fixture job");
    std::fs::create_dir_all(orbit.join("routines")).expect("create routines directory");
    std::fs::write(
        orbit.join("routines/every-minute.yaml"),
        "schemaVersion: 1\nname: every-minute\nenabled: true\ntrigger:\n  cron: '* * * * *'\n  missed_run: skip\ntarget: job:fixture_pipeline\npolicy:\n  overlap: allow\n  timeout_minutes: 30\n",
    )
    .expect("write routine");
    let runtime = OrbitRuntime::from_roots(&global, &orbit).expect("build test runtime");
    let provider = SlowPeerScan {
        runtime,
        seen_deadline: Cell::new(None),
        seen_routine_names: Cell::new(None),
    };
    let deadline = Instant::now() + Duration::from_secs(3);

    let outcome = run_sweep_at_with_providers(
        &global,
        SweepOptions {
            deadline: Some(deadline),
            dry_run: true,
            ..SweepOptions::default()
        },
        RoutineMachineIdentity {
            machine_id: "fixture".into(),
            machine_name: "fixture".into(),
        },
        &provider,
    )
    .expect("run scoped sweep");

    assert_eq!(provider.seen_deadline.get(), Some(deadline));
    assert_eq!(
        provider.seen_routine_names.take(),
        Some(BTreeSet::from(["every-minute".to_string()]))
    );
    assert_eq!(outcome.skipped_workspaces, ["horizon"]);
    assert!(outcome.reports.is_empty(), "{:?}", outcome.reports);
}
