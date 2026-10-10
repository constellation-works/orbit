//! A workspace that uses up the tick budget cannot renew it for later phases.

use std::path::Path;
use std::time::{Duration, Instant};

use super::super::RoutineMachineIdentity;
use super::super::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use super::super::sweep::{SweepOptions, run_sweep_at_with_providers_at};
use crate::{OrbitError, OrbitRuntime};
use orbit_types::workspace::{Workspace, WorkspaceStatus};

struct BlockedWorkspace(Vec<(Workspace, OrbitRuntime)>);

impl RoutineWorkspaceProvider for BlockedWorkspace {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        unreachable!("tick discovery must receive the shared deadline")
    }

    fn discover_workspaces_until(
        &self,
        _: &Path,
        deadline: Instant,
    ) -> Result<DiscoveredWorkspaces, OrbitError> {
        // Fault injection: the first workspace's synchronous open returns
        // just after its budget, like a busy store yielding at its boundary.
        std::thread::sleep(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20),
        );
        Ok(DiscoveredWorkspaces {
            entries: self.0.clone(),
            ..DiscoveredWorkspaces::default()
        })
    }
}

#[test]
fn blocked_workspace_finishes_then_the_tick_reports_all_deferred_workspaces() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &blocked_workspace_finishes_then_the_tick_reports_all_deferred_workspaces,
    )) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let mut workspaces = Vec::new();
    for name in ["blocked", "following"] {
        let orbit = root.path().join(name).join(".orbit");
        std::fs::create_dir_all(&orbit).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
        let workspace = Workspace {
            id: runtime.workspace_id().unwrap(),
            name: name.into(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        workspaces.push((workspace, runtime));
    }
    let started = Instant::now();
    let outcome = run_sweep_at_with_providers_at(
        &global,
        SweepOptions {
            deadline: Some(started + Duration::from_millis(100)),
            ..SweepOptions::default()
        },
        RoutineMachineIdentity {
            machine_id: "fixture".into(),
            machine_name: "fixture".into(),
        },
        &BlockedWorkspace(workspaces),
        &crate::runtime::host_signal::FixedHostSignals::none(),
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(
        outcome.deadline_exceeded,
        "an exhausted tick must fail, even if no routine fired"
    );
    assert_eq!(outcome.skipped_workspaces, ["blocked", "following"]);
    assert_eq!(outcome.auto_task_reports.len(), 2);
    assert!(
        outcome
            .auto_task_reports
            .iter()
            .all(|row| row.action == "skipped" && row.reason.as_deref() == Some("tick_deadline"))
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "later work must not start a new deadline"
    );
}
