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

/// Inside the configured horizon the tick skips every routine and mints
/// nothing. Past it, the first observation baselines and the next due minute
/// fires. The horizon here is 30 minutes, inside the 180 default, so a
/// hardcoded default would still hold the far side.
#[test]
fn a_shutdown_inside_the_hold_horizon_skips_the_sweep_and_one_beyond_it_fires() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &a_shutdown_inside_the_hold_horizon_skips_the_sweep_and_one_beyond_it_fires,
    )) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let orbit = repo.join(".orbit");
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nhost_shutdown_hold_minutes = 30\n",
    )
    .unwrap();
    let init = crate::git_test_support::command()
        .args(["init", "--initial-branch=main", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(init.success(), "git init");
    let runtime = OrbitRuntime::from_roots(&global, &orbit)
        .unwrap()
        .with_automation_machine_identity(Some("horizon-host".into()));
    let resources = runtime.paths().global_dir.join("resources/jobs");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::write(
        resources.join("fixture_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    )
    .unwrap();
    let routines = runtime.shared_root().join("routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(
        routines.join("every-minute.yaml"),
        "schemaVersion: 1\nname: every-minute\nenabled: true\ntrigger:\n  cron: '* * * * *'\n  \
         missed_run: skip\ntarget: job:fixture_pipeline\npolicy:\n  overlap: allow\n  \
         timeout_minutes: 30\n",
    )
    .unwrap();
    let auto_tasks = runtime.paths().local_dir.join("auto_tasks");
    std::fs::create_dir_all(&auto_tasks).unwrap();
    std::fs::write(
        auto_tasks.join("every-minute-duty.yaml"),
        "schemaVersion: 1\nname: every-minute-duty\nenabled: true\nschedule:\n  every_minutes: 1\n\
         dedupe: always\ntemplate:\n  title: Fixture duty\n  description: A recurring fixture \
         duty.\n  acceptance_criteria:\n  - The duty is done.\n",
    )
    .unwrap();
    crate::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);

    let now = chrono::Utc::now();
    let tick = |shutdown_in: i64, at: chrono::DateTime<chrono::Utc>| {
        let shutdown = crate::runtime::host_signal::ScheduledShutdown {
            mode: "reboot".into(),
            scheduled_at: now + chrono::Duration::minutes(shutdown_in),
            source: "fixture".into(),
        };
        run_sweep_at_with_providers_at(
            &global,
            SweepOptions::default(),
            RoutineMachineIdentity {
                machine_id: "horizon-host".into(),
                machine_name: "horizon-host".into(),
            },
            &OneWorkspace(runtime.clone()),
            &crate::runtime::host_signal::FixedHostSignals::scheduled(shutdown),
            at,
        )
        .expect("tick")
    };

    let held = tick(30, now);
    assert!(
        held.reports.iter().any(|report| {
            report.routine == "every-minute"
                && report.action == "skipped"
                && report
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.starts_with("host_shutdown_scheduled"))
        }),
        "{:?}",
        held.reports
    );
    assert!(
        held.auto_task_reports.is_empty(),
        "{:?}",
        held.auto_task_reports
    );
    assert!(held.load_errors.is_empty(), "{:?}", held.load_errors);

    let baselined = tick(32, now);
    assert!(
        baselined
            .reports
            .iter()
            .any(|report| { report.routine == "every-minute" && report.action == "baselined" }),
        "the first observation past the horizon baselines: {:?}",
        baselined.reports
    );
    assert!(
        baselined
            .auto_task_reports
            .iter()
            .any(|report| report.name == "every-minute-duty" && report.action == "baselined"),
        "{:?}",
        baselined.auto_task_reports
    );

    let fired = tick(32, now + chrono::Duration::minutes(1));
    assert!(
        fired
            .reports
            .iter()
            .any(|report| { report.routine == "every-minute" && report.action == "fired" }),
        "the next due minute fires: {:?}",
        fired.reports
    );
    assert!(
        fired
            .auto_task_reports
            .iter()
            .any(|report| { report.name == "every-minute-duty" && report.action == "minted" }),
        "{:?}",
        fired.auto_task_reports
    );
}

struct OneWorkspace(OrbitRuntime);

impl RoutineWorkspaceProvider for OneWorkspace {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let workspace = Workspace {
            id: self.0.workspace_id().expect("workspace id"),
            name: "horizon".into(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        Ok(DiscoveredWorkspaces {
            entries: vec![(workspace, self.0.clone())],
            ..DiscoveredWorkspaces::default()
        })
    }
}
